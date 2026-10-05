# Q-pid — user guide

Q-pid is a small Windows audio player: a folder/file playlist player with a
seek bar, ±15 s skips, five playback speeds, and a session that remembers
where you stopped. This guide covers everything a user can do — in the app
and in the CLI — with the source line behind each behavior.

---

## 1. Three ways to launch

| Launch | Behavior |
|---|---|
| Double-click `qpid.exe` (no arguments) | Restores your last session: folder, track, position, speed — **paused, no sound until you press Play** (`src/main.rs:161-164`, `src/engine/mod.rs:771-830`). Nothing in history → empty window. |
| `qpid.exe <file-or-folder>` (or Explorer "Open with") | Opens and **plays immediately** (`src/main.rs:159-160`, autoplay at `src/engine/mod.rs:738`). Re-opening a file you only half-finished resumes where you stopped — if the file hasn't changed size since it was saved (`src/store.rs:143`). |
| `qpid.exe --cli <file-or-folder>` | Headless mode: no window, everything driven from the keyboard stdin (see §6). Meant for testing/benchmarks (`src/main.rs:24-32`). |

On every exit — close button, `q`, or stdin end — the current position is
saved before the process dies (`src/main.rs:148-149`, `src/engine/mod.rs:433-441`).

**One copy at a time:** *Opening a second copy hands the file to the
running window* — q-pid holds a machine-wide lock (`Global\qpid-instance`,
`src/winapi.rs:78`), so the second copy never opens its own window: it
sends the path over a named pipe (or just asks the first copy to come to
the front) and exits in a few milliseconds (`src/main.rs:163-166`).

---

## 2. The window, top to bottom

Dark theme only (`ui/main.slint:4-6`); native OS title bar.

### Open file / Open folder (top-left)

- **Open file** (`ui/main.slint:208-223`) — native file dialog filtered to
  **mp3, m4a, m4b, aac, flac, ogg, wav** (`src/ui.rs:170`). Picking a file
  plays it (with its saved position).
- **Open folder** (`ui/main.slint:225-240`) — scans the folder for playable
  files. Empty folder → green status **"No audio files in folder"**
  (`src/engine/mod.rs:660`). Otherwise it picks the start file by rule:
  **last file if you hadn't finished it → first unfinished → first file**
  (`src/engine/mod.rs:681-688`).
- **Help** — opens an in-app overlay listing every keyboard shortcut and
  button with what it does. Click anywhere (or press `Esc`) to close; while
  it's open the other keys are ignored.

### Folder line

The full folder path of the current track, muted small text, truncated if too
long (`ui/main.slint:244-250`).

### Track area

- Nothing loaded yet: **"Drop a folder or file here, or press Open"**
  (`ui/main.slint:283`). **Drop a file or folder anywhere on the window to
  open it** — drag-and-drop runs through the winit hook
  (`src/winit_hook.rs:99`, sink registered at `src/ui.rs:194`). A drop
  while another file plays behaves like Open file: the current file's
  position is saved first, then the dropped one plays.
- Track loaded: **title** (filename without extension — no tags or cover
  art, `src/engine/mod.rs:1001`) plus **`N / M`** position in the playlist
  (`ui/main.slint:262-278`).

### Seek row: elapsed — slider — duration

- Times use **`m:ss`** under an hour (`1:15`) and **`h:mm:ss`** from an hour
  up (`1:00:00`) (`src/ui.rs:25-33`).
- Duration shows blank when the file's duration isn't known yet
  (`src/ui.rs:135`).
- The slider itself is described in §4.

### Transport row

Centered: `|<` · `-15` · **play/pause** · `+15` · `>|` (`ui/main.slint:306-321`).

| Button | Key | What it does |
|---|---|---|
| `\|<` Previous | `P` | **More than 5 s in → restarts the current track from 0**; otherwise jumps to the previous track; on the first track → restarts it (`src/engine/mod.rs:519-545`). |
| `-15` | `←` | Back 15 s, never below 0 (`src/engine/mod.rs:506`). |
| Play / Pause (big green circle) | `Space` | Playing ⇄ paused. On a finished track (Ended) it **restarts from 0** (`src/engine/mod.rs:496-499`). With nothing loaded, nothing happens. |
| `+15` | `→` | Forward 15 s, clamped to **duration − 1 s** — you can't seek into the last second (`src/engine/mod.rs:592-593`). |
| `>\|` Next | `N` | Next track; on the **last** track it does nothing (`src/engine/mod.rs:514-518`). |

All transport buttons are always clickable; the engine safely ignores them
when there's no track.

### Speed row

Five segments: **0.5x, 1x, 1.25x, 1.5x, 2x** (`ui/main.slint:329-333`).
The active one is a green pill — that's the whole "display"; there is no
separate readout. Clicking a segment:

- changes speed **immediately at the current position** (a flush — no jump,
  audible within ~100 ms, `src/engine/mod.rs:551-577`),
- is **remembered** across sessions (`src/engine/mod.rs:396`),
- keyboard: `[` steps down one segment, `]` steps up, clamped at both ends
  (`src/ui.rs:46-48`).

⚠️ Known quirk: after a session restore the audio may already play at your
saved speed while the row still shows the default 1x — there's no speed
event to sync it. **Click any segment to resync** (`IMPLEMENTATION.md:86-89`).

### Status line (bottom, green)

Shows messages like "Skipping unreadable file: …", "No playable files left",
"Could not open file: …", "Output device error: …" (`src/engine/mod.rs:660,
753, 763, 1086, 1105`). It is **never cleared once set** — the last message
stays until the window closes.

---

## 3. Keyboard shortcuts

One handler for the whole window (`ui/main.slint:166-196`):

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
  required (`ui/main.slint:177`).
- **Other Ctrl/Alt combos do nothing** (`Ctrl+N`, `Ctrl+Space`, `Alt+←`…):
  chords are evaluated first and rejected before the plain-letter arms
  (`ui/main.slint:185-187`).
- Caveat: `Shift+N` still triggers Next (the key text becomes `"N"` and the
  match is case-insensitive, `ui/main.slint:191`).

### Media keys

**Media keys: play/pause, next, previous** work while Q-pid runs — even
when the window is minimized or unfocused (`RegisterHotKey` on a
message-only window, `src/winapi.rs:244`). They trigger the same actions
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
  and doesn't snap back while the engine catches up (`src/ui.rs:237-245`).
- **Plain click** (press+release in place) = seek to that point — not a no-op.
- **Duration unknown** → the slider is **dimmed to 40% and completely inert**
  (all pointer input blocked, `ui/main.slint:82, 95`) while the elapsed
  clock still shows.
- Right/middle mouse buttons are ignored (`ui/main.slint:100-102`).
- Targets are clamped to duration − 1 s (`src/engine/mod.rs:592-593`).

---

## 5. Behaviors you'll notice over time

- **Pause longer than 10 s** → Q-pid releases the audio stream, decoder and
  resampler to save resources (**0 % CPU** while parked, `BENCH.md:242-246`).
  Pressing Play reopens the file **at the exact position you paused**
  (`src/engine/mod.rs:1184-1189`). Seeking while paused just moves the saved
  resume position.
- **End of a mid-playlist track** → auto-advances to the next file with
  essentially no gap (<100 ms, `src/engine/mod.rs:344-361`).
- **End of the last track** → the play button flips back to `>`; after 10 s
  the stream is released like a long pause; pressing **Play restarts the
  file from 0:00** (`src/engine/mod.rs:1113-1143`).
- **Autosaves** happen on: pause, next, previous, open, speed change, exit,
  error, and every 30 s while playing (`IMPLEMENTATION.md:62-71`). Worst-case
  position loss after a crash is ~30 s.
- **Minimized or covered window** → the on-screen clock and slider **freeze**
  (the UI timer stops, `src/ui.rs:104-120`) while audio keeps playing;
  restoring the window resumes updates. Same freeze while paused.
- **Session restore detail**: if the file changed size since the save
  (e.g. re-downloaded), the saved position is discarded and you start at 0
  (`src/store.rs:139-146`).
- Unreadable files are skipped with a status message; if nothing playable
  remains you get **"No playable files left"** (`src/engine/mod.rs:1085-1106`).

---

## 6. The CLI (`qpid.exe --cli`)

Headless mode: no window; engine events are printed as `[event] …` lines
(`src/main.rs:70`). Type a command per line on stdin:

| Input | Action |
|---|---|
| `p` | Play / pause toggle |
| `f` | Forward 15 s |
| `b` | Back 15 s |
| `n` | Next track |
| `P` | Previous track (capital P) |
| `s` | Cycle speed: 1 → 1.25 → 1.5 → 2 → 0.5 → 1 (prints `speed -> 2x` etc.) |
| `w` | Wake counter: `wakes=N uptime_s=T wakes_per_s=R` (engine heartbeat, background ≈0.5/s) |
| *(Enter, empty line)* | Print current `position_ms` |
| `q` | Quit (saves state on the way out) |
| anything else | `unknown command: …` on stderr |

Without a path argument, `--cli` restores the previous session the same way
the app does (`src/main.rs:76-81`).

---

## 7. Known quirks / limitations

1. **Speed row desync after restore** — shows 1x while audio may run at the
   saved speed; click a segment to resync.
2. **Filename-only track titles** — no ID3 tags, no cover art.
3. **Status line never clears** — last green message persists.
4. **No seek into the final second** (clamp at duration − 1 s).
5. **Light/dark theme switch** not available yet (dark only).
6. `Shift+N` triggers Next (case-insensitive key match).
