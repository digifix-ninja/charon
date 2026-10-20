# Developing Charon

Charon is a [Tauri 2](https://tauri.app) desktop app: a Rust sync engine (`core/`, crate
`charon-core`), a thin Rust app around it (`src-tauri/`, crate `charon`) and a plain
HTML/CSS/JavaScript interface (`ui/`). There's no Node.js and no frontend build step.

Two-way sync is what Charon is for, and nothing matters more than never losing someone's files.
By default a sync never changes anything by itself: Charon prepares the list of changes, shows
it, and carries it out only after the person confirms it. A job can be switched to *Full
automation*, behind two warnings, and then syncs without showing the list first, still inside
the safety checks described below.

## Requirements

- **Rust 1.90+** (`rustup update`)
- **Tauri CLI:** `cargo install tauri-cli --version "^2" --locked`
- Per system:
  - **macOS:** Xcode command line tools (`xcode-select --install`)
  - **Windows:** Microsoft C++ Build Tools ("Desktop development with C++"); WebView2 is built
    into Windows 10/11
  - **Linux (Debian/Ubuntu):**
    `sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev patchelf libdbus-1-dev pkg-config build-essential`
    (the release build on Ubuntu 22.04 in `build.yml` installs `libappindicator3-dev` instead, and
    `xdg-utils`)

## Running, testing and building

```sh
cargo tauri dev                # run in development
cargo test -p charon-core      # the sync engine: unit tests and end-to-end tests with real folders
cargo test -p charon           # the app's own unit tests
cargo tauri build              # installers for the system you're on
```

A development build keeps its own data folder (`dev.stean.charon-dev`, next to the installed
app's), so trying something out never touches the real jobs or their two-way memory. It never
changes launch at login, and it can run next to the installed app: only an installed build
brings the running Charon's window up when it's launched a second time. Its log file is
`charon-dev.log`.

### The tests

Unit tests live next to the code they test, in a `#[cfg(test)] mod tests` at the end of the
file: the planners, the guard, the stores, the executor, the scanner and so on in `core/src`,
and the data folder, notifications and update check in `src-tauri/src`. `cargo test -p charon`
runs the app's; the release workflow runs only the engine's.

The end-to-end tests in `core/tests/` drive the real engine on real folders in temporary
directories:

- `safety_regressions.rs`: the permanent safety net. The first part turns around every
  data-loss probe written against Charon 0.2 for the code review: each one asserts that the loss
  can no longer happen. The second part checks what 0.3 promises: nothing changes without the
  person's OK (unless full automation is on and the job had its first confirmed sync), a list
  can't be confirmed once it no longer matches the folders or the job, unreadable places never
  turn into removals, excluded items are never deleted, and edits made after a list was made are
  never overwritten. Every test asserts the safe outcome. A change to how files are copied,
  replaced or removed must keep all of them passing, and should add one here.
- `engine_runs.rs`: looks, lists and runs through the engine, end to end.
- `run_lifecycle.rs`: how runs start, stop and wait when something around them goes wrong, such
  as a run cut off before it was recorded, a record that can't be saved, or Charon starting
  again.
- `slow_damage.rs`: full automation and damage that spreads slowly: the long count of what
  automatic runs removed or replaced, and a trash that only the person empties.
- `batch_h.rs`: names, links and what a drive refuses.
- `batch_i.rs`: the scanner, network shares and cloud folders.

Some tests in `safety_regressions.rs` need a folder Charon can't read. A process running as root
with `CAP_DAC_OVERRIDE` can read a folder whose permissions are `000`, so there these tests would
prove nothing: they skip, and write a line starting with `SKIPPED` straight to stderr, so a
skipped safety test never looks like one that passed. With `CHARON_REQUIRE_PERMISSION_TESTS=1`
they fail instead of skipping. As root, run them without those capabilities:

```sh
capsh --drop=cap_dac_override,cap_dac_read_search -- -c \
    "cargo test -p charon-core --test safety_regressions"
```

`build.yml` doesn't set the variable. The tests that need folder permissions or symbolic links
are Unix only (`#[cfg(unix)]`). One test waits for a list to expire (15 minutes) and is ignored
by default: `cargo test -p charon-core --test safety_regressions -- --ignored`.

### Working on the interface only

Serve `ui/` with any local web server (`python3 -m http.server -d ui`) and open it. Outside the
app, `ui/js/api.js` loads `ui/js/demo/backend.js`, a pretend Rust side with example jobs: a
two-way job with conflicts, jobs with full automation, places that couldn't be read, a drive
that isn't connected and a paused job. It answers every command and sends every event the real
one does, so keep it in step when you add either. From the browser console:

- `charonDemo.syncAll()`: the tray's *Sync All…*
- `charonDemo.showWaiting()`: the tray's *Review Waiting Jobs…*
- `charonDemo.quit()`: the tray's *Quit Charon* while jobs wait for an OK
- `charonDemo.quitting()`: Charon quits while a sync runs, and stops it safely first
- `charonDemo.change("projects")`: something changes in a job's folders, so its open list goes
  stale
- `charonDemo.refuseNext()`: the next *Confirm* is refused, to see how that looks

### Per system

Each system builds its own installers; a Mac can't build the Windows or Linux versions locally.

| | macOS | Windows | Linux |
|---|---|---|---|
| Installers | `.app`, `.dmg` | `.msi`, `-setup.exe` | `.deb`, `.rpm`, `.AppImage` |
| Folder watching | FSEvents | ReadDirectoryChangesW | inotify |
| Tray icon | menu bar (black, tinted by macOS) | system tray (white) | system tray (white) |
| Launch at login | LaunchAgent | registry | autostart entry |

If the project sits on a disk without file-lock support (some external or network drives),
incremental compilation is already switched off in `Cargo.toml`. For faster builds, put the
build output on the internal disk, for example
`export CARGO_TARGET_DIR="$HOME/Library/Caches/charon-target"`.

## Releases (all three systems)

`.github/workflows/build.yml` (*Build Charon*) builds Charon on GitHub's servers for macOS (Apple
Silicon and Intel), Windows and Linux. Each build first runs the engine's tests
(`cargo test -p charon-core`; failing tests also show as annotations on the run's page), then
makes the installers and gives them fixed names without the version:
`Charon-macOS-Apple-Silicon.dmg`, `Charon-macOS-Intel.dmg`, `Charon-Windows-Setup.exe`,
`Charon-Windows.msi`, `Charon-Linux.AppImage`, `Charon-Linux.deb` and `Charon-Linux.rpm`. So
`https://github.com/steanart/charon/releases/latest/download/<name>` always gives the newest
published version. Every run keeps the installers as downloadable artifacts of the run.

The workflow starts in two ways:

- **Push a version tag** (`git tag v0.3.0 && git push origin v0.3.0`). Once every build has
  succeeded, a release job puts all the installers into a **draft** release for that tag.
  Nothing is public yet.
- **Start it by hand** under *Actions → Build Charon → Run workflow*. With the version left
  empty it's a test build: the installers are only kept as artifacts of the run. With a version
  (`v0.3.0`), the same draft release is made for the built commit, and GitHub creates the tag
  when the release is published (handy when git can't push tags).

If a release with that tag exists already, the release job leaves it as it is. (Publishing a
draft creates its tag, which can start the workflow again.)

`.github/workflows/publish.yml` (*Publish Charon*) makes a draft public. Start it by hand under
*Actions → Publish Charon → Run workflow* with the version of the draft (`v0.3.0`) and, if you
like, release notes in Markdown (left empty, the draft's notes stay). It sets the notes, publishes
the release and marks it as the latest. From then on `releases/latest` points to it, and the
copies of Charon already out there see it as an update. A draft can also be published by hand
on GitHub.

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

With the certificate and identity but without `APPLE_ID`, `APPLE_PASSWORD` and `APPLE_TEAM_ID`,
the app is signed but not notarised. Without the certificate (in a fork, for example), the app
gets an ad-hoc signature: it works, but macOS asks to confirm it under *System Settings →
Privacy & Security → Open Anyway*.

## How the engine decides

Every look at a job, every list the person confirms and every run of a job with full automation
follows the same path: a **survey** looks at both folders and plans, the **guard** and the
other checks say what deserves a look, and only a confirmed list (or an automatic run with
nothing to hold it back) is **carried out**. Afterwards Charon **records** what really
happened.

### Looking at both folders: the survey

`core/src/engine/survey.rs` makes a survey for one of three purposes: a background look
(`Check`), a list the person will see (`Review`), or a run nobody looks at (`Automatic`). A
survey never changes the person's files. (Where neither the system nor the names in a folder
show whether its drive ignores case, and the folder has no `.charon` folder yet, the survey makes
one with a single empty file of Charon's own and removes both at once.) It:

1. Checks that both folders are there (otherwise the job is *Offline*), reads the job's markers
   (`.charon/<job>.json`) at the top of both folders, and finds out what kind of drive each
   folder is on (`scan/volume.rs`): APFS, exFAT, an SMB share and so on, whether it ignores
   upper and lower case, and what it can hold.
2. Scans both folders at once, each on its own thread (`scan/scanner.rs`). On macOS and Linux,
   symbolic links are recorded as links and never followed; on Windows, links and junctions are
   left out with a note, and so are names Windows can't handle. Also left out: the job's exclude
   patterns; Charon's own names at any depth (`.charon`, `.charon-trash`,
   `.charon-<16 hex digits>.part` and Charon 0.2's `*.charon-partial`); on a Mac, every `._*`
   file, whatever the patterns say (on drives that aren't Mac drives it holds another file's tags
   and resource fork); names that aren't valid Unicode; sockets, pipes and devices; and, at the
   top of the job's folder, items named like the folders drives keep for themselves
   (`$RECYCLE.BIN`, `System Volume Information`, `.Trashes`, `.Spotlight-V100`, `lost+found`,
   `.Trash-*` and the like), with a note when the job's folder isn't the top of its drive. A new
   job starts with the exclude patterns `.DS_Store`, `._*`, `.Spotlight-V100`, `.Trashes`,
   `.fseventsd`, `Thumbs.db`, `desktop.ini`, `$RECYCLE.BIN`, `~$*` and `*.tmp`, so files matching
   `*.tmp` or `~$*` aren't synced until the pattern is removed. A place that can't be read is
   recorded as *frozen*, never as empty, and nothing at or inside it is touched on either side.
   Files and folders that are only in the cloud (recognised on macOS and Windows) and drives
   mounted inside the folder are frozen too; on macOS the scan threads are also set never to
   download a file that's only in the cloud.
3. Turns every path into a *key* (`scan/keys.rs`): relative to the job's folder, `/`-separated
   and in Unicode NFC, and folded when either drive ignores case (each letter to the lower case
   of its upper case, so ς and σ, or µ and μ, are one letter). Two names in one folder that end
   up as the same key are a name clash, and are frozen too.
4. Two-way: loads the memory of the last sync (`store/baselines.rs`) and decides whether to
   trust it. Mirror and Backup: loads the record of copies whose dates didn't stick
   (`store/landings.rs`).
5. Two-way: reads pairs of files byte for byte where the dates alone can't tell
   (`compare_contents`): pairs of the same size that have no record and different dates, or
   that changed on both sides since the record (whatever their dates), and a file that changed
   on one side but kept its size while its time stayed put or moved only by a rounding or by
   whole quarter hours. Identical pairs are recorded as in sync; pairs that changed on both
   sides and look alike but differ become conflicts. Without a record, a pair of the same size
   whose times are at most 2 seconds apart is taken as identical without being read. A
   background look reads at most 1 GB and counts the rest as identical for that look only;
   lists and runs read everything.
6. Plans (`plan::plan_for`), then runs the guard and the other checks. Each check can add a
   **warning** to the list, a **hold** that stops a run nobody looks at, and the need for a
   **tick** before the list can be confirmed.

### Mirror and Backup

`plan/one_way.rs`. The destination follows the source. Two items count as the same when they're
both folders, files of the same size whose modification times are at most 2 seconds apart (FAT,
exFAT and network shares round them), or links pointing to the same place. So an edit that
keeps a file's size and its time (within those 2 seconds) isn't seen, and that file isn't copied
again until one of them changes. A new or different file is copied, except:

- a copy whose date didn't stick on the destination's drive (`landings/<job>.json`), while
  neither side has changed since it landed. This record only ever spares a copy; it never
  justifies removing or replacing anything;
- on FAT and exFAT, which keep local times, a file of the same size whose time is a whole number
  of hours off (a daylight-saving change or another time zone).

Mirror also removes, item by item, everything in the destination that isn't in the source,
including files that were only ever put in the destination. Backup never removes anything.
When an item of another kind is in the way (a file where the source has a folder), Mirror
removes it first, unless it's a folder holding items Charon leaves alone; Backup leaves it alone
with a note. A link in the source where the destination has a file or folder never removes it,
in either mode.

Mirror and Backup check that they're looking at the folders of the last sync: the destination
must carry the marker token Charon left there, and the source's marker (where Charon could
write one) must have the same token and counter. A source whose counter went down is probably
an older copy of the drive, which would replace the newest backups.

### Two-way

`plan/two_way.rs` and `plan/baseline.rs`. When a file is missing on one side, Charon has to know
whether it was removed there or is new on the other side. So it keeps a *memory* (the
baseline): for every item both folders agreed on after the last sync, how it looked *on each
side*. Each side is compared with its own record, exactly: kind, size, modification time to the
millisecond, link target, and the drive's change time where both looks know one. On FAT and
exFAT, a whole-hour shift of a file of the same size doesn't count as a change.

| source vs memory | destination vs memory | what happens |
|---|---|---|
| same | same | nothing |
| changed | same | the source's change crosses (a new item, an edit or a removal) |
| same | changed | the destination's change crosses |
| changed | changed differently | conflict, settled by the job's rule (*Keep both*, *Newest wins* or *Ask me*) |

- Both sides changed in the same way (they look alike now and weren't found to differ): nothing
  to do.
- An edit on one side and a removal on the other: the edit wins and is copied back.
- A file whose time is now older than its own record (restored from a copy, or a drive that
  lost its last writes) is treated as a conflict, never as an edit that crosses, and *Newest
  wins* never settles it.
- *Keep both* keeps the destination's version as `name (conflict 2026-10-16 16.20).ext` on both
  sides and the source's under the original name. *Newest wins* copies the newer version over the
  older one, which goes to `.charon-trash`. It only decides when the times can be trusted: both
  drives local and of a known kind, neither keeping local times (FAT, exFAT), the times more than
  2 seconds apart and not a whole number of hours apart, and neither side's time gone backwards.
  Otherwise two files are both kept, as with *Keep both*, and two links are left for the
  person. *Ask me* leaves the item for the person.
- When both sides changed an item (or there's no memory to tell), a file meeting a folder is
  always a question for the person, and so are two different links unless *Newest wins* can
  tell which is newer (then the newer link is carried and the other goes to `.charon-trash`). A
  link taking the place of a file or folder is always a question. When only one side changed an
  item into another kind, that change crosses: a folder that became a file is replaced on the
  other side only when that folder holds nothing Charon leaves alone and nothing changed in it.
  The item replaced by one of another kind is removed like any other removal, so with safe
  deletes off it's gone for good.
- **Units** (`plan/units.rs`): an app's library or document package (a Photos or Final Cut
  library, a Logic project, a Pages document, a sparse bundle; the list is `PACKAGES`) and an
  SQLite database with its `-wal`, `-shm` or `-journal` file only work as a whole. When files of
  one unit changed on both sides, the whole unit becomes one conflict, and none of its files
  moves.
- A folder is only removed when everything inside it is removed by the same plan.

Without a memory Charon can trust, **nothing is removed**: an item on one side only is copied to
the other, and two files that differ are kept both or left for the person, except with *Newest
wins*, which still replaces the older version when the times can be trusted (the older one goes
to `.charon-trash`). That's the case for the first sync, and whenever the memory can't be read or
is damaged, was saved by Charon 0.2 or earlier, the last sync didn't finish (`run_in_progress` is
still set), a drive now treats case differently, or a folder's marker is missing, unreadable,
carries another token, or has a lower or higher counter than the memory. The list then says why,
and an automatic run is held.

### Settling one conflict

`plan/resolve.rs`. A conflict in a job's row offers *Keep source*, *Keep destination* and, for
two files, *Keep both*. The choice prepares a list like any other, which touches only that item
(and what's inside it) and is confirmed the same way; the version it replaces or removes goes
to `.charon-trash`, even with safe deletes off. An app library or database is settled as a
whole, and can't be kept both. When the memory is trusted, only that item's record changes;
when it isn't, settling the item makes Charon forget the memory, so every look compares afresh
and removes nothing until a whole list is confirmed.

### The guard

`plan/guard.rs` looks at every plan. A **refusal** stops it in every mode, and such a list can't
be confirmed: one side has no files while the plan would remove on the other side, which has
some (almost always a drive that isn't connected properly, or a wrong folder).

A **concern** is a plan that looks like something went wrong. It holds an automatic run, shows
in red in the list, and the list needs the tick *I've looked at the removals and replacements*.
Counted per side, on files and links:

- removals of at least 3 files that are at least half of the files there;
- removals and replacements of at least 5 files that are at least half of them, of more than 20
  that are more than a quarter of them, or of more than 1,000 in any case;
- a file with contents that would be replaced by an empty one;
- a folder on the other side that's there but looks empty, while the plan would remove in it on
  this side and it held at least 5 items at the last sync (two-way), or the plan removes at
  least 5 items from it. A share or cloud folder listed before it has loaded looks like that.

For jobs with full automation (in their automatic runs, and in their lists, so a list shows why a
run was held), what automatic runs did before counts too, each file once:

- **The day window:** the files automatic runs removed or replaced during the last 24 hours
  (kept in the job's record in `charon-state.json`, at most 1,001 per side, more than the largest
  threshold), added to this plan's, against the files there plus those removed since: the
  removal-and-replacement thresholds above apply.
- **The long count** (`store/unattended.rs`): every file automatic runs removed or replaced since
  the person last confirmed a list, kept in `unattended/<job>.json` for the last *Keep deleted
  files* days (with 0, until the next confirmed list), at most 100,000 per side. Together with
  this plan's, more than 20 files that are more than a quarter of the side is a concern, and so
  is a count that overflowed. This catches damage that spreads more slowly than a day allows (a
  broken tool rewriting a few dozen files an hour). Damage spread more slowly than *Keep deleted
  files* days isn't caught, though the originals stay in the trash, which automatic runs never
  empty. A count that can't be read is never taken for zero: any plan that removes or replaces
  something is then a concern.

The survey adds its own checks besides the guard:

- **Hold an automatic run and need the tick:** a folder now on another drive than at the last
  sync, and a Mirror or Backup marker that doesn't match.
- **Hold an automatic run:** a two-way memory Charon doesn't trust, files changed inside
  `.charon-trash` after Charon put them there, and not enough room on a drive.
- **Need the tick:** with safe deletes off, a list that removes files (or, in Mirror and Backup,
  replaces them).

Lists also warn about places that couldn't be read, items that can't be synced, conflicts
settled by *Newest wins*, symbolic links, the trash growing large, and app libraries and
databases the sync changes (close the app first). That last one is a warning only: it doesn't
hold an automatic run, so with *Full automation* a library or database may be copied while its
app is writing to it. Leave such folders out of jobs with full automation, or quit the app
before the sync.

### Lists and confirming

`engine/review.rs`. *Review and sync* on a job's row prepares a list from a fresh survey. It's
kept, one per job, until it's confirmed, closed, replaced by a newer one or the job changes. It
shows the steps (removals first, then what replaces something, conflicts, new folders and
copies), the notes (places left alone, with the reason), the warnings, and housekeeping such as
old trash it would empty.

A place that couldn't be read offers *Leave out*, which adds an exclude pattern for exactly that
place; the next sync then waits for an OK.

*Sync all* (and the tray's *Sync All…*) starts the jobs that run by themselves and opens *Waiting
for your OK* for the others (paused jobs, jobs whose folders aren't there and jobs already
syncing are left out), where each list is prepared and confirmed on its own. There is
deliberately no way to confirm them all at once.

*Confirm* carries out exactly that list and nothing else. The engine refuses it when:

- the list is more than 15 minutes old;
- something that matters changed in the folders since it was made (the folder watcher marks it
  stale, and the window hears `review-stale`);
- a folder isn't there, isn't the same folder anymore (`RootId`: its drive, its number on the
  drive and, where the system keeps a believable one, its creation time), or carries another
  marker;
- the job's settings changed, or the job synced in the meantime;
- the guard refused the plan;
- the list needs the tick and it isn't ticked.

A list that's out of date offers *Look again*, which makes a fresh one. While a list is being
prepared or waits for an OK, automatic runs of that job wait too.

### Carrying out a list

`run/executor.rs` runs the steps in this order (`Plan::ordered`): removals that clear the way
for an item of another kind (deepest first), new folders (from the top down), copies, and then
every other removal (deepest first). Removing last means a run that stops halfway never leaves an
item that was moved or renamed with only one copy. Conflicts left for the person are never
touched.

Before every step, both folders must still be the folders the plan was made for, or the run
stops. Right before a step acts it looks at the disk again, on both sides: the item it copies,
replaces or removes must be exactly what the plan saw, and so must the other side's item the
step relies on (a file restored on the other side stops its removal here). If anything changed,
the step is skipped, never forced, and the next look picks it up. The folders above an item are
checked one by one, so nothing is ever written or removed through a link. On macOS and Linux,
what a step wrote must be on the folder's drive; if the drive went away, the run stops there.

- **Copies** (`run/copier.rs`) are written to a new file `.charon-<16 hex digits>.part` next to
  the target (the first 8 digits are when the copy started), never through a link. The source is
  checked before and after reading, and a copy of a file that changed meanwhile is thrown away.
  When the source's drive keeps times in steps of a second or more, a file saved there in the
  last 3 seconds waits for the next sync. The copy is flushed, takes the extended attributes
  worth carrying (Finder tags, Finder info and resource forks on macOS, `user.` attributes on
  Linux) and the original's modification time (and its creation time on macOS and Windows), and
  is flushed again; then it takes the original's permissions (a drive that refuses them only
  leaves a line in the app's log). A drive that can't keep extended attributes (FAT, exFAT)
  gets the file without them, apart from a resource fork and the Finder info that goes with it,
  and the run's summary says what stayed behind. On macOS, a file the cloud took back since the
  scan is left alone, never downloaded.

  Tags, permissions and extended attributes travel only with a copy of the file: a change to
  them alone, with the contents and the modification time as they were, isn't synced (two-way
  finds the contents identical, and Mirror and Backup don't look at them). A copy never carries
  ACLs, owner and group, other extended attributes (such as the quarantine flag), or, on
  Windows, extended attributes and alternate data streams. Hard links arrive as separate files,
  and sparse files take their full size.
- **Taking the real name** (`run/rename.rs`) uses a rename that never replaces anything
  (`renameat2` with `RENAME_NOREPLACE` on Linux, `renamex_np` with `RENAME_EXCL` on macOS,
  `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` on Windows). Where a drive can't do that,
  the drive itself claims the name: a second name for the file (a hard link), or an empty
  placeholder that the file is renamed over. Something that appeared under the name before
  Charon claimed it is never overwritten. On drives that need the empty placeholder, only
  something written into it in the instant before the rename could be.
- **Replacing** a file: where the drive can swap two names in one step (`RENAME_EXCHANGE` on
  Linux, `RENAME_SWAP` on macOS), the copy and the old file swap, and the file swapped out is
  checked; if an app saved a new version at that very moment, the two swap back. Elsewhere
  (Windows, FAT and exFAT, most network shares) the old file is renamed aside and checked first,
  and only then does the copy take the free name. Either way, what an app saved is put back or
  kept in `.charon-trash`, never written over.
- **Keeping the old version:** with safe deletes, always in two-way jobs, and always when a copy
  settles a conflict, a file being replaced is first *copied* into the trash and then replaced
  in place (a link is moved into the trash). An app that has the file open follows the file, not
  its name: had the old version been moved into the trash, the app could go on saving the
  person's work there. Replaced in place, the app finds the new version where the file was. On
  APFS that copy is a clone, which takes no time and no room. An item replaced by one of another
  kind (a file by a folder) is removed first, as a removal (below).
- **Removals:** a file or link is renamed aside, checked once more, and then moved into
  `.charon-trash` (with safe deletes) or removed for good. On APFS a file kept that way is cloned
  into the trash and then removed. A folder is removed only when nothing but system clutter
  (`.DS_Store`, `Thumbs.db`, `desktop.ini`, `._*` files and the like) is left in it, and that
  clutter always goes to the trash.
- **Keep both:** the destination's version is renamed to its conflict name (numbered when taken,
  and shortened to fit the drive's limit on name length by `run/names.rs`), the source's version
  is copied to the destination under the original name, and the conflict copy is copied to the
  source.
- **Databases:** the files of one SQLite database are carried together. Every new version is
  written next to its target, everything is checked again, and only then do the old journals go
  and the new files take their places, one right after the other. If a new version can't be
  written, or anything changed while they were written, none is carried. A failure once the
  files are taking their places (rare) stops the rest and can leave the database part old and
  part new; the next sync looks at it again. With safe deletes on, the journals that were
  removed are in `.charon-trash`.
- **Room** (`run/room.rs`): Charon keeps 2% of every drive it writes to free, at most 2 GB, where
  the drive says how much room it has, so other apps can still save. A copy that would eat into
  that fails with a clear reason. On a Mac, room the system can free by itself counts as free.
- **One failed item doesn't stop the run:** it's counted, written to the job's log (the first 50
  failures, and the first 50 of the skipped and left-out items together) and shown. A side where
  a copy failed, or where the drive refused an item, keeps everything the run would still remove
  there. Charon remembers what a drive refused until it quits (or the job's folders or mode
  change), while the item and the drive stay the same: later lists leave it out with the reason,
  and after a restart it's tried again.
- **The trash** (`run/trash.rs`): each run puts what it removes or replaces into
  `.charon-trash/<YYYY-MM-DD_HH-MM-SS>_<6 hex digits>/` at the top of the folder, with a
  `.charon` list of every file's size and time. A file that no longer matches its list was
  changed after it went in (an app may still be saving someone's work into it): its dated folder
  is never emptied, lists say where it is, and automatic runs are held. Dated folders older than
  *Keep deleted files* (30 days by default; 0 keeps everything) are emptied only after a run the
  person confirmed, whose list named them, went all the way. Runs nobody looked at never empty
  the trash. If `.charon-trash` is a link or a file, nothing is put into it.
- **Leftovers:** `.part` files left by an interrupted sync are moved into the trash by a run that
  went all the way, once nothing has written to them for an hour and their copy started more
  than an hour ago. Files named like Charon 0.2's half-copied files (`*.charon-partial`) may be
  the person's own, so only a list that names them moves them.
- **Durability** (`run/durable.rs`): at the end, every folder the run changed (and the trash's
  lists) is flushed to its drive, both folders are checked once more, and then each drive is
  flushed. Until that succeeds, nothing about the run is recorded. Each file is flushed as it's
  written; flushing folders and whole drives is as good as the system allows. On Windows there's
  no drive flush, and folders are flushed where Windows lets Charon open them. Drives that can't
  flush a folder (many network shares) are taken at their word and write it through in their
  own time.

### What is recorded after a run

`engine/runs.rs`. Before anything is carried, the job's record is marked `run_in_progress`; if
that can't be saved, the run changes nothing. Once the run is durable:

1. Both markers must still be what the survey found. If one changed (another drive came in under
   the same name), nothing about the run is recorded, the two-way memory is forgotten, and the
   next sync waits for an OK.
2. Two-way: the markers are written on both sides (the same tokens with the counter one higher,
   or new tokens and counter 1 when the memory wasn't trusted), then the memory. The new memory
   holds what each finished step left behind, the items both sides already agreed on as the plan
   saw them, and the old record for everything else (a skipped or failed step, an open conflict,
   an unreadable place, a run that stopped early), so the next run decides again instead of
   copying a removed file back. If a marker or the memory can't be written, the memory is
   forgotten, and the next look compares both folders afresh and waits for an OK.
3. Mirror and Backup: the destination's marker, the copies whose dates didn't stick, the
   source's marker where Charon can write there (its counter goes up with every sync), and then
   the tokens in the job's record.
4. The job's record always gets the run's summary, and a run that ended without an error and
   wasn't stopped also sets the time of the last successful sync. A recorded run clears
   `run_in_progress` and, unless it only settled one conflict, saves the drives both folders
   were on; a run that couldn't be recorded leaves `run_in_progress` set. A run nobody looked at
   adds what it removed or replaced to the day window and the long count.
5. After a clean sync the person confirmed, the job no longer waits for an OK (so a job with full
   automation runs by itself from then on), and the day window and the long count start again.
   After *Stop*, a run that couldn't be recorded, a long count that couldn't be saved, or a sync
   that ended on an error of Charon's own, the next sync waits for an OK and the row says why. A
   run stopped because Charon quit carries on as before next time.
6. The job's activity log gets the summary, the items that failed, were skipped or were left
   out, the conflicts *Newest wins* settled, and the files that arrived without details their
   drive can't keep (up to 50 of each, skipped and left-out items sharing one 50).

### When jobs run by themselves

`core/src/autopilot/` starts three seconds after launch and looks at every job once a second. A
job **runs by itself** only when *Full automation* is on, its next sync doesn't wait for the
person's OK (`Job::runs_by_itself`), it isn't paused, *Pause everything* is off, and no list of
its changes is being prepared or waiting. A new job waits for a confirmed list first, and so
does a job whose folders, mode, conflict rule, excludes or safe deletes changed, that just had
full automation turned on, or that was held back or stopped. In the job editor, changing the
mode, turning safe deletes off or swapping a one-way job's folders also turns *Full automation*
off; it has to be turned on again, with its two warnings. Every other job is only *looked at*:
Charon scans and plans in the background, the row shows *Needs your OK* when changes wait
(unless it shows *Syncing*, *Offline*, *Paused*, *Conflict* or *Error*, which come first), and
nothing changes in its folders until the person confirms a list.

For each job whose folders are reachable and that isn't syncing, in this order:

1. **Drive connected:** a *Drive connected* job whose folders just became reachable (at launch
   too) syncs, or is looked at.
2. **Changes settled:** the folder watcher reported a change that matters (not an excluded item,
   not Charon's own files, and not during or within 3 seconds after the job's own sync), and the
   folders have been quiet for the job's settle time (*On change*) or 5 seconds (any other
   trigger). An *On change* job that runs by itself syncs; every other job is looked at.
3. **Schedule:** an *Every…* or *Daily* job is due (not while it or everything is paused).
   Schedules count from the last run, the last time they came due, or the job's creation,
   whichever is latest, so a computer that was asleep catches up when it wakes. Daily times are
   local. On a day when a daylight-saving change skips the chosen time (02:30 on the night the
   clocks go forward, for example), the job doesn't run that day; the next run is at the same
   time the following day.
4. **Regular look:** every *Look for changes* minutes (15 by default), in case the watcher missed
   something. An *On change* job that runs by itself and has changes found by a look syncs.

*Sync now*, shown instead of *Review and sync* for a job that runs by itself, starts the same
kind of run. A run nobody looks at makes a fresh survey and goes ahead only when nothing holds
it. Otherwise it's **held back**: nothing is touched, the log (and, with *Notifications* on, a
notification) says why, and the job shows *Needs your OK* until the person confirms a list.

Removals that only follow from an item missing for less than ten minutes on a drive that may
show changes late (a network share, or a drive run by a program, like sshfs and cloud drives
that mount as a FUSE drive) are left for a later automatic run; a list the person confirms
shows them all. Cloud folders on the computer's own disk (where iCloud Drive, Dropbox and
OneDrive usually keep them) count as local: this rule doesn't cover them, and *Newest wins*
trusts their times.

When changes or conflicts wait for the person, Charon says so in one notification, reminds them
after two hours, and then once a day (`Engine::remind`). Paused and offline jobs aren't
mentioned, and right after a start Charon waits for the first looks (at most two minutes), so the
jobs found waiting come in one notification.

### Safety rules

- Without *Full automation*, nothing in a job's folders changes until the person confirms the
  list. With it, a job syncs by itself only after a confirmed sync, and needs one again after
  every hold, stop, or change to what a sync does.
- A run nobody looked at goes ahead only when nothing about it needs a look; otherwise it touches
  nothing and waits for an OK.
- A plan that would remove from one side because the other side is empty is refused in every
  mode, and can't be confirmed.
- Without a two-way memory it can trust, Charon removes nothing.
- An edit beats a removal, and a file that went back in time is never "the newest".
- Every step checks both sides again right before it acts; an item that changed is skipped,
  never forced.
- Copies are written under a temporary name and take the real name only when complete, with a
  rename that never replaces what appeared there before Charon claimed the name.
- Replaced files and links always go to `.charon-trash` in two-way jobs and when a conflict is
  settled; with safe deletes, removed and replaced items in every mode do too. An item replaced
  by one of another kind (a file by a folder) counts as a removal: outside of settling one
  conflict, with safe deletes off it's gone for good.
- Removals come last, and a side where copies failed keeps the run's removals.
- A folder is removed only when everything in it is removed; folders holding unreadable items,
  or excluded items other than system clutter, are never removed (clutter such as `.DS_Store`
  goes to the trash with its folder).
- Places that can't be read, names written two ways, and items the other drive can't hold are
  left alone on both sides, with a note in the list.
- Symbolic links are never followed. Never synced: Charon's own files (`.charon`,
  `.charon-trash`, `.part` files) and anything named like them or like Charon 0.2's
  `*.charon-partial` files, at any depth; `._*` files on a Mac, whatever the patterns say; names
  that aren't valid Unicode; and sockets, pipes and devices.
- A run stops as soon as a folder isn't the one the plan was made for.
- Nothing about a run is recorded until its changes are flushed to the drives (as far as each
  system and drive allows), and only while both markers are unchanged.
- Runs nobody looked at never empty the trash.
- Charon keeps 2% of every drive it writes to free, at most 2 GB, where the drive says how much
  room it has.
- Only one Charon works with a data folder at a time: a second one finds `charon.lock` locked
  and refuses to start. (On a disk that can't lock files at all, Charon starts anyway; the data
  folder is on the computer's own disk, where that shouldn't happen.)

## Project layout

```
core/                         the sync engine (crate charon-core, no interface)
  src/
    lib.rs                    the crate's modules at a glance
    error.rs                  Error: one error type, with messages written for people
    model/                    the data Charon keeps and what the window receives
      mod.rs
      job.rs                  Job, JobSetup, SyncMode, ConflictRule, Trigger, the day window
      settings.rs             Settings
      view.rs                 JobStatus, JobView, Progress, RunSummary, Overview
      review.rs               Review, ReviewStep, Warning, WaitingJob, the upgrade notice
    scan/                     reading a folder into a snapshot
      mod.rs
      entry.rs                Entry, Snapshot, and what a scan learned besides the items
      contents.rs             Contents: comparing two files byte for byte
      filter.rs               ExcludeFilter, system clutter, Charon's own names
      keys.rs                 Keys: NFC, /-separated, folded when a drive ignores case
      root.rs                 RootId: which folder a path really is
      scanner.rs              Scanner: walks a folder, noting what it couldn't read
      volume.rs               Volume: the kind of drive, and what it can hold
    plan/                     deciding what crosses, and which way
      mod.rs                  plan_for: the planner for the job's mode
      action.rs               Action, Note, Plan, PlanSummary, the order steps run in
      ground.rs               places a plan must not touch; the last look at folder removals
      one_way.rs              OneWayPlanner: Mirror and Backup
      two_way.rs              TwoWayPlanner
      baseline.rs             Baseline and Pair: the two-way memory
      guard.rs                Guard: refusals and concerns, the day window, the long count
      resolve.rs              Resolution: settling one conflict
      units.rs                Unit: app libraries and SQLite databases
    run/                      carrying out a plan
      mod.rs
      executor.rs             Executor: checks each step against the disk, then runs it
      copier.rs               Copier: one file or link, through a temporary .part file
      rename.rs               renames that never replace, and swaps
      durable.rs              flushing files, folders and drives
      room.rs                 Room: free space, and the room Charon leaves for other apps
      trash.rs                Trash: .charon-trash, its lists and its clean-up
      marker.rs               Marker: .charon/<job>.json in the job's folders
      meter.rs                ProgressMeter
      names.rs                conflict and trash names that fit a drive's limits
    store/                    what Charon keeps in its data folder
      mod.rs                  DataFolder: where each file lives
      state_file.rs           StateFile: charon-state.json, and the move from charon.json
      baselines.rs            Baselines: each job's two-way memory
      landings.rs             Landings: Mirror and Backup copies whose dates didn't stick
      unattended.rs           Unattended: the long count of automatic changes
      journal.rs              Journal: one activity log per job
      lock.rs                 DataLock: one Charon per data folder
      atomic.rs               saving a file so a crash never leaves it half-written
    engine/                   the orchestration
      mod.rs                  Engine and EngineEvent: opening, settings, rows, events
      jobs.rs                 adding, editing, duplicating, pausing and removing jobs
      survey.rs               the survey: both folders, the plan and every check
      review.rs               lists waiting for an OK, confirming them, settling conflicts
      runs.rs                 starting, carrying out, recording and holding back runs
      checks.rs               background looks, folder changes, reminders
      status.rs               the row the window shows for a job
      runtime.rs              each job's live state, which isn't saved
    autopilot/                looking at jobs and running them by themselves
      mod.rs                  Autopilot: the once-a-second loop
      schedule.rs             Schedule: when an Every… or Daily job is due
      watcher.rs              FolderWatcher: changes inside the folders
  tests/                      end-to-end tests with real folders (see above)
src-tauri/                    the desktop app (crate charon)
  src/
    main.rs                   start-up, plugins, the app's log, wiring
    state.rs                  AppState: what every command can reach
    commands.rs               everything the window can ask for
    events.rs                 engine events to the window, the tray and notifications
    tray.rs                   Tray: the menu-bar or system-tray icon and its menu
    notifier.rs               notifications, and Repeats (the same problem told once)
    quit.rs                   Quit: asks first when changes wait, lets syncs stop cleanly
    login.rs                  LaunchAtLogin
    desktop.rs                showing folders and opening links, per system
    about.rs                  About: what the About panel shows, and the links it may open
    updates.rs                Updates: asks GitHub whether a newer version is out
    data_folder.rs            DataFolder: the data folder, a dev build's own, the move from 0.1.0
  capabilities/default.json   what the window may do
  icons/                      app and tray icons
  tauri.conf.json             app configuration (identifier dev.stean.charon)
ui/                           the interface (plain HTML, CSS and JavaScript modules)
  index.html
  css/                        base (tokens), controls, layout, overlays, review, rows
  img/                        logo-light.svg, logo-dark.svg (the logo without background)
  js/
    main.js                   App: creates the views, connects them to Rust
    api.js                    Api: every command and event
    store.js                  JobStore: jobs, settings and what the person is looking at
    format.js                 Format: words and numbers, and the status names (STATUS_NAMES)
    dom.js                    Dom: building markup safely
    icons.js                  Icons: inline SVG icons
    logo.js                   Logo: the logo for light and dark
    platform.js               Platform: names that differ per system ("menu bar")
    theme.js                  Theme: light or dark
    toast.js                  Toast: a short message at the bottom
    updates.js                Updates: the "Update" button and GitHub's answer
    views/
      header.js               Header: status line, "Pause all", overview tiles
      job-list.js             JobList: the rows
      job-row.js              JobRow: one row and its detail, with conflicts to settle
      job-editor.js           JobEditor: the job editor, and Full automation's two warnings
      review-dialog.js        ReviewDialog: the list of changes, and settling one conflict
      waiting-sheet.js        WaitingSheet: "Waiting for your OK"
      upgrade-sheet.js        UpgradeSheet: what changed on the first start of 0.3
      settings-panel.js       SettingsPanel
      about-panel.js          AboutPanel
      sheet.js                Sheet: a panel that slides in from the right
      modal.js                Modal: a question that must be answered first
      overlay.js              Overlay: the stack of everything over the window
      pop-menu.js             PopMenu: a small menu under a button
    effects/
      river.js                River: the animated backdrop and the boat
      glow.js                 PointerGlow: cards that light up around the pointer
      counter.js              Counter: numbers that roll to their new value
    demo/
      backend.js              DemoBackend: the pretend Rust side for a plain browser
.github/workflows/            build.yml and publish.yml (see Releases)
docs/                         this guide and the README screenshot
```

Code style: small objects with one job each, and a short comment on every function.

## Where data lives

In the app's data folder:

- macOS: `~/Library/Application Support/dev.stean.charon/`
- Windows: `%APPDATA%\dev.stean.charon\`
- Linux: `~/.local/share/dev.stean.charon/`

A development build uses `dev.stean.charon-dev` next to it instead.

| File | What's in it |
|---|---|
| `charon-state.json` | settings and jobs, with each job's record: its last run, whether its next sync waits for an OK and why, the drives and Mirror/Backup markers of the last sync, and what automatic runs removed or replaced during the last day |
| `charon-state.json.damaged-<date>-<time>` | a state file that couldn't be read, put aside instead of overwritten (Charon then starts without jobs) |
| `charon.json.moved-to-0.3` | Charon 0.2's settings and jobs, after they moved (see below) |
| `charon.lock` | locked while a Charon works with this folder; a second one refuses to start |
| `baselines/<job>.json` | two-way memory: the two folders it belongs to, the markers' tokens and counter, and how each side looked at the last sync |
| `landings/<job>.json` | Mirror and Backup: copies whose dates didn't stick on the destination, with the folders and destination marker they belong to |
| `unattended/<job>.json` | full automation: what automatic runs removed or replaced since the last confirmed list, within *Keep deleted files* days |
| `logs/<job>.log` | each job's activity log, cut to its newest half when it grows past 512 KB |

The state file, the memories, the landings and the counts are saved under a temporary name next
to them (`.<name>.<process>-<n>.tmp`) and renamed into place, so a crash never leaves one
half-written. The job logs are added to as they go.

The app's own log is `charon.log` (`charon-dev.log` for a development build), started afresh at
5 MB with a few older ones kept: in `~/Library/Logs/dev.stean.charon/` on macOS,
`%LOCALAPPDATA%\dev.stean.charon\logs\` on Windows, and `~/.local/share/dev.stean.charon/logs/` on
Linux (next to the job logs).

**From 0.1.0.** Charon 0.1.0 used the identifier `ninja.digifix.charon`. The first time an
installed 0.1.1 or later starts, it moves that folder's items into the new data folder when the
new one holds no jobs yet and the old one does. Nothing already there is replaced, and the
files with the jobs move last, so a move cut off halfway is finished at the next start. The old
folder is removed once it's empty.

**From 0.2.** Charon 0.2 kept its settings and jobs in `charon.json`. When `charon-state.json`
doesn't exist yet, 0.3 takes them over: every job comes over with full automation off and its
next sync waiting for the person's OK. Jobs that used to sync by themselves are listed once in
*What changed in Charon 0.3.0* and in their logs. The old file is renamed to
`charon.json.moved-to-0.3`, so an old copy of the app started by mistake finds no jobs to run. A
`charon.json` that's damaged is left where it is, and Charon starts without jobs. Two-way
memories saved by 0.2.0 and earlier don't say which folders they belong to, so they're never
used: the first sync with 0.3 compares both folders afresh and removes nothing.

In each synced folder, Charon keeps:

- `.charon/<job>.json`: the job's marker, a random token and a counter. Two-way jobs keep one in
  both folders; Mirror and Backup in the destination, and in the source where Charon can write
  there. Removing a job removes its marker.
- `.charon-trash/<date>_<time>_<id>/`: what a run removed or replaced, with its `.charon` list.
- `.charon-<16 hex digits>.part`: a copy on its way, or an original Charon renamed aside for a
  moment while replacing or removing it. After an interrupted sync, such a file may be the only
  copy of a version. A run that goes all the way moves it into `.charon-trash` once it's an hour
  old (see *Leftovers* above), so don't delete these files by hand.

On Windows, Charon marks `.charon` and `.charon-trash` as hidden; elsewhere the dot hides them.

## Changing the About information

The app's website, the source code link, the developer's name, website and e-mail, the licence
name and the copyright year are constants in `src-tauri/src/about.rs`; name and version come from
the app's package information. The About panel's copyright line is built from `COPYRIGHT_YEAR`
and `DEVELOPER`, so keep those in step with the copyright line in `LICENSE` and the README. The
window may only open the app's website, the source code (releases included), the developer's
website and an e-mail to the developer (`About::may_open`), so a new link needs a constant there
too. The licence text is in `LICENSE`.

## How the update check works

`src-tauri/src/updates.rs` reads `https://api.github.com/repos/steanart/charon/releases/latest`
ten seconds after start-up and then once a day while Charon runs, each time only if *Look for
new versions* is on in Settings. *Check for updates* in the About panel and *Check for Updates…*
in the tray menu ask at any time. GitHub's latest release never includes drafts or pre-releases,
so a new version reaches people only once its release is **published**. The tag (`v0.3.0`) is
compared with the app's own version; a tag that isn't a version never counts as newer.

When a newer version is out, the title bar shows an *Update* button, the tray item becomes
*Download Charon <version>…*, and a background check shows one notification per version (with
*Notifications* on). Charon installs nothing by itself: the link is the release asset with the
fixed name for this computer (`Charon-macOS-Apple-Silicon.dmg`, `Charon-macOS-Intel.dmg`,
`Charon-Windows-Setup.exe`; on Linux the release page). Keep those names in `build.yml` and
`updates.rs` the same.
