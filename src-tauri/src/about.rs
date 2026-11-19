//! Facts shown in the About panel. Change them here (and in LICENSE) when needed.

use serde::Serialize;
use tauri::AppHandle;

/// The app's own website.
pub const APP_WEBSITE: &str = "https://charonsync.app";
/// Public source code and releases on GitHub.
pub const SOURCE_CODE: &str = "https://github.com/digifix-ninja/charon";
/// Who makes Charon (the legal name, used in the copyright line), the name its website goes
/// by, the website itself and how to write to them.
pub const DEVELOPER: &str = "DigiFix S.à r.l.-S.";
pub const DEVELOPER_BRAND: &str = "DigiFix.Ninja";
pub const DEVELOPER_WEBSITE: &str = "https://digifix.ninja";
pub const DEVELOPER_EMAIL: &str = "dojo@digifix.ninja";
pub const LICENCE: &str = "MIT";
pub const COPYRIGHT_YEAR: &str = "2026";

/// Everything the About panel shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct About {
    name: String,
    version: String,
    app_website: &'static str,
    source_code: &'static str,
    developer: &'static str,
    developer_brand: &'static str,
    developer_website: &'static str,
    developer_email: &'static str,
    licence: &'static str,
    copyright_year: &'static str,
}

impl About {
    /// Name and version come from the app bundle, the rest from the constants above.
    pub fn of(app: &AppHandle) -> Self {
        let info = app.package_info();
        Self {
            name: info.name.clone(),
            version: info.version.to_string(),
            app_website: APP_WEBSITE,
            source_code: SOURCE_CODE,
            developer: DEVELOPER,
            developer_brand: DEVELOPER_BRAND,
            developer_website: DEVELOPER_WEBSITE,
            developer_email: DEVELOPER_EMAIL,
            licence: LICENCE,
            copyright_year: COPYRIGHT_YEAR,
        }
    }

    /// Links the app may open: its website, GitHub (releases included), the developer's
    /// website, and an e-mail to the developer.
    pub fn may_open(url: &str) -> bool {
        if url == format!("mailto:{DEVELOPER_EMAIL}") {
            return true;
        }
        [APP_WEBSITE, SOURCE_CODE, DEVELOPER_WEBSITE]
            .iter()
            .any(|site| url == *site || url.starts_with(&format!("{site}/")))
    }
}
