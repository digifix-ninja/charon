//! Looking for a newer Charon on GitHub: shortly after start-up, once a day while it runs,
//! and whenever someone asks. Charon installs nothing by itself: it says that a new version
//! is out (in the window, the tray menu and a notification) and links to its installer.

use crate::state::AppState;
use crate::tray::Tray;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

/// GitHub's answer for the newest published release (drafts and pre-releases never show here).
const LATEST_RELEASE: &str = "https://api.github.com/repos/steanart/charon/releases/latest";
/// Gives the app a moment to settle before the first check.
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(10);
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
const TIMEOUT: Duration = Duration::from_secs(20);

/// The part of GitHub's release that Charon reads.
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// What the window, the tray and notifications are told.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub current: String,
    pub latest: String,
    /// The latest release is newer than this app.
    pub available: bool,
    /// The release page, with its notes and every installer.
    pub page: String,
    /// The installer for this computer, when the release has one.
    pub download: Option<String>,
}

impl UpdateInfo {
    /// Where "Download" goes: the installer for this computer, or else the release page.
    pub fn link(&self) -> &str {
        self.download.as_deref().unwrap_or(&self.page)
    }
}

/// Remembers the last answer, and which version a notification was already shown for.
#[derive(Default)]
pub struct Updates {
    last: Mutex<Option<UpdateInfo>>,
    announced: Mutex<Option<String>>,
}

impl Updates {
    /// Starts the background checks (each one skipped while the setting is off).
    pub fn start(app: &AppHandle) {
        app.manage(Updates::default());
        let app = app.clone();
        let spawned = std::thread::Builder::new().name("updates".into()).spawn(move || {
            std::thread::sleep(FIRST_CHECK_AFTER);
            loop {
                let wanted = app.try_state::<AppState>().is_some_and(|s| s.engine.settings().check_updates);
                if wanted {
                    match Self::check(&app) {
                        Ok(info) if info.available => Self::notify(&app, &info, false),
                        Ok(_) => {}
                        Err(e) => log::info!("update check: {e}"),
                    }
                }
                std::thread::sleep(CHECK_EVERY);
            }
        });
        if let Err(e) = spawned {
            log::warn!("couldn't start the update checks: {e}");
        }
    }

    /// The last answer from GitHub, if Charon has asked yet.
    pub fn last(app: &AppHandle) -> Option<UpdateInfo> {
        app.try_state::<Updates>()?.last.lock().ok()?.clone()
    }

    /// Asks GitHub now (blocking), then tells the window and the tray menu.
    pub fn check(app: &AppHandle) -> Result<UpdateInfo, String> {
        let current = app.package_info().version.clone();
        let info = Self::compare(&current, Self::fetch(&current)?);
        if let Some(updates) = app.try_state::<Updates>() {
            if let Ok(mut last) = updates.last.lock() {
                *last = Some(info.clone());
            }
        }
        Tray::show_update(app, &info);
        let _ = app.emit("update", &info);
        Ok(info)
    }

    /// A system notification about a new version. Background checks show it once per version
    /// and only with notifications on; `asked` (from the tray menu) always shows it.
    pub fn notify(app: &AppHandle, info: &UpdateInfo, asked: bool) {
        if !asked {
            let enabled = app.try_state::<AppState>().is_some_and(|s| s.engine.settings().notifications);
            let Some(updates) = app.try_state::<Updates>() else { return };
            let Ok(mut announced) = updates.announced.lock() else { return };
            if !enabled || announced.as_deref() == Some(info.latest.as_str()) {
                return;
            }
            *announced = Some(info.latest.clone());
        }
        Self::show(
            app,
            &format!("Charon {} is out", info.latest),
            &format!("You have {}. Open Charon and press Update to download the new version.", info.current),
        );
    }

    /// Shows a system notification.
    pub fn show(app: &AppHandle, title: &str, body: &str) {
        if let Err(e) = app.notification().builder().title(title).body(body).show() {
            log::warn!("couldn't show a notification: {e}");
        }
    }

    /// Reads the newest published release from GitHub.
    fn fetch(current: &Version) -> Result<Release, String> {
        let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(TIMEOUT)).build().into();
        agent
            .get(LATEST_RELEASE)
            .header("User-Agent", &format!("Charon/{current}"))
            .header("Accept", "application/vnd.github+json")
            .call()
            .map_err(|e| match e {
                ureq::Error::StatusCode(404) => "No version of Charon has been published on GitHub yet.".to_string(),
                ureq::Error::StatusCode(403 | 429) => "GitHub asked to wait a little. Try again later.".to_string(),
                other => format!("Couldn't reach GitHub: {other}"),
            })?
            .body_mut()
            .read_json::<Release>()
            .map_err(|e| format!("GitHub's answer couldn't be read: {e}"))
    }

    /// Compares the release with this app. A tag that isn't a version ("v1.2.3") never counts as newer.
    fn compare(current: &Version, release: Release) -> UpdateInfo {
        let tag = release.tag_name.trim();
        let latest = tag.strip_prefix('v').unwrap_or(tag);
        let available = Version::parse(latest).is_ok_and(|latest| latest > *current);
        let download = Self::installer_name().and_then(|name| {
            release.assets.iter().find(|a| a.name == name).map(|a| a.browser_download_url.clone())
        });
        UpdateInfo {
            current: current.to_string(),
            latest: latest.to_string(),
            available,
            page: release.html_url,
            download,
        }
    }

    /// The installer that fits this computer, by its fixed name on GitHub. Linux has several
    /// (AppImage, deb, rpm), so there the release page lets the person choose.
    fn installer_name() -> Option<&'static str> {
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Some("Charon-macOS-Apple-Silicon.dmg")
        } else if cfg!(target_os = "macos") {
            Some("Charon-macOS-Intel.dmg")
        } else if cfg!(windows) {
            Some("Charon-Windows-Setup.exe")
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str) -> Release {
        Release {
            tag_name: tag.into(),
            html_url: format!("https://github.com/steanart/charon/releases/tag/{tag}"),
            assets: vec![],
        }
    }

    #[test]
    fn newer_tags_are_updates() {
        let current = Version::new(0, 2, 0);
        assert!(Updates::compare(&current, release("v0.2.1")).available);
        assert!(Updates::compare(&current, release("v0.10.0")).available);
        assert!(!Updates::compare(&current, release("v0.2.0")).available);
        assert!(!Updates::compare(&current, release("v0.1.1")).available);
        assert!(!Updates::compare(&current, release("nightly")).available);
        assert_eq!(Updates::compare(&current, release("v0.3.0")).latest, "0.3.0");
    }
}
