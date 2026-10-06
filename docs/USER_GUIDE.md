# Q-pid — user guide

Q-pid is a small Windows/Linux audio player: a folder/file playlist player with a
seek bar, ±15 s skips, five playback speeds, and a session that remembers
where you stopped. This guide covers everything a user can do — in the app
and in the CLI — with the source line behind each behavior.

---

## 1. Three ways to launch

| Launch | Behavior |
|---|---|
| Double-click `qpid.exe` (no arguments) | Restores your last session: folder, track, position, speed — **paused, no sound until you press Play** (`src/main.rs:188-191`, `src/engine/mod.rs:797-856`). Nothing in history → empty window. |
| `qpid.exe <file-or-folder>` (or Explorer "Open with") | Opens and **plays immediately** (`src/main.rs:186-187`, autoplay at `src/engine/mod.rs:756-764`). Re-opening a file you only half-finished resumes where you stopped — if the file hasn't changed size since it was saved (`src/store.rs:141`). |
| `qpid.exe --cli <file-or-folder>` | Headless mode: no window, everything driven from the keyboard stdin (see §6). Meant for testing/benchmarks (`src/main.rs:29-33`). |

On every exit — close button, `q`, or stdin end — the current position is
saved before the process dies (`src/main.rs:156-157` CLI, `src/main.rs:196-197`
UI, `src/engine/mod.rs:459-466`).

**One copy at a time:** *Opening a second copy hands the file to the
running window* — q-pid holds a machine-wide lock (`Global\qpid-instance`,
`src/platform/windows.rs:87`), so the second copy never opens its own window: it
sends the path over a named pipe (or just asks the first copy to come to
the front) and exits in a few milliseconds (`src/main.rs:163-166`).
**`--cli` runs are exempt** — they never claim the instance, so a CLI
session can run alongside the UI (`src/main.rs:161-163`).

---

## 2. The window, top to bottom

Dark theme only (`ui/main.slint:4-6`); native OS title bar.

### Open file / Open folder (top-left)

- **Open file** (`ui/main.slint:220-235`) — native file dialog filtered to
  **mp3, m4a, m4b, aac, flac, ogg, wav** (`src/ui.rs:170`). Picking a file
  plays it (with its saved position).
- **Open folder** (`ui/main.slint:237-252`) — scans the folder for playable
  files. Empty folder → green status **"No audio files in folder"**
  (`src/engine/mod.rs:686`). Otherwise it picks the start file by rule:
  **last file if you hadn't finished it → first unfinished → first file**
  (`src/engine/mod.rs:655-664`).
- **Help** — opens an in-app overlay listing every keyboard shortcut and
  button with what it does. Click anywhere (or press `Esc`) to close; while
  it's open the other keys are ignored.

### Folder line

The full folder path of the current track, muted small text, truncated if too
long (`ui/main.slint:273-279`).

### Track area

- Nothing loaded yet: **"Drop a folder or file here, or press Open"**
  (`ui/main.slint:283`). **Drop a file or folder anywhere on the window to
  open it** — drag-and-drop runs through the winit hook
  (`src/winit_hook.rs:99`, sink registered at `src/ui.rs:194`). A drop
  while another file plays behaves like Open file: the current file's
  position is saved first, then the dropped one plays.
- Track loaded: **title** (filename without extension — no tags or cover
  art, `src/engine/mod.rs:1027`) plus **`N / M`** position in the playlist
  (`ui/main.slint:291-307`).

### Seek row: elapsed — slider — duration

- Times use **`m:ss`** under an hour (`1:15`) and **`h:mm:ss`** from an hour
  up (`1:00:00`) (`src/ui.rs:25-33`).
- Duration shows blank when the file's duration isn't known yet
  (`src/ui.rs:135`).
- The slider itself is described in §4.

### Transport row

Centered: `|<` · `-15` · **play/pause** · `+15` · `>|` (`ui/main.slint:335-350`).

| Button | Key | What it does |
|---|---|---|
| `\|<` Previous | `P` | **More than 5 s in → restarts the current track from 0**; otherwise jumps to the previous track; on the first track → restarts it (`src/engine/mod.rs:545-571`). |
| `-15` | `←` | Back 15 s, never below 0 (`src/engine/mod.rs:530-534`). |
| Play / Pause (big green circle) | `Space` | Playing ⇄ paused. On a finished track (Ended) it **restarts from 0** (`src/engine/mod.rs:513-527`). With nothing loaded, nothing happens. |
| `+15` | `→` | Forward 15 s, clamped to **duration − 1 s** — you can't seek into the last second (`src/engine/mod.rs:617-622`). |
| `>\|` Next | `N` | Next track; on the **last** track it does nothing (`src/engine/mod.rs:540-544`). |

All transport buttons are always clickable; the engine safely ignores them
when there's no track.

### Speed row

Five segments: **0.5x, 1x, 1.25x, 1.5x, 2x** (`ui/main.slint:358-362`).
The active one is a green pill — that's the whole "display"; there is no
separate readout. Clicking a segment:

- changes speed **immediately at the current position** (a flush — no jump,
  audible within ~100 ms, `src/engine/mod.rs:577-603`),
- is **remembered** across sessions (`src/engine/mod.rs:422, 850`),
- keyboard: `[` steps down one segment, `]` steps up, clamped at both ends
  (`src/ui.rs:46-48`).

⚠️ Known quirk: after a session restore the audio may already play at your
saved speed while the row still shows the default 1x — there's no speed
event to sync it. **Click any segment to resync** (`IMPLEMENTATION.md:88-90`).

### Status line (bottom, green)

Shows messages like "Skipping unreadable file: …", "No playable files left",
"Could not open file: …", "Output device error: …", "Output device changed —
press Play to resume" (`src/engine/mod.rs:779, 789, 1112, 1131, 1195`). It is
**never cleared once set** — the last message stays until the window closes.

---

## 3. Keyboard shortcuts

One handler for the whole window (`ui/main.slint:170-208`):

| Key | Action |
|---|---|
| `Space` | Play / pause |
| `←` / `→` | Seek −15 s / +15 s |
| `N` | Next track |
| `P` | Previous track (with the >5 s restart rule) |
| `[` / `]` | Speed down / up one segment |
| `Ctrl+O` | Open file dialog |
| `Ctrl+Shift+O` | Open folder dialog |
| `Esc` | Close the Help overlay (all keys are ignored while Help is open) |

Deliberate non-actions:

- **Plain `O` does nothing** — the chord is checked with the Ctrl modifier
  required (`ui/main.slint:189`).
- **Other Ctrl/Alt combos do nothing** (`Ctrl+N`, `Ctrl+Space`, `Alt+←`…):
  chords are evaluated first and rejected before the plain-letter arms
  (`ui/main.slint:189-199`).
- Caveat: `Shift+N` still triggers Next (the key text becomes `"N"` and the
  match is case-insensitive, `ui/main.slint:203`).

### Media keys

**Media keys: play/pause, next, previous** work while Q-pid runs on
Windows — even
when the window is minimized or unfocused (`RegisterHotKey` on a
message-only window, `src/platform/windows.rs:248-326`). They trigger the same actions
as the transport buttons; if your keyboard/headset has those keys, they
work system-wide while q-pid is open.

---

## 4. The seek slider in detail

Custom slider (`ui/main.slint:69-125`), because the stock Slint slider
can't expose drag state:

- **Press and drag** — the green bar follows your pointer (`dragging`
  state); the 500 ms engine refresh is guarded so **the engine cannot yank
  the bar out from under your finger** (`src/ui.rs:155-158`). The elapsed
  time text keeps ticking during the drag.
- **Release** — one `commit` fires and the seek happens
  (`ui/main.slint:118-120`); the bar immediately jumps to your release point
  and doesn't snap back while the engine catches up (`src/ui.rs:237-253`).
- **Plain click** (press+release in place) = seek to that point — not a no-op.
- **Duration unknown** → the slider is **dimmed to 40% and completely inert**
  (all pointer input blocked, `ui/main.slint:82, 95`) while the elapsed
  clock still shows.
- Right/middle mouse buttons are ignored (`ui/main.slint:100-102`).
- Targets are clamped to duration − 1 s (`src/engine/mod.rs:617-622`).

---

## 5. Behaviors you'll notice over time

- **Pause longer than 10 s** → Q-pid releases the audio stream, decoder and
  resampler to save resources (**0 % CPU** while parked, BENCH.md,
  "paused-over-10s").
  Pressing Play reopens the file **at the exact position you paused**
  (`src/engine/mod.rs:1248-1253`). Seeking while paused just moves the saved
  resume position.
- **End of a mid-playlist track** → auto-advances to the next file with
  essentially no gap (<100 ms, `src/engine/mod.rs:369-397`).
- **End of the last track** → the play button flips back to `>`; after 10 s
  the stream is released like a long pause; pressing **Play restarts the
  file from 0:00** (`src/engine/mod.rs:388-396, 1144-1169`).
- **Autosaves** happen on: pause, next, previous, open, speed change, exit,
  error, and every 30 s while playing (`IMPLEMENTATION.md:62-73`). Worst-case
  position loss after a crash is ~30 s.
- **Minimized or covered window** → the on-screen clock and slider **freeze**
  (the UI timer stops, `src/ui.rs:104-120`) while audio keeps playing;
  restoring the window resumes updates. Same freeze while paused.
- **Session restore detail**: if the file changed size since the save
  (e.g. re-downloaded), the saved position is discarded and you start at 0
  (`src/store.rs:137-144`).
- Unreadable files are skipped with a status message; if nothing playable
  remains you get **"No playable files left"** (`src/engine/mod.rs:1111-1133`).

---

## 6. The CLI (`qpid.exe --cli`)

Headless mode: no window; engine events are printed as `[event] …` lines
(`src/main.rs:71`). Type a command per line on stdin:

| Input | Action |
|---|---|
| `p` | Play / pause toggle |
| `f` | Forward 15 s |
| `b` | Back 15 s |
| `n` | Next track |
| `P` | Previous track (capital P) |
| `s` | Cycle speed: 1 → 1.25 → 1.5 → 2 → 0.5 → 1 (prints `speed -> 2x` etc.) |
| `w` | Wake counter: `wakes=N uptime_s=T wakes_per_s=R underruns=U` (engine heartbeat, background ≈0.5/s) |
| `d` | Simulate an output-device error: pauses, releases the stream, posts "Output device changed — press Play to resume" (dev/probe hook, `src/main.rs:140-145`) |
| *(Enter, empty line)* | Print current `position_ms` |
| `q` | Quit (saves state on the way out) |
| anything else | `unknown command: …` on stderr |

Without a path argument, `--cli` restores the previous session the same way
the app does (`src/main.rs:75-82`).

---

## 7. Known quirks / limitations

1. **Speed row desync after restore** — shows 1x while audio may run at the
   saved speed; click a segment to resync.
2. **Filename-only track titles** — no ID3 tags, no cover art.
3. **Status line never clears** — last green message persists.
4. **No seek into the final second** (clamp at duration − 1 s).
5. **Light/dark theme switch** not available yet (dark only).
6. `Shift+N` triggers Next (case-insensitive key match).

---

## 8. Linux

Q-pid builds and runs on Linux; packages ship on the [v1.1.0
release](https://github.com/KajeJeka/qpid/releases/tag/v1.1.0), the first
release to carry Linux assets (the `v1.0.0` tag predates the port).

### Install

| Format | How |
|---|---|
| AppImage | `chmod +x qpid-<tag>-x86_64.AppImage` (or `-aarch64`), then `./qpid-<tag>-x86_64.AppImage` |
| Flatpak | `flatpak install --user qpid-<tag>.flatpak` — a self-hosted bundle from the release page, **not** Flathub |
| AUR | download the release's `aur` artifact, then `makepkg -si` (full procedure: `packaging/aur/README.md`) |

### First-release limitations

1. **Single instance does not coordinate across formats.** The lock is a
   Unix socket in `$XDG_RUNTIME_DIR`: an AppImage copy and an AUR copy
   share that path and coordinate as one instance, while a Flatpak copy
   runs sandboxed with its own restricted view of the runtime dir, so it
   runs as its own instance.
2. **Window raise on Wayland is best-effort.** Compositors refuse
   force-focus by design; the second instance still delivers the path
   (`OpenPath` reaches the first copy), the window just may not come to
   the front.
3. **Flatpak session-restore needs the file inside `$HOME`.** The bundle
   is granted `--filesystem=home:ro`; audio elsewhere on disk is invisible
   to the sandbox, so it cannot be reopened after a restart.
4. **Media keys / MPRIS are not supported on Linux yet** (MPRIS over
   D-Bus is a future phase). Keyboard shortcuts work exactly as in §3.
5. **State lives at `~/.local/state/qpid/state.json`** (`$XDG_STATE_HOME`
   when set), not `%LOCALAPPDATA%`. Name keys are lowercased
   unconditionally on Linux too — two files whose names differ only by
   case share a state entry (rare on case-sensitive filesystems).
