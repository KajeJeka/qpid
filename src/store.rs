//! JSON state persistence (architecture.md section 9): schema, atomic
//! save, pruning, key normalization. Never crashes on bad state: a
//! missing, corrupt or wrong-version file loads as defaults.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 1;
#[allow(dead_code)] // used from a later Phase 3 task (rule 8)
pub const RESUME_REWIND_MS: u64 = 0; // section 9 rule 8
const MAX_FOLDERS: usize = 100;       // section 9 rule 3

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Store {
    pub version: u32,
    pub speed: f64,
    pub last_folder: String,
    pub folders: HashMap<String, Folder>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Folder {
    pub touched: u64,
    pub last_file: String,
    pub files: HashMap<String, FileEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileEntry {
    pub pos_ms: u64,
    pub size: u64,
    pub done: bool,
}

impl Default for Store {
    fn default() -> Self {
        Store { version: SCHEMA_VERSION, speed: 1.0, last_folder: String::new(), folders: HashMap::new() }
    }
}

/// Lowercase + strip the Windows long-path prefix (section 9 rule 1).
pub fn key(s: &str) -> String {
    let s = s.strip_prefix(r"\\?\").unwrap_or(s);
    s.to_lowercase()
}

pub fn speed_to_milli(s: f64) -> u32 {
    if !s.is_finite() || s <= 0.0 { return 1000; }
    (s * 1000.0).round() as u32
}

pub fn milli_to_speed(m: u32) -> f64 {
    m as f64 / 1000.0
}

/// `%LOCALAPPDATA%/qpid/state.json`; None when LOCALAPPDATA is unset
/// (persistence silently disabled, never crash).
pub fn state_path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("qpid").join("state.json"))
}

pub fn load() -> Store {
    match state_path() {
        Some(p) => load_from(&p),
        None => Store::default(),
    }
}

fn load_from(p: &Path) -> Store {
    let text = match std::fs::read_to_string(p) {
        Ok(t) => t,
        Err(_) => return Store::default(),
    };
    match serde_json::from_str::<Store>(&text) {
        Ok(s) if s.version == SCHEMA_VERSION => s,
        _ => Store::default(), // corrupt / wrong version: defaults (rule 4)
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Store {
    pub fn save(&mut self) {
        match state_path() {
            Some(p) => self.save_to(&p),
            None => {}
        }
    }

    fn save_to(&mut self, p: &Path) {
        self.prune();
        let text = match serde_json::to_string_pretty(self) {
            Ok(t) => t,
            Err(_) => return,
        };
        let dir = match p.parent() { Some(d) => d, None => return };
        if std::fs::create_dir_all(dir).is_err() { return; }
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_err() { return; }
        // std::fs::rename replaces an existing destination on Windows
        // (MoveFileExW + REPLACE_EXISTING): the rule 6 atomic write.
        let _ = std::fs::rename(&tmp, p);
    }

    fn prune(&mut self) {
        while self.folders.len() > MAX_FOLDERS {
            let oldest = self.folders.iter().min_by_key(|(_, f)| f.touched).map(|(k, _)| k.clone());
            match oldest {
                Some(k) => { self.folders.remove(&k); }
                None => break,
            }
        }
    }

    pub fn record(&mut self, folder: &str, file: &str, size: u64, pos_ms: u64, done: bool) {
        self.last_folder = folder.to_string();
        let fk = key(folder);
        let file_key = key(file);
        let touched = now_unix();
        let entry = self.folders.entry(fk.clone()).or_insert_with(|| Folder {
            touched,
            last_file: String::new(),
            files: HashMap::new(),
        });
        entry.touched = touched;
        entry.last_file = file_key.clone();
        if pos_ms > 0 || done {
            entry.files.insert(file_key, FileEntry { pos_ms, size, done });
        } else {
            entry.files.remove(&file_key); // rule 3: entries only if pos > 0 or done
        }
    }

    pub fn position_for(&self, folder: &str, file: &str, size: u64) -> u64 {
        self.folders
            .get(&key(folder))
            .and_then(|f| f.files.get(&key(file)))
            .filter(|e| e.size == size) // rule 2: size differs -> ignore position
            .map(|e| e.pos_ms)
            .unwrap_or(0)
    }

    pub fn is_done(&self, folder: &str, file: &str) -> bool {
        self.folders
            .get(&key(folder))
            .and_then(|f| f.files.get(&key(file)))
            .map(|e| e.done)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("qpid_store_test_{tag}_{}.json", std::process::id()))
    }

    #[test]
    fn roundtrip_and_corrupt_default() {
        let p = tmp_path("rt");
        let mut s = Store::default();
        s.record("D:\\Books", "Ch 1.mp3", 100, 5000, false);
        s.save_to(&p);
        let loaded = load_from(&p);
        assert_eq!(loaded.last_folder, "D:\\Books");
        assert!(loaded.folders[&key("d:\\books")].files[&key("ch 1.mp3")].pos_ms == 5000);
        std::fs::write(&p, "{ not json").unwrap();
        assert_eq!(load_from(&p).version, SCHEMA_VERSION); // rule 4: corrupt -> defaults
        assert!(load_from(Path::new("Z:\\no\\such\\file.json")).version == SCHEMA_VERSION);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn size_mismatch_and_entry_rules() {
        let mut s = Store::default();
        s.record("F", "a.mp3", 100, 0, false);   // pos 0, not done -> no entry (rule 3)
        assert!(s.position_for("F", "a.mp3", 100) == 0);
        s.record("F", "b.mp3", 100, 50, false);
        assert_eq!(s.position_for("F", "b.mp3", 999), 0);  // size differs -> ignore pos (rule 2)
        assert_eq!(s.position_for("F", "b.mp3", 100), 50);
        s.record("F", "c.mp3", 100, 99, true);   // done with pos -> stored
        assert!(s.is_done("F", "c.mp3"));
        assert_eq!(s.last_folder, "F");
    }

    #[test]
    fn key_and_speed_helpers() {
        assert_eq!(key(r"\\?\C:\A"), key("c:\\a"));
        assert_eq!(speed_to_milli(1.25), 1250);
        assert_eq!(speed_to_milli(f64::NAN), 1000);
        assert!((milli_to_speed(1500) - 1.5).abs() < 1e-9);
    }

    #[test]
    fn prune_keeps_100_newest() {
        let mut s = Store::default();
        for i in 0..105u64 {
            let f = format!("F{i}");
            s.folders.insert(f.clone(), Folder { touched: i, last_file: String::new(), files: HashMap::new() });
        }
        s.prune();
        assert_eq!(s.folders.len(), 100);
        assert!(!s.folders.contains_key("F0") && s.folders.contains_key("F104"));
    }
}
