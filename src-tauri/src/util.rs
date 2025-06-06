use std::path::PathBuf;

use anyhow::{Context, Result};
use tauri::{AppHandle, Emitter, Manager};
use tokio::fs::{create_dir_all, read_to_string, write};

use crate::{
    types::{GlobalState, LocalStorageConfig},
    AppState,
};

pub async fn get_config(app_handle: AppHandle) -> Result<LocalStorageConfig> {
    let app_config_dir = get_config_file(app_handle).await?;
    let file = read_to_string(&app_config_dir)
        .await
        .context("Failed to open config file")?;
    serde_json::from_str::<LocalStorageConfig>(file.as_str()).context("Failed to parse config file")
}

pub async fn write_config(app_handle: AppHandle, data: LocalStorageConfig) -> Result<()> {
    let app_config_dir = get_config_file(app_handle).await?;
    write(
        &app_config_dir,
        serde_json::to_string(&data).context("Failed to serialize config to write")?,
    )
    .await
    .context("Failed to write config")?;
    Ok(())
}

async fn get_config_file(app_handle: AppHandle) -> Result<PathBuf> {
    let res = app_handle
        .path()
        .app_config_dir()
        .context("Failed to retrieve config directory path")?;
    create_dir_all(&res)
        .await
        .context("Failed to create config base directory")?;
    Ok(res.join("config.json"))
}

pub async fn get_global_state(app_handle: AppHandle) -> Result<GlobalState> {
    let app_state = app_handle.state::<AppState>();
    let global_state_lock = app_state.global_state.lock().await;
    let global_state = global_state_lock.clone();
    return Ok(global_state);
}

pub async fn set_global_state(app_handle: AppHandle, value: GlobalState) -> Result<()> {
    let app_state = app_handle.state::<AppState>();
    let mut guard = app_state.global_state.lock().await;
    if guard.clone() == value {
        return Ok(());
    }

    *guard = value.clone();
    let _ = app_handle
        .emit("handle-global-state-change", value)
        .context("Failed to emit event on global state change");

    return Ok(());
}

pub async fn mutate_global_state(
    app_handle: AppHandle,
    mutation: impl Fn(GlobalState) -> GlobalState,
) -> Result<()> {
    let app_state = app_handle.state::<AppState>();
    let mut guard = app_state.global_state.lock().await;
    let new_value = mutation(guard.clone());
    if guard.clone() == new_value {
        return Ok(());
    }

    *guard = new_value.clone();
    let _ = app_handle
        .emit("handle-global-state-change", new_value)
        .context("Failed to emit event on global state change");

    return Ok(());
}
