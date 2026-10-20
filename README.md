<p align="center">
  <img src="ui/img/logo-light.svg#gh-light-mode-only" width="200" alt="Charon: a boat with a lantern, carrying a folder">
  <img src="ui/img/logo-dark.svg#gh-dark-mode-only" width="200" alt="Charon: a boat with a lantern, carrying a folder">
</p>

<h1 align="center">Charon</h1>

<p align="center">
  <strong>The ferryman for your files.</strong><br>
  Sync jobs between folders and drives: mirror, backup or two-way. Charon shows you every
  change it is about to make, and by default carries nothing across until you say OK.
</p>

<p align="center">
  <a href="../../releases/latest">Download</a> ·
  <a href="https://charonsync.app">Website</a> ·
  <a href="docs/DEVELOPMENT.md">Build from source</a>
</p>

<p align="center">
  <img alt="macOS" src="https://img.shields.io/badge/macOS-11%2B-111?logo=apple">
  <img alt="Windows" src="https://img.shields.io/badge/Windows-10%2F11-111?logo=windows">
  <img alt="Linux" src="https://img.shields.io/badge/Linux-deb%20·%20rpm%20·%20AppImage-111?logo=linux&logoColor=white">
  <img alt="Licence: MIT" src="https://img.shields.io/badge/licence-MIT-111">
</p>

<img src="docs/screenshot.png" alt="Charon showing sync jobs: one in sync, one syncing with a progress bar, one with changes waiting and one with conflicts">

---

## What it does

A **sync job** connects two folders: a source and a destination. Each job is one row in
Charon's window, with its status, a progress bar while it runs, when it last synced and what's
waiting, a *Review and sync* button (*Sync now* once a job may sync by itself, *Stop* while it
runs), and buttons to open either folder or pause the job.

**Three ways to cross**

| Mode | What happens |
|---|---|
| **Mirror** | The destination becomes a copy of the source, apart from what's excluded, unreadable or listed as not synced. Files removed from the source are removed there too. |
| **Backup** | New and changed files are copied. A file in the destination that differs from the source's, even one edited there, is replaced by the source's version. Files removed from the source stay in the destination. |
| **Two-way** | Changes flow both ways: a file you delete on one side is removed on the other too. Charon remembers the last synced state, so it can tell "deleted here" from "new there". The first two-way sync of two folders removes nothing. |

**Every sync waits for your OK**

By default Charon never changes your files by itself. When a job is due, it looks at both
folders and counts what would change. If anything would, the row says *Needs your OK*, a
notification tells you (if *Notifications* is on in Settings), and nothing crosses until you
press *Review and sync*, which makes the list afresh, and confirm it. A new job shows its first
list as soon as you save it.

**Four ways to start**

Each job chooses when it's due. Its row says when Charon looks next (*looks at 14:00 and asks
you*), and its *Details* tab says what happens each time (*Looks every hour and asks you*).
Charon also looks when it notices changes, and every 15 minutes by default (*Look for changes*
in Settings). Looking never changes your files.

- **Manual:** only when you press *Review and sync* (or *Sync now*, once a job may sync by itself).
- **On change:** a few seconds after edits in the folders settle down (you choose how long).
- **Every… / Daily:** every *N* minutes, or every day at a set time.
- **Drive connected:** plug in the backup drive, and Charon looks at once and shows what waits,
  so you can confirm it before you eject the drive.

**Full automation, only if you want it**

A job can sync by itself instead of asking: turn on **Full automation** in its settings. Two
warnings come first (*Full automation can lose data*, then a *Last warning* with a tick): a
wrong folder or a damaged file is carried over before you see it, so it is only for people who
know exactly what it does.

The job then syncs by itself only after one more list you confirm. It asks again after a
*Stop*, after you change what it syncs, and whenever the guard below holds a run back. Changing
the mode, turning safe deletes off or swapping a Mirror or Backup job's folders turns full
automation off again, so you read the warnings anew.

Such jobs carry a red *Full auto* tag, and once they may sync by themselves their row says when
(*next sync 14:00*).

## Status at a glance

Every row uses a colour *and* an icon, so it still reads for colour-blind eyes.

| Status | Colour and icon | Meaning |
|---|---|---|
| In sync | green, a tick | The last look found nothing to carry, apart from what the row's flags say couldn't be read or isn't synced. |
| Syncing | blue, turning arrows | Files are crossing right now. |
| Needs your OK | pink, a checklist | Changes wait for you to look at the list and confirm it, or a full-automation run was held back for your OK. |
| Ready | cyan, a spark | A new job that hasn't synced yet. Nothing crosses until you confirm its first list. |
| Looking | no colour yet, a magnifying glass | Charon hasn't finished a new look at this job yet, so it doesn't claim the folders match. |
| Pending | amber, an up arrow | Full automation only: changes were found and cross at the job's next run. |
| Paused | grey, two bars | Doesn't sync by itself. Charon still looks and shows what waits, and you can still sync it from its row. |
| Conflict | violet, a fork | A file changed on both sides and waits for you to choose which version to keep. |
| Error | red, an exclamation mark | The last look or sync failed, or some files couldn't be copied. The row says why. |
| Offline | faded grey, a crossed-out plug | A folder or its drive isn't reachable, or the folder is now on another drive than at the last sync. |

## Built to be trusted

- **A list before every sync.** What changes on each side (new, replaced, removed), whole
  folders that go, conflicts, old trash to be emptied, and *Notes* on every place Charon
  couldn't read or won't sync, with the reason. *Confirm* (never the Enter key) names what it
  removes or replaces, *Confirm: copy 23, replace 2, remove 3*, and carries out exactly that list.
- **Warnings that need a tick.** A list that removes or replaces much of one side, replaces
  files with empty ones, follows a folder that suddenly looks empty or finds a folder on another
  drive needs a tick before *Confirm*. So does one that removes files (in a Mirror or Backup
  job, also one that replaces them) with safe deletes off. If one side is empty and the sync
  would remove files on the other, Charon refuses.
- **An out-of-date list can't be confirmed.** A list is good for 15 minutes, and only until
  Charon sees the folders, the drives or the job's settings change. During the sync each step
  checks the disk again: a file edited after the list was made is skipped, never overwritten.
- **Nothing unreadable becomes a removal.** A place Charon can't read (no permission, a folder
  macOS protects, a cloud folder that isn't downloaded) is left alone on both sides with
  everything inside it, and listed in the job's *Couldn't read* tab.
- **Excluded items are never deleted.** What matches the job's *Leave out* patterns is never
  copied or removed, and a folder that still holds such an item stays. Only system clutter like
  `.DS_Store` goes along with a removed folder, into `.charon-trash`. A new job starts with a few
  patterns: system files like `.DS_Store` and `Thumbs.db`, Office lock files (`~$*`) and
  `*.tmp`. Remove any you want synced.
- **A guard for full automation.** A run nobody looks at is held for your OK when it would
  remove or replace too much of one side, replace files with empty ones or follow a folder that
  suddenly looks empty, or when a drive, a marker or the room on a drive isn't right. Too much
  means half of a side's files removed (3 or more), half of them removed or replaced (5 or
  more), more than a quarter of them (21 or more), or over 1,000.
- **Slow damage counts too.** The guard also adds up what automatic runs removed or replaced
  within the last day (held at the same limits for removing or replacing) and since you last
  confirmed a list (held at more than 20 files that are more than a quarter of a side, counting
  back as far as Settings keeps deleted files). Runs nobody looked at never empty
  `.charon-trash`, so with safe deletes on, the originals are still there when the guard steps in.
- **Safe deletes.** Removed and replaced files go to a hidden `.charon-trash` folder at the top
  of the synced folder (so on the same drive), one dated folder per sync, for 30 days by default
  (0 in Settings keeps them until you empty it). Two-way jobs, *Newest wins* and settling a
  conflict keep replaced versions there even with safe deletes off. Only a sync you confirmed,
  whose list said so, empties old trash, and never a file that changed after it went in (an app
  may still be saving into it).
- **No half-written files.** Each file is copied under a temporary name next to its target,
  flushed to the drive, and only then takes its real name, with its original modification time.
  A replaced file is swapped out or moved aside, and checked: if an app saved a new version at
  that moment, that version stays. A source that changes while it's copied waits for the next sync.
- **The right folders on the right drives.** Charon leaves a small marker (a `.charon` folder)
  in the folders it syncs and remembers which drive each was on. A folder now on another drive
  (the empty folder an unplugged drive leaves behind) shows as *Offline*. A missing or older
  marker (another disk, a copy restored from a backup) makes a two-way job compare afresh and
  remove nothing. In a Mirror or Backup job, a missing or foreign marker on either side, or a
  source older than at the last sync, needs a tick before *Confirm* and holds full automation
  back. If a drive goes away mid-sync, the sync stops.
- **Room to spare, in a safe order.** Charon keeps 2% of the receiving drive free (at most 2 GB),
  and the list warns when a sync won't fit. New folders and copies go first and removals last
  (only a removal that clears the way for an item of another kind comes first), and when a copy
  to one side fails, that side's other removals wait for the next sync.
- **App libraries and databases stay whole.** A Photos or Final Cut library, a Logic project or
  a Pages document that changed on both sides of a two-way job becomes one conflict, kept whole
  from the side you choose. An SQLite database and its journal files are put in place together.
- **When a file changed on both sides.** Two-way jobs choose *Keep both*, *Newest wins* or
  *Ask me*, and an edit always beats a deletion. *Newest wins* decides only when the times can
  be trusted (not on FAT, exFAT or network drives, not whole hours apart, not a time that went
  backwards); otherwise both versions are kept. When the dates can't tell (no record of the
  last sync, both sides changed, or a date that only moved by a rounding), two-way jobs compare
  files of the same size byte by byte: identical ones aren't copied, and look-alikes that differ
  inside become a conflict. A look in the background reads at most 1 GB this way; the list and
  the sync read them all. Mirror and Backup go by size and modification time only.
- **Crashes and quitting.** Quitting lets a sync stop between two files and records what it
  carried. If Charon or the computer stops mid-sync, a two-way job's next look compares both
  folders afresh and removes nothing. After your *Stop*, the next sync waits for your OK.

## Good to know

- **Full automation trades the list for speed.** The guard catches big and slowly spreading
  damage, but a smaller mistake (a few files overwritten by a broken app) is carried over before
  you see it. With safe deletes on, the originals stay in `.charon-trash`; with safe deletes
  off, removed files (and in Mirror and Backup, replaced ones) are gone at once.
- **A two-way job compares afresh** the first time, after you change its folders or its mode,
  and whenever it can't trust its memory of the last sync. It then removes nothing, so files
  deleted on one side since may come back, and files that differ are kept twice or left for you.
  With *Newest wins*, the newer of two versions still replaces the older one, which goes to
  `.charon-trash`.
- **FAT and exFAT drives** keep times in local time and less precisely; Charon allows for that,
  and *Newest wins* keeps both versions there.
- **What a drive can't hold** is left out, with a note in the job's *Not synced* tab: on
  Windows and on FAT, exFAT and NTFS drives and SMB shares, names with characters like `:` `?`
  `*`, a trailing dot or space, or names like `CON`; on any drive, names too long for it; on
  FAT32, files over 4 GB; and links on a drive that holds none. Two names that differ only in
  how Unicode writes them, or only in case where a drive ignores case, are both left alone.
- **Never synced:** sockets, pipes and devices, names that aren't valid Unicode, the folders a
  drive keeps for itself (like `lost+found`) at the top of a synced folder, and Charon's own
  `.charon`, `.charon-trash` and half-copied files. The list's *Notes* name most of them.
- **Network shares and drives run by a program** (SMB, NFS, AFP, sshfs, rclone and other FUSE
  drives) may show changes late: there, full automation follows an item that went missing only
  once it has been missing for ten minutes while Charon runs. Cloud folders on the computer's
  own drive (iCloud Drive, Dropbox, OneDrive) don't count as such drives.
- **Files only in the cloud** (iCloud Drive, OneDrive, Dropbox on macOS and Windows) are left
  alone when Charon looks, and it says so. On a Mac Charon never downloads one; on Windows, a
  file that goes back to the cloud after the list was made may be downloaded when it's copied.
- **What travels with a file:** contents, modification time, and where the drive takes them,
  access time and permissions (creation time on macOS and Windows). On a Mac also Finder tags,
  Finder info and resource forks; on Linux, `user.` extended attributes. Owners, ACLs, other
  extended attributes and Windows alternate data streams are not copied, and a change to only
  tags or permissions doesn't make a file sync. FAT and exFAT keep no tags (a resource fork still
  travels), and the run's summary says so. On a Mac, `._` files are never synced on their own.
  Folders are created fresh: their own dates, permissions and tags aren't copied.
- **Close the app** that uses a library or database before you confirm a list that changes it
  (the list reminds you). A file another app won't let Charon read fails, with the reason in the
  job's activity log, and is tried again at the next sync.
- **Links** are copied as links on macOS and Linux, never followed; on Windows, links and
  junctions are left alone. When a link takes the place of a file or folder, Mirror and Backup
  keep the file or folder on the other side, with a note, and a two-way job asks you. A drive
  mounted inside a synced folder isn't crossed into.

## More

- Lives in the **menu bar** or system tray, with the overall status, *Sync All…* (it starts the
  jobs with full automation that may sync by themselves and shows the others' lists; paused jobs
  and jobs whose folders aren't there are left out), *Review Waiting Jobs…* and *Pause
  Everything*.
- **Notifications** when changes wait for your OK (a reminder after two hours, then once a day),
  when a full-automation run is held back, and for errors, conflicts and big syncs.
- **Launch at login** (off by default). With *Keep running when the window is closed* on (the
  default), Charon keeps looking for changes from the menu bar or tray after you close the window.
- **Tells you when a new version is out**: it asks GitHub at start-up and once a day, and an
  *Update* button takes you to the installer for your computer (on Linux, to the release page,
  where you choose a package). Nothing is downloaded without you.
- A **per-job activity log**: what each run did, and which files failed or were left out and
  why (up to 50 of each per run).
- **macOS, Windows and Linux.**

## Install

Download the latest version for your system from the [**Releases**](../../releases/latest) page:

| System | File |
|---|---|
| macOS (Apple Silicon) | `Charon-macOS-Apple-Silicon.dmg` |
| macOS (Intel) | `Charon-macOS-Intel.dmg` |
| Windows | `Charon-Windows-Setup.exe` or `Charon-Windows.msi` |
| Linux | `Charon-Linux.AppImage` (any distribution), `.deb` (Debian, Ubuntu) or `.rpm` (Fedora, openSUSE) |

> **First start:** the macOS version is signed and notarised by Apple. The Windows installers
> aren't signed yet: choose **More info → Run anyway** if SmartScreen asks.
> On Linux, the tray icon needs AppIndicator support (on GNOME: the "AppIndicator" extension).
> Without a tray, Charon still works, and closing its window quits it.

## Upgrading from 0.2

Charon 0.3 takes over your jobs and settings the first time it starts. What you will notice:

- **Every job now waits for your OK.** A sheet, *What changed in Charon 0.3.0*, lists the jobs
  that used to sync by themselves: keep each one asking, or turn on full automation.
- **A two-way job's first sync compares both folders afresh**, because 0.2's memory of the last
  sync can't be fully trusted. It removes nothing, so files deleted since then may come back;
  with *Newest wins*, the newer of two versions still replaces the older one.
- **Symbolic links are now copied as links** on macOS and Linux, so a first list may include
  links 0.2 left out.
- **Files ending in `.charon-partial`** (0.2's half-copied files) go to `.charon-trash` only from
  a list that names them. If one of them is yours, rename it before you confirm.
- **The old settings file becomes `charon.json.moved-to-0.3`**, so a copy of 0.2 started by
  mistake finds no jobs to run.

## Privacy

Charon works on your computer. No accounts, no tracking, no analytics. The only time it goes
online is to ask GitHub whether a new version is out, which you can turn off (*Look for new
versions* in Settings). Your jobs and settings, what each job remembers of its last sync and
the activity logs stay in the app's data folder on your computer. In your synced folders,
Charon adds its `.charon` marker and its `.charon-trash`.

## Build from source

Charon is written in Rust with [Tauri](https://tauri.app); the interface is plain HTML, CSS and
JavaScript, with no Node.js needed.

```sh
cargo install tauri-cli --version "^2" --locked
cargo tauri dev       # run it
cargo tauri build     # make installers for your system
```

See [**docs/DEVELOPMENT.md**](docs/DEVELOPMENT.md) for requirements per system, the project
layout and how the sync engine decides what to do.

## Contributing

Bug reports, ideas and pull requests are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md), and
please open an issue first for bigger changes.

## Licence

[MIT](LICENSE) © 2026 [stean.ART](https://stean.art)
