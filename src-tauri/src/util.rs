use std::path::PathBuf;

use anyhow::{Context, Result};
use tauri::{AppHandle, Manager};
use tokio::fs::{create_dir_all, read_to_string, write};

use crate::types::LocalStorageConfig;

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
    create_dir_all(&res).await.context("Failed to create config base directory")?;
    Ok(res.join("config.json"))
}
