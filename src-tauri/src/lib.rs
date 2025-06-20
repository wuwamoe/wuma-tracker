mod external;
mod offsets;
mod server;
mod types;
mod webrtc_handler;
mod neoserver;
mod util;
use std::sync::Arc;

use external::WinProc;
use server::ServerManager;
use tauri::{
    menu::{Menu, MenuItem}, tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent}, AppHandle, Manager, State, WindowEvent
};
use tokio::sync::Mutex;
use types::{GlobalState, LocalStorageConfig, PlayerInfo};
use util::get_config;

struct AppState {
    proc: Mutex<Option<WinProc>>,
    server_manager: Arc<Mutex<ServerManager>>,
    global_state: Arc<Mutex<GlobalState>>,
}

#[tauri::command]
async fn find_and_attach(app_handle: AppHandle) -> Result<(), String> {
    let Ok(mut win_proc) = WinProc::from_name("Client-Win64-Shipping.exe") else {
        let _ = util::mutate_global_state(app_handle, |old| GlobalState {
            proc_state: 0,
            ..old
        })
        .await;
        return Err(String::from("게임이 실행 중이 아닙니다."));
    };

    if !win_proc.init() {
        let _ = util::mutate_global_state(app_handle, |old| GlobalState {
            proc_state: 0,
            ..old
        })
        .await;
        return Err(String::from("게임에 연결하지 못했습니다."));
    }
    let state = app_handle.state::<AppState>();
    *state.proc.lock().await = Some(win_proc);
    let _ = util::mutate_global_state(app_handle, |old| GlobalState {
        proc_state: 1,
        ..old
    })
    .await;
    Ok(())
}

// remember to call `.manage(MyState::default())`
#[tauri::command]
async fn get_location(state: tauri::State<'_, AppState>) -> Result<PlayerInfo, String> {
    // let state = state.clone();
    let proc_lock = state.proc.lock().await;
    let Some(ref proc) = *proc_lock else {
        return Err(String::from("Process not initialized"));
    };
    return proc.get_location();
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
            auto_attach_enabled
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
    restart_server_impl(app_handle).await;
    Ok(())
}

#[tauri::command]
async fn channel_get_config(app_handle: AppHandle) -> Result<LocalStorageConfig, String> {
    match util::get_config(app_handle).await {
        Ok(config) => return Ok(config),
        Err(e) => return Err(e.to_string()),
    }
}

#[tauri::command]
async fn channel_get_global_state(app_handle: AppHandle) -> Result<GlobalState, String> {
    match util::get_global_state(app_handle).await {
        Ok(gs) => return Ok(gs),
        Err(e) => return Err(e.to_string()),
    }
}

#[tauri::command]
async fn channel_set_global_state(app_handle: AppHandle, value: GlobalState) -> Result<(), String> {
    match util::set_global_state(app_handle, value).await {
        Ok(_) => return Ok(()),
        Err(e) => return Err(e.to_string()),
    }
}

async fn restart_server_impl(app_handle: AppHandle) {
    let config = get_config(app_handle.clone()).await.unwrap_or_default();
    app_handle
        .clone()
        .state::<AppState>()
        .server_manager
        .lock()
        .await
        .restart(
            app_handle,
            config.ip.unwrap_or(String::from("0.0.0.0")),
            config.port.unwrap_or(46821),
        )
        .await;
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
    let server_manager =  Arc::new(Mutex::new(ServerManager::default()));
    let app = builder
        .manage(AppState {
            proc: Mutex::new(None),
            server_manager: server_manager.clone(),
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
                            std::process::exit(0);
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
            tokio::spawn(async move { restart_server_impl(handle).await });

            // Actix 서버를 setup 단계에서 비동기적으로 시작
            // tokio::spawn(async move {
            //     tokio_init(app_handle).await;
            // });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            find_and_attach,
            get_location,
            write_config,
            restart_server,
            channel_get_config,
            channel_get_global_state,
            channel_set_global_state,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run_return(|_app_handle, _event| {});

    // 3. 앱이 닫히고 나면, 제어권이 여기로 돌아옵니다.
    //    이제 여기서 비동기 종료 로직을 깔끔하게 '기다릴' 수 있습니다.
    println!("Tauri app window closed. Starting final cleanup...");
    
    // 복제해 두었던 Arc를 사용하여 shutdown을 호출합니다.
    server_manager.lock().await.shutdown().await;
    
    println!("Cleanup complete. Exiting process.");
}
