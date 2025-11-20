mod native_collector;
mod offsets;
mod peer_manager;
mod room_code_generator;
mod rtc_supervisor;
mod signaling_handler;
mod types;
mod util;
mod win_proc;

use std::sync::Arc;

use crate::rtc_supervisor::RtcSupervisor;
use crate::types::SupervisorCommand;
use tauri::{
    AppHandle, Manager, WindowEvent,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::{Mutex, mpsc, oneshot};
use types::{GlobalState, LocalStorageConfig};
use util::get_config;

struct TauriState {
    supervisor_tx: mpsc::Sender<SupervisorCommand>,
    global_state: Arc<Mutex<GlobalState>>,
}

#[tauri::command]
async fn find_and_attach(app_handle: AppHandle) -> Result<(), String> {
    let state = app_handle.state::<TauriState>();
    let (resp_tx, resp_rx) = oneshot::channel();
    state
        .supervisor_tx
        .send(SupervisorCommand::AttachProcess(
            "Client-Win64-Shipping.exe".to_string(),
            resp_tx,
        ))
        .await
        .map_err(|e| format!("앱 내부 오류: {}", e))?;

    match resp_rx.await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(format!("앱 내부 오류: {}", e)),
    }
}

#[tauri::command]
async fn write_config(
    app_handle: AppHandle,
    ip: Option<String>,
    port: Option<u16>,
    use_secure_connection: Option<bool>,
    auto_attach_enabled: Option<bool>,
    start_in_tray: Option<bool>,
) -> Result<(), String> {
    let Ok(_) = util::write_config(
        app_handle,
        LocalStorageConfig {
            ip,
            port,
            use_secure_connection,
            auto_attach_enabled,
            start_in_tray,
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
    app_handle
        .state::<TauriState>()
        .supervisor_tx
        .send(SupervisorCommand::RestartSignalingServer)
        .await
        .map_err(|e| format!("재시작 실패: {}", e))
}

#[tauri::command]
async fn restart_external_signaling_client(app_handle: AppHandle) -> Result<String, String> {
    let (resp_tx, resp_rx) = oneshot::channel();
    app_handle
        .state::<TauriState>()
        .supervisor_tx
        .send(SupervisorCommand::RestartExternalConnection(resp_tx))
        .await
        .map_err(|e| format!("앱 내부 오류: {}", e))?;

    match resp_rx.await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(format!("앱 내부 오류: {}", e)),
    }
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
#[tokio::main]
pub async fn run() {
    let mut builder = tauri::Builder::default().plugin(tauri_plugin_clipboard_manager::init());
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let _ = app
                .get_webview_window("main")
                .expect("no main window")
                .set_focus();
        }));
    }
    let mut rtc_supervisor = RtcSupervisor::new();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let (supervisor_tx, supervisor_rx) = mpsc::channel(32);
    let app = builder
        .manage(TauriState {
            supervisor_tx,
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
        .plugin(tauri_plugin_notification::init())
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
            
                // 트레이 시작 설정
                let start_in_tray = config.start_in_tray.unwrap_or(false);
                if !start_in_tray {
                    if let Some(window) = handle.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                } else {
                    if let Err(e) = handle.notification()
                        .builder()
                        .title("명조 맵스 트래커")
                        .body("프로그램이 시스템 트레이에서 실행 중입니다.")
                        .show() {
                        log::error!("알림 발송 실패: {}", e);
                    }
                }

                rtc_supervisor
                    .run(
                        handle,
                        config.ip.unwrap_or(String::from("0.0.0.0")),
                        config.port.unwrap_or(46821),
                        supervisor_rx,
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
            restart_external_signaling_client,
            channel_get_config,
            channel_get_global_state,
            channel_set_global_state,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run_return(|_app_handle, _event| {});

    println!("Tauri app window closed. Starting final cleanup...");
    let _ = shutdown_tx.send(());
    println!("Cleanup complete. Exiting process.");
}
