use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use crate::offsets::TrackerConfig;
use anyhow::{Result, Context, anyhow};
use tauri::{Emitter, Manager};
use tokio::sync::Mutex;

const CACHE_FILE: &str = "offsets_cache_v2.json";

fn get_remote_urls() -> Vec<&'static str> {
    let mut urls = vec![
        "https://wuwa.moe/tracker-offsets-v2.json",
        "https://raw.githubusercontent.com/wuwamoe/wuwa-moe/refs/heads/main/static/tracker-offsets-v2.json",
    ];

    #[cfg(debug_assertions)]
    {
        urls.insert(0, "http://localhost:1420/tracker-offsets-v2.json");
    }

    urls
}

pub async fn start_offset_loading(app_handle: tauri::AppHandle, target: Arc<Mutex<Option<TrackerConfig>>>) {
    match load_offsets(&app_handle).await {
        Ok(config) => {
            log::info!(
                "오프셋 로드 완료 (last_updated: {}, 패턴 스캔: {})",
                config.last_updated,
                config.gworld_scan.enabled
            );
            *target.lock().await = Some(config);
        }
        Err(_) => {
            let error_message = String::from("오프셋 로딩 실패! 인터넷 연결을 확인하고, 관리자에게 문의하세요.");
            if let Err(emit_err) = app_handle.emit("report-error-toast", error_message) {
                log::error!("Failed to emit error to frontend: {}", emit_err);
            }
        }
    }
}

pub async fn load_offsets(app_handle: &tauri::AppHandle) -> Result<TrackerConfig> {
    let cache_path = app_handle.path().app_config_dir()?
        .join(CACHE_FILE);

    match fetch_from_remotes().await {
        Ok(config) => {
            let _ = save_cache(&cache_path, &config);
            Ok(config)
        }
        Err(e) => {
            log::warn!("모든 서버 연결 실패, 로컬 캐시를 사용합니다. 에러: {}", e);
            let error_message = String::from("오프셋 동기화 실패");
            if let Err(emit_err) = app_handle.emit("report-error-toast", error_message) {
                log::error!("Failed to emit error to frontend: {}", emit_err);
            }
            load_cache(&cache_path)
        }
    }
}

async fn fetch_from_remotes() -> Result<TrackerConfig> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;

    for url in get_remote_urls() {
        log::info!("Trying to fetch offsets from: {}", url);
        match client.get(url).send().await {
            Ok(res) => {
                if res.status().is_success() {
                    match res.json::<TrackerConfig>().await {
                        Ok(config) => return Ok(config),
                        Err(e) => log::warn!("JSON 파싱 에러 ({}): {}", url, e),
                    }
                } else {
                    log::warn!("서버 응답 에러 ({}): {}", url, res.status());
                }
            }
            Err(e) => log::warn!("네트워크 연결 실패 ({}): {}", url, e),
        }
    }

    Err(anyhow!("모든 원격 저장소로부터 데이터를 가져오지 못했습니다."))
}

fn save_cache(path: &PathBuf, config: &TrackerConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string(config)?;
    fs::write(path, json).context("캐시 저장 실패")
}

fn load_cache(path: &PathBuf) -> Result<TrackerConfig> {
    let data = fs::read_to_string(path).context("저장된 캐시가 없습니다.")?;
    Ok(serde_json::from_str(&data)?)
}
