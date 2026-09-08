use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};

use crate::AppState;
use crate::arming::ArmingOverride;

const MENU_OPEN_SETTINGS: &str = "open-settings";
const MENU_ARM_UNTIL_NIGHT_ENDS: &str = "arm-until-night-ends";
const MENU_DISARM_UNTIL_NEXT_NIGHT: &str = "disarm-until-next-night";
const MENU_CLEAR_OVERRIDE: &str = "clear-override";
const MENU_QUIT: &str = "quit";

pub(crate) struct TrayHandles {
    pub armed_item: MenuItem<Wry>,
    pub supervisor_item: MenuItem<Wry>,
    pub timecode_item: MenuItem<Wry>,
}

pub(crate) fn build(app: &AppHandle) -> tauri::Result<TrayHandles> {
    let armed_item = MenuItem::with_id(
        app,
        "arming-state",
        "Reading arming state…",
        false,
        None::<&str>,
    )?;
    let supervisor_item = MenuItem::with_id(
        app,
        "supervisor-state",
        "Reading Resolume supervision state…",
        false,
        None::<&str>,
    )?;
    let timecode_item = MenuItem::with_id(
        app,
        "timecode-state",
        "Reading LTC observation state…",
        false,
        None::<&str>,
    )?;

    let open_settings_item =
        MenuItem::with_id(app, MENU_OPEN_SETTINGS, "Open settings", true, None::<&str>)?;
    let arm_item = MenuItem::with_id(
        app,
        MENU_ARM_UNTIL_NIGHT_ENDS,
        "Arm for this night",
        true,
        None::<&str>,
    )?;
    let disarm_item = MenuItem::with_id(
        app,
        MENU_DISARM_UNTIL_NEXT_NIGHT,
        "Disarm until next night",
        true,
        None::<&str>,
    )?;
    let clear_item = MenuItem::with_id(
        app,
        MENU_CLEAR_OVERRIDE,
        "Clear override",
        true,
        None::<&str>,
    )?;
    let quit_item = MenuItem::with_id(
        app,
        MENU_QUIT,
        "Quit ShowMesh Companion",
        true,
        None::<&str>,
    )?;

    let menu = Menu::with_items(
        app,
        &[
            &armed_item,
            &supervisor_item,
            &timecode_item,
            &PredefinedMenuItem::separator(app)?,
            &open_settings_item,
            &PredefinedMenuItem::separator(app)?,
            &arm_item,
            &disarm_item,
            &clear_item,
            &PredefinedMenuItem::separator(app)?,
            &quit_item,
        ],
    )?;

    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| tauri::Error::AssetNotFound("default window icon is missing".into()))?;

    TrayIconBuilder::new()
        .icon(icon)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .tooltip("ShowMesh Companion")
        .on_menu_event(handle_menu_event)
        .build(app)?;

    Ok(TrayHandles {
        armed_item,
        supervisor_item,
        timecode_item,
    })
}

fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    match event.id().as_ref() {
        MENU_OPEN_SETTINGS => open_settings(app),
        MENU_ARM_UNTIL_NIGHT_ENDS => apply_override(app, ArmingOverride::ArmUntilNightEnds),
        MENU_DISARM_UNTIL_NEXT_NIGHT => apply_override(app, ArmingOverride::DisarmUntilNextNight),
        MENU_CLEAR_OVERRIDE => apply_override(app, ArmingOverride::None),
        MENU_QUIT => app.exit(0),
        _ => {}
    }
}

fn apply_override(app: &AppHandle, value: ArmingOverride) {
    let state = app.state::<AppState>();
    let _ = state.apply_override(value);
}

pub(crate) fn open_settings(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.set_skip_taskbar(false);
    let _ = window.show();
    #[cfg(target_os = "macos")]
    let _ = app.show();
    let _ = window.set_focus();
}
