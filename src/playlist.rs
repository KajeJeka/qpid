//! Folder scan for the playlist (architecture.md sections 12.6, 12.7,
//! 13): playable extension filter, hidden/system skip, natural sort.

use std::path::{Path, PathBuf};

const EXTS: [&str; 7] = ["mp3", "m4a", "m4b", "aac", "flac", "ogg", "wav"];
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;

/// Pure predicate so tests do not have to fake file attributes.
pub fn is_playable(name: &str, attributes: u32) -> bool {
    let ext_ok = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| EXTS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false);
    let hidden = attributes & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0;
    ext_ok && !hidden
}

/// All playable files in `dir` (no recursion, section 17), natural
/// sorted by file name so "2.mp3" precedes "10.mp3".
pub fn scan(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                if !p.is_file() { return false; }
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                is_playable(name, crate::platform::hidden_flags(p))
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort_by(|a, b| {
        natord::compare(
            &a.file_name().unwrap_or_default().to_string_lossy(),
            &b.file_name().unwrap_or_default().to_string_lossy(),
        )
    });
    out
}

pub fn index_of(files: &[PathBuf], path: &Path) -> Option<usize> {
    // CLI opens may use '/' while scan results use '\': normalize both sides.
    // Lowercase on all platforms too (linux-port.md section 6 ruling — same direction as store::key).
    let target = path.to_string_lossy().to_lowercase().replace('/', "\\");
    files.iter()
        .position(|p| p.to_string_lossy().to_lowercase().replace('/', "\\") == target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_playable_filters_exts_and_hidden_system() {
        assert!(is_playable("a.mp3", 0));
        assert!(is_playable("B.M4A", 0));           // case-insensitive ext
        assert!(!is_playable("a.txt", 0));
        assert!(!is_playable("a.mp3", 0x2));         // FILE_ATTRIBUTE_HIDDEN
        assert!(!is_playable("a.mp3", 0x4));         // FILE_ATTRIBUTE_SYSTEM
        assert!(is_playable("a.mp3", 0x20));         // ARCHIVE is fine
    }

    #[test]
    fn natural_sort_orders_1_to_30_numerically() {   // acceptance test 1
        let names: Vec<String> = (1..=30).map(|i| format!("{i}.mp3")).collect();
        let mut paths: Vec<std::path::PathBuf> =
            names.iter().map(|n| std::path::PathBuf::from(n)).collect();
        paths.sort_by(|a, b| {
            natord::compare(
                &a.file_name().unwrap_or_default().to_string_lossy(),
                &b.file_name().unwrap_or_default().to_string_lossy(),
            )
        });
        let sorted: Vec<String> = paths.iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(sorted.first().unwrap(), "1.mp3");
        assert_eq!(sorted.get(9).unwrap(), "10.mp3");
        assert_eq!(sorted.last().unwrap(), "30.mp3");
        // "2" must sort before "10" (section 12.7)
        let pos2 = sorted.iter().position(|n| n == "2.mp3").unwrap();
        let pos10 = sorted.iter().position(|n| n == "10.mp3").unwrap();
        assert!(pos2 < pos10);
    }

    #[test]
    fn index_of_is_case_insensitive() {
        let files = vec![
            std::path::PathBuf::from("C:\\Music\\a.mp3"),
            std::path::PathBuf::from("C:\\Music\\b.mp3"),
        ];
        assert_eq!(index_of(&files, std::path::Path::new("c:\\MUSIC\\B.MP3")), Some(1));
        assert_eq!(index_of(&files, std::path::Path::new("C:\\Music\\zz.mp3")), None);
        // Forward-slash candidate matches backslash entry and vice versa.
        let rel = vec![std::path::PathBuf::from("test_audio\\test.mp3")];
        assert_eq!(index_of(&rel, std::path::Path::new("test_audio/test.mp3")), Some(0));
        let fwd = vec![std::path::PathBuf::from("test_audio/test.mp3")];
        assert_eq!(index_of(&fwd, std::path::Path::new("test_audio\\test.mp3")), Some(0));
    }
}
