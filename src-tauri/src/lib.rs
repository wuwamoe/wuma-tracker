mod native_collector;
mod offsets;
mod peer_manager;
mod room_code_generator;
mod rtc_supervisor;
mod signaling_handler;
mod types;
mod util;
mod win_proc;
mod offset_manager;

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
use windows::core::AgileReference;
use types::{GlobalState, LocalStorageConfig};
use util::get_config;
use crate::offsets::WuwaOffset;

struct TauriState {
    supervisor_tx: mpsc::Sender<SupervisorCommand>,
    global_state: Arc<Mutex<GlobalState>>,
    offsets: Arc<Mutex<Option<Vec<WuwaOffset>>>>,
}

#[tauri::command]
fn is_store_build() -> bool {
    cfg!(feature = "store")
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

#[cfg(feature = "store")]
async fn check_store_updates_background(app_handle: AppHandle) -> Result<(), String> {
    use windows::Services::Store::StoreContext;

    let context = StoreContext::GetDefault().map_err(|e| e.to_string())?;
    let updates = context.GetAppAndOptionalStorePackageUpdatesAsync()
        .map_err(|e| e.to_string())?.await
        .map_err(|e| e.to_string())?;

    if updates.Size().map_err(|e| e.to_string())? > 0 {
        log::info!("Store updates found. Switching to main thread for installation request...");

        let updates_agile = AgileReference::new(&updates).map_err(|e| e.to_string())?;

        app_handle.run_on_main_thread(move || {
            if let Ok(updates_resolved) = updates_agile.resolve() {
                if let Ok(store_context) = StoreContext::GetDefault() {
                    log::info!("Triggering store update dialog on main thread.");
                    let _ = store_context.RequestDownloadAndInstallStorePackageUpdatesAsync(&updates_resolved);
                }
            }
        }).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
#[tokio::main]
pub async fn run() {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("install rustls crypto provider");

    let offsets_shared = Arc::new(Mutex::new(None));
    let offsets_for_setup = offsets_shared.clone();
    let offsets_for_supervisor = offsets_shared.clone();

    let mut builder = tauri::Builder::default().plugin(tauri_plugin_clipboard_manager::init());
    #[cfg(not(feature = "store"))]
    {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    }
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let _ = app
                .get_webview_window("main")
                .expect("no main window")
                .set_focus();
        }));
    }
    let mut rtc_supervisor = RtcSupervisor::new(offsets_for_supervisor);
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let (supervisor_tx, supervisor_rx) = mpsc::channel(32);
    let app = builder
        .manage(TauriState {
            supervisor_tx,
            global_state: Arc::new(Mutex::new(GlobalState::default())),
            offsets: offsets_shared
        })
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .max_file_size(5242880)
                .build(),
        )
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let handle = app.handle().clone();
            tokio::spawn(async move {
                offset_manager::start_offset_loading(handle, offsets_for_setup).await;
            });

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
                        config.ip.unwrap_or(String::from("127.0.0.1")),
                        config.port.unwrap_or(46821),
                        supervisor_rx,
                        shutdown_rx,
                    )
                    .await
            });

            #[cfg(feature = "store")]
            {
                let handle = app.handle().clone();
                tokio::spawn(async move {
                    log::info!("Checking for MS Store updates...");
                    if let Err(e) = check_store_updates_background(handle).await {
                        log::error!("Failed to check store updates: {}", e);
                    }
                });
            }
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
            is_store_build
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run_return(|_app_handle, _event| {});

    println!("Tauri app window closed. Starting final cleanup...");
    let _ = shutdown_tx.send(());
    println!("Cleanup complete. Exiting process.");
}
