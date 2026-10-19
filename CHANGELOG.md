# Changelog

All notable changes to Charon are listed here.

## 0.2.0

- **Updates:** Charon asks GitHub at start-up and once a day whether a newer version is out. When one is, an **Update** button glows in the title bar, the menu bar / tray menu offers the download, and a notification says so (once per version). The download opens the installer for your computer. You can also check by hand from About or the tray menu ("Check for Updates…"), or turn the daily check off in Settings.
- **About** now lists the app's website ([charonsync.app](https://charonsync.app)) and GitHub page, and the developer, **stean.DEV**, with its website ([stean.dev](https://stean.dev)) and e-mail ([a.s@stean.dev](mailto:a.s@stean.dev)).

## 0.1.1

- Charon is now a stean.ART project: credits, copyright and the About panel point to [stean.art](https://stean.art).
- Links point to the new GitHub home, [github.com/steanart/charon](https://github.com/steanart/charon).
- The app identifier is now `dev.stean.charon`. Jobs and settings from 0.1.0 are moved over the first time 0.1.1 starts.

## 0.1.0 — first crossing

- Sync jobs with a source and a destination: **Mirror**, **Backup** and **Two-way**.
- Triggers: manual, on change, every *N* minutes, daily at a time, and when a drive is connected.
- Dry-run preview before syncing; new jobs open it by themselves.
- Safe deletes into `.charon-trash`, cleared after a configurable number of days.
- Two-way conflicts: keep both, newest wins, or ask.
- Exclude filters with sensible defaults.
- Job rows with status colours and icons, live progress, and an activity log per job.
- Menu bar / system tray icon, notifications, launch at login, pause everything.
- macOS, Windows and Linux.
