// Crumb shell: tray icon, popover window, Rust runtime, SQLite,
// and the IPC surface for the React frontend.

mod ai;
mod commands;
mod db;
mod discord;
mod events;
mod runtime;
mod settings;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use tauri::{
    image::Image,
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, LogicalPosition, LogicalSize, Manager, PhysicalPosition, Position, Rect, Size,
    WindowEvent,
};
use tracing_subscriber::EnvFilter;

/// Shared flag that lets commands ask the popover focus-loss handler to skip
/// the next hide. Used by `open_action_source_in_discord` so the popover can
/// stay visible while Discord steals focus.
#[derive(Clone, Default)]
pub struct PopoverHideGuard(Arc<AtomicBool>);

impl PopoverHideGuard {
    pub fn suppress_next(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn take(&self) -> bool {
        self.0.swap(false, Ordering::SeqCst)
    }
}

const POPOVER_WIDTH: f64 = 380.0;
const POPOVER_HEIGHT: f64 = 520.0;
const POPOVER_TRAY_GAP: f64 = 4.0;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,crumb=debug")),
        )
        .with_writer(std::io::stderr)
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![
            commands::list_scrapes,
            commands::get_scrape,
            commands::delete_source,
            commands::open_action_source_in_discord,
            commands::list_action_items,
            commands::set_action_item_status,
            commands::set_action_item_assignee,
            commands::get_sidecar_status,
            commands::hide_popover,
            commands::get_app_settings,
            commands::save_app_settings,
            commands::test_discord_settings,
            commands::test_ai_settings,
            commands::open_settings_window,
            commands::get_launch_at_login,
            commands::set_launch_at_login,
        ])
        .setup(|app| {
            // Menubar app: no Dock icon, no menu bar.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let db_path = db::resolve_db_path(&app.handle())?;
            let database = db::Db::open(&db_path)?;
            app.manage(database.clone());

            let runtime = runtime::RuntimeManager::start(app.handle().clone(), database)?;
            app.manage(runtime);

            app.manage(PopoverHideGuard::default());

            let tray_menu = MenuBuilder::new(app)
                .items(&[
                    &MenuItemBuilder::with_id("show", "Show Crumb").build(app)?,
                    &MenuItemBuilder::with_id("settings", "Settings...").build(app)?,
                    &MenuItemBuilder::with_id("quit", "Quit Crumb").build(app)?,
                ])
                .build()?;

            let tray_icon = Image::from_bytes(include_bytes!("../icons/tray.png"))?;
            TrayIconBuilder::with_id("main")
                .icon(tray_icon)
                .icon_as_template(true)
                .menu(&tray_menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => {
                        let _ = show_popover_centered(app);
                    }
                    "settings" => {
                        let _ = show_settings_window(app);
                    }
                    "quit" => {
                        graceful_exit(app);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        position,
                        rect,
                        ..
                    } = event
                    {
                        let app = tray.app_handle().clone();
                        let _ = toggle_popover(&app, position, rect);
                    }
                })
                .build(app)?;

            // Hide popover on focus loss so it behaves like a real menubar dropdown.
            if let Some(win) = app.get_webview_window("popover") {
                let win_clone = win.clone();
                let guard = app.state::<PopoverHideGuard>().inner().clone();
                win.on_window_event(move |ev| {
                    if let WindowEvent::Focused(false) = ev {
                        if guard.take() {
                            return;
                        }
                        let _ = win_clone.hide();
                    }
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if matches!(event, WindowEvent::CloseRequested { .. }) {
                let _ = window.hide();
                if window.label() == "settings" {
                    let _ = hide_app_from_dock_and_switcher(window.app_handle());
                }
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn toggle_popover(
    app: &AppHandle,
    _click_pos: PhysicalPosition<f64>,
    rect: Rect,
) -> tauri::Result<()> {
    let Some(win) = app.get_webview_window("popover") else {
        return Ok(());
    };
    if win.is_visible().unwrap_or(false) {
        win.hide()?;
        return Ok(());
    }

    // The tray rect is reported in physical pixels using the tray item's display scale.
    // The hidden popover may still have the scale factor from a previous monitor.
    let scale = tray_event_scale_factor(win.scale_factor().unwrap_or(1.0));
    let (icon_x, icon_y) = rect_position_to_logical(rect.position, scale);
    let (icon_w, icon_h) = rect_size_to_logical(rect.size, scale);

    let icon_center_x = icon_x + icon_w / 2.0;
    let icon_bottom_y = icon_y + icon_h;

    let target_x = icon_center_x - POPOVER_WIDTH / 2.0;
    let target_y = icon_bottom_y + POPOVER_TRAY_GAP;

    let logical = LogicalPosition::new(target_x, target_y);
    win.set_size(LogicalSize::new(POPOVER_WIDTH, POPOVER_HEIGHT))?;
    win.set_position(logical)?;
    win.show()?;
    win.set_focus()?;
    Ok(())
}

fn tray_event_scale_factor(fallback_scale: f64) -> f64 {
    current_mouse_screen_scale_factor().unwrap_or_else(|| normalized_scale(fallback_scale))
}

fn rect_position_to_logical(position: Position, scale: f64) -> (f64, f64) {
    let scale = normalized_scale(scale);
    match position {
        Position::Physical(p) => (p.x as f64 / scale, p.y as f64 / scale),
        Position::Logical(p) => (p.x, p.y),
    }
}

fn rect_size_to_logical(size: Size, scale: f64) -> (f64, f64) {
    let scale = normalized_scale(scale);
    match size {
        Size::Physical(s) => (s.width as f64 / scale, s.height as f64 / scale),
        Size::Logical(s) => (s.width, s.height),
    }
}

fn normalized_scale(scale: f64) -> f64 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

#[cfg(target_os = "macos")]
fn current_mouse_screen_scale_factor() -> Option<f64> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSEvent, NSScreen};

    let mtm = MainThreadMarker::new()?;
    let mouse = NSEvent::mouseLocation();
    let screens = NSScreen::screens(mtm);

    for screen in screens.iter() {
        let frame = screen.frame();
        let contains_x = mouse.x >= frame.origin.x && mouse.x < frame.origin.x + frame.size.width;
        let contains_y = mouse.y >= frame.origin.y && mouse.y < frame.origin.y + frame.size.height;

        if contains_x && contains_y {
            return Some(normalized_scale(screen.backingScaleFactor()));
        }
    }

    NSScreen::mainScreen(mtm).map(|screen| normalized_scale(screen.backingScaleFactor()))
}

#[cfg(not(target_os = "macos"))]
fn current_mouse_screen_scale_factor() -> Option<f64> {
    None
}

fn show_popover_centered(app: &AppHandle) -> tauri::Result<()> {
    let Some(win) = app.get_webview_window("popover") else {
        return Ok(());
    };
    win.center()?;
    win.show()?;
    win.set_focus()?;
    Ok(())
}

pub fn show_settings_window(app: &AppHandle) -> tauri::Result<()> {
    let Some(win) = app.get_webview_window("settings") else {
        return Ok(());
    };
    show_app_in_dock_and_switcher(app)?;
    win.center()?;
    win.show()?;
    win.set_focus()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn show_app_in_dock_and_switcher(app: &AppHandle) -> tauri::Result<()> {
    app.set_activation_policy(tauri::ActivationPolicy::Regular)?;
    app.set_dock_visibility(true)?;
    app.show()?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn show_app_in_dock_and_switcher(_app: &AppHandle) -> tauri::Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn hide_app_from_dock_and_switcher(app: &AppHandle) -> tauri::Result<()> {
    app.set_activation_policy(tauri::ActivationPolicy::Accessory)?;
    app.set_dock_visibility(false)?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn hide_app_from_dock_and_switcher(_app: &AppHandle) -> tauri::Result<()> {
    Ok(())
}

fn graceful_exit(app: &AppHandle) {
    if let Some(runtime) = app.try_state::<runtime::RuntimeManager>() {
        let runtime = runtime.inner().clone();
        tauri::async_runtime::spawn(async move { runtime.shutdown().await });
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        app.exit(0);
    });
}
