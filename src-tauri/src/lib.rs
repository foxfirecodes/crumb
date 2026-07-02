// Crumb shell: tray icon, popover window, Rust runtime, SQLite,
// and the IPC surface for the React frontend.

mod ai;
mod commands;
mod db;
mod discord;
mod events;
mod runtime;
mod settings;

use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
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

#[derive(Debug, Default)]
pub struct TrayUnreadState {
    inner: parking_lot::Mutex<TrayUnreadInner>,
}

#[derive(Debug, Default)]
struct TrayUnreadInner {
    initialized: bool,
    known_open_action_ids: HashSet<String>,
    has_unread_actions: bool,
}

impl TrayUnreadState {
    fn mark_seen(&self, open_action_ids: HashSet<String>) {
        let mut inner = self.inner.lock();
        inner.initialized = true;
        inner.known_open_action_ids = open_action_ids;
        inner.has_unread_actions = false;
    }

    fn clear_unread(&self) {
        let mut inner = self.inner.lock();
        inner.has_unread_actions = false;
    }

    fn observe_open_actions(
        &self,
        open_action_ids: HashSet<String>,
        mark_new_as_seen: bool,
    ) -> bool {
        let mut inner = self.inner.lock();
        if !inner.initialized || mark_new_as_seen {
            inner.initialized = true;
            inner.known_open_action_ids = open_action_ids;
            inner.has_unread_actions = false;
            return false;
        }

        if open_action_ids
            .iter()
            .any(|id| !inner.known_open_action_ids.contains(id))
        {
            inner.has_unread_actions = true;
        }
        inner.known_open_action_ids = open_action_ids;
        inner.has_unread_actions
    }
}

const POPOVER_WIDTH: f64 = 380.0;
const POPOVER_HEIGHT: f64 = 520.0;
const POPOVER_TRAY_GAP: f64 = 4.0;
const TRAY_ID: &str = "main";

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
        .plugin(tauri_plugin_clipboard_manager::init())
        .invoke_handler(tauri::generate_handler![
            commands::list_scrapes,
            commands::get_scrape,
            commands::delete_source,
            commands::open_action_source_in_discord,
            commands::list_action_items,
            commands::create_manual_action_item,
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

            let tray_unread_state = TrayUnreadState::default();
            match database.list_open_action_items() {
                Ok(actions) => {
                    tray_unread_state.mark_seen(action_ids(&actions));
                }
                Err(e) => {
                    tracing::warn!("failed to seed tray unread state: {e}");
                    tray_unread_state.mark_seen(HashSet::new());
                }
            }
            app.manage(tray_unread_state);

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

            let tray_icon = tray_icon_image()?;
            TrayIconBuilder::with_id(TRAY_ID)
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
            set_tray_unread_indicator(&app.handle(), false);

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
    mark_tray_actions_seen(app);
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
    mark_tray_actions_seen(app);
    Ok(())
}

pub fn observe_tray_action_items(app: &AppHandle, actions: &[events::CanonicalActionItem]) {
    let Some(state) = app.try_state::<TrayUnreadState>() else {
        return;
    };
    let has_unread = state.observe_open_actions(action_ids(actions), is_popover_visible(app));
    set_tray_unread_indicator(app, has_unread);
}

fn mark_tray_actions_seen(app: &AppHandle) {
    let Some(state) = app.try_state::<TrayUnreadState>() else {
        return;
    };
    match app.try_state::<db::Db>() {
        Some(db) => match db.list_open_action_items() {
            Ok(actions) => state.mark_seen(action_ids(&actions)),
            Err(e) => {
                tracing::warn!("failed to mark tray actions seen: {e}");
                state.clear_unread();
            }
        },
        None => state.clear_unread(),
    }
    set_tray_unread_indicator(app, false);
}

fn is_popover_visible(app: &AppHandle) -> bool {
    app.get_webview_window("popover")
        .and_then(|win| win.is_visible().ok())
        .unwrap_or(false)
}

fn action_ids(actions: &[events::CanonicalActionItem]) -> HashSet<String> {
    actions.iter().map(|action| action.id.clone()).collect()
}

fn set_tray_unread_indicator(app: &AppHandle, has_unread: bool) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let icon = if has_unread {
        unread_tray_icon_image()
    } else {
        tray_icon_image()
    };
    match icon {
        Ok(icon) => {
            if let Err(e) = tray.set_icon_with_as_template(Some(icon), true) {
                tracing::warn!("failed to update tray icon: {e}");
            }
        }
        Err(e) => tracing::warn!("failed to build tray icon: {e}"),
    }
    let tooltip = if has_unread {
        "Crumb - new action items"
    } else {
        "Crumb"
    };
    if let Err(e) = tray.set_tooltip(Some(tooltip)) {
        tracing::warn!("failed to update tray tooltip: {e}");
    }
}

fn tray_icon_image() -> tauri::Result<Image<'static>> {
    Image::from_bytes(include_bytes!("../icons/tray.png"))
}

fn unread_tray_icon_image() -> tauri::Result<Image<'static>> {
    let base = tray_icon_image()?;
    let width = base.width();
    let height = base.height();
    let mut rgba = base.rgba().to_vec();
    draw_unread_dot(&mut rgba, width, height);
    Ok(Image::new_owned(rgba, width, height))
}

fn draw_unread_dot(rgba: &mut [u8], width: u32, height: u32) {
    let scale = (width.min(height) as f32 / 44.0).max(0.5);
    let center_x = width as f32 - 8.0 * scale;
    let center_y = 8.0 * scale;
    let clear_radius = 7.6 * scale;
    let dot_radius = 5.6 * scale;

    for y in 0..height {
        for x in 0..width {
            let distance = pixel_distance(x, y, center_x, center_y);
            let idx = ((y * width + x) * 4) as usize;
            if distance <= clear_radius {
                let edge = ((distance - (clear_radius - scale)) / scale).clamp(0.0, 1.0);
                rgba[idx + 3] = (rgba[idx + 3] as f32 * edge).round() as u8;
            }
        }
    }

    for y in 0..height {
        for x in 0..width {
            let distance = pixel_distance(x, y, center_x, center_y);
            if distance > dot_radius {
                continue;
            }
            let edge = ((dot_radius - distance) / scale).clamp(0.0, 1.0);
            let idx = ((y * width + x) * 4) as usize;
            rgba[idx] = 0;
            rgba[idx + 1] = 0;
            rgba[idx + 2] = 0;
            rgba[idx + 3] = rgba[idx + 3].max((255.0 * edge).round() as u8);
        }
    }
}

fn pixel_distance(x: u32, y: u32, center_x: f32, center_y: f32) -> f32 {
    let dx = x as f32 + 0.5 - center_x;
    let dy = y as f32 + 0.5 - center_y;
    (dx * dx + dy * dy).sqrt()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(values: &[&str]) -> HashSet<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn tray_unread_state_seeds_without_marking_existing_actions_unread() {
        let state = TrayUnreadState::default();

        assert!(!state.observe_open_actions(ids(&["a", "b"]), false));
        assert!(!state.observe_open_actions(ids(&["a", "b"]), false));
    }

    #[test]
    fn tray_unread_state_marks_new_open_action_ids_unread() {
        let state = TrayUnreadState::default();

        state.mark_seen(ids(&["a"]));

        assert!(state.observe_open_actions(ids(&["a", "b"]), false));
        assert!(state.observe_open_actions(ids(&["b"]), false));
    }

    #[test]
    fn tray_unread_state_can_mark_current_actions_seen() {
        let state = TrayUnreadState::default();

        state.mark_seen(ids(&["a"]));
        assert!(state.observe_open_actions(ids(&["a", "b"]), false));

        state.mark_seen(ids(&["a", "b"]));
        assert!(!state.observe_open_actions(ids(&["a", "b"]), false));
    }

    #[test]
    fn tray_unread_state_treats_visible_popover_updates_as_seen() {
        let state = TrayUnreadState::default();

        state.mark_seen(ids(&["a"]));

        assert!(!state.observe_open_actions(ids(&["a", "b"]), true));
        assert!(!state.observe_open_actions(ids(&["a", "b"]), false));
    }
}
