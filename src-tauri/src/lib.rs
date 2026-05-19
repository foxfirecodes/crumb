// Crumb shell: tray icon, popover window, Rust runtime, SQLite,
// and the IPC surface for the React frontend.

mod ai;
mod commands;
mod db;
mod discord;
mod env;
mod events;
mod runtime;

use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, LogicalPosition, LogicalSize, Manager, PhysicalPosition, Position, Rect, Size,
    WindowEvent,
};
use tracing_subscriber::EnvFilter;

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
        .invoke_handler(tauri::generate_handler![
            commands::list_scrapes,
            commands::get_scrape,
            commands::get_sidecar_status,
            commands::hide_popover,
        ])
        .setup(|app| {
            // Menubar app: no Dock icon, no menu bar.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let db_path = db::resolve_db_path(&app.handle())?;
            let database = db::Db::open(&db_path)?;
            app.manage(database.clone());

            let handle = runtime::spawn(app.handle().clone(), database)?;
            app.manage(handle);

            let tray_menu = MenuBuilder::new(app)
                .items(&[
                    &MenuItemBuilder::with_id("show", "Show Crumb").build(app)?,
                    &MenuItemBuilder::with_id("quit", "Quit Crumb").build(app)?,
                ])
                .build()?;

            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().cloned().unwrap())
                .icon_as_template(true)
                .menu(&tray_menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => {
                        let _ = show_popover_centered(app);
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
                win.on_window_event(move |ev| {
                    if let WindowEvent::Focused(false) = ev {
                        let _ = win_clone.hide();
                    }
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if matches!(event, WindowEvent::CloseRequested { .. }) {
                let _ = window.hide();
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
    click_pos: PhysicalPosition<f64>,
    rect: Rect,
) -> tauri::Result<()> {
    let Some(win) = app.get_webview_window("popover") else {
        return Ok(());
    };
    if win.is_visible().unwrap_or(false) {
        win.hide()?;
        return Ok(());
    }

    let scale = win.scale_factor().unwrap_or(1.0);
    let win_size = win
        .outer_size()
        .unwrap_or(tauri::PhysicalSize::new(380, 520));

    // Both fields of Rect are enums (Position::Physical | Logical, Size::*).
    // Normalize to physical pixels.
    let (icon_x, icon_y) = match rect.position {
        Position::Physical(p) => (p.x as f64, p.y as f64),
        Position::Logical(p) => (p.x * scale, p.y * scale),
    };
    let (icon_w, icon_h) = match rect.size {
        Size::Physical(s) => (s.width as f64, s.height as f64),
        Size::Logical(s) => (s.width * scale, s.height * scale),
    };

    let icon_center_x = icon_x + icon_w / 2.0;
    let icon_bottom_y = icon_y + icon_h;
    let _ = click_pos; // rect is more reliable than cursor position.

    let target_x = icon_center_x - (win_size.width as f64 / 2.0);
    let target_y = icon_bottom_y + 4.0;

    let logical = LogicalPosition::new(target_x / scale, target_y / scale);
    win.set_size(LogicalSize::new(380.0, 520.0))?;
    win.set_position(logical)?;
    win.show()?;
    win.set_focus()?;
    Ok(())
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

fn graceful_exit(app: &AppHandle) {
    if let Some(handle) = app.try_state::<runtime::RuntimeHandle>() {
        let h = handle.inner().clone();
        tauri::async_runtime::spawn(async move { h.shutdown().await });
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        app.exit(0);
    });
}
