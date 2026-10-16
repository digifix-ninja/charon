//! The menu-bar icon and its menu: the overall status at a glance, and the main actions.

use crate::state::AppState;
use charon_core::Overview;
use tauri::image::Image;
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};

/// macOS: black-on-transparent template, tinted by the system for light and dark menu bars.
#[cfg(target_os = "macos")]
const TRAY_ICON: &[u8] = include_bytes!("../icons/tray.png");
/// Windows and Linux: white boat, made for the usual dark taskbars and panels.
#[cfg(not(target_os = "macos"))]
const TRAY_ICON: &[u8] = include_bytes!("../icons/tray-white.png");

const TRAY_ID: &str = "charon";

/// The menu items whose text changes with the status.
pub struct Tray {
    status: MenuItem<Wry>,
    pause: MenuItem<Wry>,
}

impl Tray {
    /// Puts the icon in the menu bar (macOS) or system tray (Windows, Linux).
    pub fn install(app: &AppHandle, overview: &Overview) -> tauri::Result<()> {
        let status = MenuItem::with_id(app, "status", overview.headline(), false, None::<&str>)?;
        let pause = MenuItem::with_id(app, "pause", Self::pause_label(overview), true, None::<&str>)?;
        let menu = Menu::with_items(
            app,
            &[
                &status,
                &PredefinedMenuItem::separator(app)?,
                &MenuItem::with_id(app, "open", "Open Charon", true, None::<&str>)?,
                &MenuItem::with_id(app, "sync", "Sync All Now", true, None::<&str>)?,
                &pause,
                &PredefinedMenuItem::separator(app)?,
                &MenuItem::with_id(app, "quit", "Quit Charon", true, None::<&str>)?,
            ],
        )?;
        TrayIconBuilder::with_id(TRAY_ID)
            .icon(Image::from_bytes(TRAY_ICON)?)
            .icon_as_template(true)
            .tooltip(format!("Charon · {}", overview.headline()))
            .menu(&menu)
            .show_menu_on_left_click(true)
            .on_menu_event(Self::on_menu)
            .build(app)?;
        app.manage(Tray { status, pause });
        Ok(())
    }

    /// Updates the status line, the pause item and the tooltip.
    pub fn refresh(app: &AppHandle, overview: &Overview) {
        let Some(tray) = app.try_state::<Tray>() else { return };
        let _ = tray.status.set_text(overview.headline());
        let _ = tray.pause.set_text(Self::pause_label(overview));
        if let Some(icon) = app.tray_by_id(TRAY_ID) {
            let _ = icon.set_tooltip(Some(format!("Charon · {}", overview.headline())));
        }
    }

    fn pause_label(overview: &Overview) -> &'static str {
        if overview.paused_all {
            "Resume Everything"
        } else {
            "Pause Everything"
        }
    }

    /// Handles a click on one of the menu items.
    fn on_menu(app: &AppHandle, event: MenuEvent) {
        let engine = app.state::<AppState>().engine.clone();
        match event.id.as_ref() {
            "open" => crate::show_main_window(app),
            "sync" => engine.sync_all(),
            "pause" => {
                let paused = engine.settings().paused_all;
                let _ = engine.set_all_paused(!paused);
            }
            "quit" => app.exit(0),
            _ => {}
        }
    }
}
