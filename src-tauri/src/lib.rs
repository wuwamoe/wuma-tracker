mod native_collector;
mod offsets;
mod peer_manager;
mod rtc_supervisor;
mod signaling_handler;
mod types;
mod util;
mod win_proc;
use std::sync::Arc;

use crate::rtc_supervisor::RtcSupervisor;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, WindowEvent,
};
use tokio::sync::{oneshot, Mutex};
use types::{GlobalState, LocalStorageConfig};
use util::get_config;

struct TauriState {
    rtc_supervisor: Arc<Mutex<RtcSupervisor>>,
    global_state: Arc<Mutex<GlobalState>>,
}

#[tauri::command]
async fn find_and_attach(app_handle: AppHandle) -> Result<(), String> {
    let state = app_handle.state::<TauriState>();
    state
        .rtc_supervisor
        .lock()
        .await
        .attach_process(app_handle.clone(), "Client-Win64-Shipping.exe")
        .await?;
    Ok(())
}

#[tauri::command]
async fn write_config(
    app_handle: AppHandle,
    ip: Option<String>,
    port: Option<u16>,
    use_secure_connection: Option<bool>,
    auto_attach_enabled: Option<bool>,
) -> Result<(), String> {
    let Ok(_) = util::write_config(
        app_handle,
        LocalStorageConfig {
            ip,
            port,
            use_secure_connection,
            auto_attach_enabled,
        },
    )
    .await
    else {
        return Err(String::from("Error while saving config"));
    };
    Ok(())
}

#[tauri::command]
async fn restart_server(app_handle: AppHandle) -> Result<(), String> {
    restart_server_impl(app_handle).await
}

#[tauri::command]
async fn channel_get_config(app_handle: AppHandle) -> Result<LocalStorageConfig, String> {
    return match get_config(app_handle).await {
        Ok(config) => Ok(config),
        Err(e) => Err(e.to_string()),
    };
}

#[tauri::command]
async fn channel_get_global_state(app_handle: AppHandle) -> Result<GlobalState, String> {
    return match util::get_global_state(app_handle).await {
        Ok(gs) => Ok(gs),
        Err(e) => Err(e.to_string()),
    };
}

#[tauri::command]
async fn channel_set_global_state(app_handle: AppHandle, value: GlobalState) -> Result<(), String> {
    return match util::set_global_state(app_handle, value).await {
        Ok(_) => Ok(()),
        Err(e) => Err(e.to_string()),
    };
}

async fn restart_server_impl(app_handle: AppHandle) -> Result<(), String> {
    let config = get_config(app_handle.clone()).await.unwrap_or_default();
    app_handle
        .clone()
        .state::<TauriState>()
        .rtc_supervisor
        .lock()
        .await
        .restart_local_signaling_server(
            app_handle.clone(),
            config.ip.unwrap_or(String::from("0.0.0.0")),
            config.port.unwrap_or(46821),
        )
        .await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
#[tokio::main]
pub async fn run() {
    let mut builder = tauri::Builder::default();
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let _ = app
                .get_webview_window("main")
                .expect("no main window")
                .set_focus();
        }));
    }
    let rtc_supervisor = Arc::new(Mutex::new(RtcSupervisor::new()));
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    builder
        .manage(TauriState {
            rtc_supervisor: rtc_supervisor.clone(),
            global_state: Arc::new(Mutex::new(GlobalState::default())),
        })
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .max_file_size(5242880)
                .build(),
        )
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            let quit_menu = MenuItem::with_id(app, "quit", "종료", true, None::<&str>)?;
            let show_menu = MenuItem::with_id(app, "show", "창 표시", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_menu, &quit_menu])?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .on_menu_event(|app, event| {
                    let window = app.get_webview_window("main").unwrap();
                    match event.id.as_ref() {
                        "quit" => {
                            app.exit(0);
                        }
                        "show" => {
                            window.show().unwrap();
                            window.set_focus().unwrap();
                        }
                        _ => {}
                    }
                })
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, event| match event {
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } => {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    _ => {}
                })
                .build(app)?;

            if let Some(window) = app.get_webview_window("main") {
                let window_handle = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        window_handle.hide().unwrap();
                        api.prevent_close();
                    }
                });
            }

            let handle = app.handle().clone();
            tokio::spawn(async move {
                let config = get_config(handle.clone()).await.unwrap_or_default();
                rtc_supervisor
                    .lock()
                    .await
                    .run(
                        handle,
                        config.ip.unwrap_or(String::from("0.0.0.0")),
                        config.port.unwrap_or(46821),
                        shutdown_rx,
                    )
                    .await
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            find_and_attach,
            write_config,
            restart_server,
            channel_get_config,
            channel_get_global_state,
            channel_set_global_state,
        ])
        .build(tauri::generate_context!())
        // .run()
        .expect("error while building tauri application");

    println!("Tauri app window closed. Starting final cleanup...");
    let _ = shutdown_tx.send(());
    println!("Cleanup complete. Exiting process.");
}
