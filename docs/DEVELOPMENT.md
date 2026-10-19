# Developing Charon

Charon is a [Tauri 2](https://tauri.app) desktop app: a Rust sync engine (`core/`), a thin Rust
app around it (`src-tauri/`) and a plain HTML/CSS/JavaScript interface (`ui/`). There's no
Node.js and no frontend build step.

## Requirements

- **Rust 1.85+** (`rustup update`)
- **Tauri CLI:** `cargo install tauri-cli --version "^2" --locked`
- Per system:
  - **macOS:** Xcode command line tools (`xcode-select --install`)
  - **Windows:** Microsoft C++ Build Tools ("Desktop development with C++"); WebView2 is built
    into Windows 10/11
  - **Linux (Debian/Ubuntu):**
    `sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev patchelf libdbus-1-dev pkg-config build-essential`

## Running, testing and building

```sh
cargo tauri dev                # run in development
cargo test -p charon-core      # the sync engine's tests (real folders in a temp dir)
cargo tauri build              # installers for the system you're on
```

**Working on the interface only?** Open `ui/index.html` through any local web server
(`python3 -m http.server -d ui`). Outside the app, the window talks to a simulated backend with
example jobs (`ui/js/demo/backend.js`), including a running sync, a conflict and an offline drive.

Each system builds its own installers; a Mac can't build the Windows or Linux versions locally.

| | macOS | Windows | Linux |
|---|---|---|---|
| Installers | `.app`, `.dmg` | `.msi`, `-setup.exe` | `.deb`, `.rpm`, `.AppImage` |
| Folder watching | FSEvents | ReadDirectoryChangesW | inotify |
| Tray icon | menu bar (black, tinted by macOS) | system tray (white) | system tray (white) |
| Launch at login | LaunchAgent | registry | autostart entry |

If the project sits on a disk without file-lock support (some external or network drives),
incremental compilation is already switched off in `Cargo.toml`. For faster builds, put the
build output on the internal disk, e.g. `export CARGO_TARGET_DIR="$HOME/Library/Caches/charon-target"`.

## Releases (all three systems)

`.github/workflows/build.yml` runs the engine's tests and builds macOS (Apple Silicon and Intel),
Windows and Linux on GitHub's servers.

- **Push a version tag** (`git tag v0.1.0 && git push origin v0.1.0`): the installers are attached
  to a **draft** release. Nothing is public until you press *Publish*. They always have the same
  names (`Charon-macOS-Apple-Silicon.dmg`, `Charon-Windows-Setup.exe`, `Charon-Linux.deb`…), so
  `releases/latest/download/<name>` always gives the newest version.
- **Run it by hand** under *Actions → Build Charon → Run workflow*: the installers are kept as
  downloadable artifacts of the run.

### Signing the macOS version

The workflow signs and notarises the macOS installers when the repository has these secrets
(*Settings → Secrets and variables → Actions*):

| Secret | What it is |
|---|---|
| `APPLE_CERTIFICATE` | the *Developer ID Application* certificate as a `.p12`, base64-encoded |
| `APPLE_CERTIFICATE_PASSWORD` | the password chosen when exporting the `.p12` |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Name (TEAMID)` |
| `APPLE_ID` | the Apple ID e-mail of the developer account |
| `APPLE_PASSWORD` | an app-specific password for that Apple ID (account.apple.com) |
| `APPLE_TEAM_ID` | the 10-character team ID |

Without them (in a fork, for example), the app gets an ad-hoc signature: it works, but macOS asks
to confirm it under *System Settings → Privacy & Security → Open Anyway*.

## How the engine decides

Every run, and every preview, follows the same three steps:

1. **Scan** both folders into snapshots: path, size and modification time of every item that
   isn't excluded. Symbolic links are skipped, never followed. Times within 2 seconds count as
   equal (FAT, exFAT and network shares round them).
2. **Plan** what has to change. A preview stops here and shows the plan.
3. **Carry it out**: removals first, then folders from the top down, then the copies. One failed
   file is logged and the run carries on.

**Mirror** and **Backup** compare the source with the destination. Mirror also removes what the
source no longer has; Backup never removes anything.

**Two-way** compares each side with the *baseline*, the state both sides agreed on after the last
run (stored per job in the data folder):

| source vs baseline | destination vs baseline | what happens |
|---|---|---|
| same | same | nothing |
| changed | same | the source's change crosses (a new file, an edit or a deletion) |
| same | changed | the destination's change crosses |
| changed | changed differently | conflict: keep both, newest wins, or ask |

In a conflict between an edit and a deletion, the edit wins. After a run, paths where both sides
agree become the new baseline; paths that still differ (a failed copy, a stopped run, an open
conflict) keep their old baseline entry, so the next run makes the same decision again instead of
copying a deleted file back.

**Safety rules**

- Files are copied to `name.charon-partial` and renamed only when complete, with the original
  permissions and modification time.
- With safe deletes, removed and replaced items are moved into `.charon-trash/<run time>/` at the
  root of their folder, and cleared after the number of days in Settings.
- A two-way folder deletion only removes the folder once nothing but excluded clutter is left in
  it, so files added on the other side meanwhile are never lost.
- A run that would delete every file on one side because the other side is completely empty is
  refused (almost always an unmounted drive or the wrong folder).
- Charon's own files (`.charon-trash`, `*.charon-partial`) are never scanned, copied or watched.

**When jobs run by themselves** (`core/src/autopilot/`): once a second the autopilot looks at each
job: a drive that just became reachable, changes that have settled, a schedule that's due, or a
regular check for pending changes. Schedules count from the last run, so a computer that was asleep
catches up when it wakes. Changes seen during and just after a run are ignored, because they're
Charon's own writes.

## Project layout

```
core/src/                   sync engine (no UI)
  model/                      Job, JobSetup, Settings, JobView, statuses and summaries
  scan/                       Entry, Snapshot, ExcludeFilter, Scanner
  plan/                       Action, Plan, OneWayPlanner, TwoWayPlanner
  run/                        Executor, Copier, Trash, ProgressMeter
  store/                      StateFile (charon.json), Baselines, Journal (logs)
  engine/                     Engine: jobs, runs, previews, checks, conflicts, status
  autopilot/                  Autopilot, Schedule, FolderWatcher
core/tests/                 end-to-end tests with real folders
src-tauri/src/              the desktop app (macOS, Windows, Linux)
  main.rs                     start-up and wiring
  commands.rs                 everything the window can ask for
  events.rs                   engine events → window, tray, notifications
  tray.rs                     Tray: menu-bar / system-tray icon and menu
  notifier.rs                 Notifier: when to show a notification
  login.rs                    LaunchAtLogin
  desktop.rs                  Desktop: show in Finder/Explorer, open links (per system)
  about.rs                    About: version, websites, developer, licence
  updates.rs                  Updates: asks GitHub's latest release whether a newer version is out
src-tauri/tauri.conf.json   app configuration
ui/                         the interface (plain HTML/CSS/JS modules)
  index.html
  img/                        logo-light.svg, logo-dark.svg (the logo without background)
  css/                        base (tokens), controls, layout, rows, overlays
  js/main.js                  App: creates the views, connects to Rust
  js/api.js                   Api: every command and event
  js/store.js                 JobStore: jobs, filters, expanded rows
  js/updates.js               Updates: the "Update" button and the answer from GitHub
  js/views/                   Header, JobList, JobRow, JobEditor, PreviewDialog, SettingsPanel, AboutPanel, PopMenu, Sheet
  js/effects/                 River (animated backdrop), PointerGlow, Counter
  js/demo/backend.js          simulated backend for working on the UI in a browser
.github/workflows/build.yml  tests and installers for all three systems
docs/                       this guide and the README screenshot
```

Code style: small objects with one job each, and a short comment on every function.

## Where data lives

In the app's data folder:

- macOS: `~/Library/Application Support/dev.stean.charon/`
- Windows: `%APPDATA%\dev.stean.charon\`
- Linux: `~/.local/share/dev.stean.charon/`

| File | What's in it |
|---|---|
| `charon.json` | settings and jobs |
| `baselines/<job>.json` | the last agreed state of each two-way job |
| `logs/<job>.log` | each job's activity log |

## Changing the About information

Websites, developer, e-mail and the link to the source code are constants in
`src-tauri/src/about.rs`. The licence text is in `LICENSE`.

## How the update check works

`src-tauri/src/updates.rs` reads `https://api.github.com/repos/steanart/charon/releases/latest`
ten seconds after start-up and then once a day (unless *Look for new versions* is off in
Settings). Drafts don't count, so a new version reaches people only when its release is
**published**. The tag (`v0.2.0`) is compared with the app's own version, and the download link
is the release asset with the fixed name for this computer (`Charon-macOS-Apple-Silicon.dmg`,
`Charon-macOS-Intel.dmg`, `Charon-Windows-Setup.exe`; on Linux the release page). Keep those
names in `build.yml` and `updates.rs` the same.
