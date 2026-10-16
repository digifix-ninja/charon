//! Launch at login (a LaunchAgent on macOS, the registry on Windows, an autostart entry
//! on Linux).

use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt;

pub struct LaunchAtLogin;

impl LaunchAtLogin {
    /// Makes the system's login items match the setting.
    pub fn apply(app: &AppHandle, enabled: bool) {
        let launcher = app.autolaunch();
        let already = launcher.is_enabled().unwrap_or(false);
        let result = match (enabled, already) {
            (true, false) => launcher.enable(),
            (false, true) => launcher.disable(),
            _ => Ok(()),
        };
        if let Err(e) = result {
            log::warn!("couldn't change launch at login: {e}");
        }
    }
}
