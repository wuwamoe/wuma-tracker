use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use crate::offsets::WuwaOffset;
use anyhow::{Result, Context, anyhow};
use tauri::{Emitter, Manager};
use tokio::sync::Mutex;

const CACHE_FILE: &str = "offsets_cache.json";

fn get_remote_urls() -> Vec<&'static str> {
    let mut urls = vec![
        "https://wuwa.moe/tracker-offsets.json",
        "https://raw.githubusercontent.com/wuwamoe/wuwa-moe/refs/heads/main/static/tracker-offsets.json",
    ];

    #[cfg(debug_assertions)]
    {
        urls.insert(0, "http://localhost:1420/tracker-offsets.json");
    }

    urls
}

pub async fn start_offset_loading(app_handle: tauri::AppHandle, target: Arc<Mutex<Option<Vec<WuwaOffset>>>>) {
    match load_offsets(&app_handle).await {
        Ok(offsets) => {
            *target.lock().await = Some(offsets);
        }
        Err(_) => {
            let error_message = String::from("오프셋 로딩 실패! 인터넷 연결을 확인하고, 관리자에게 문의하세요.");
            if let Err(emit_err) = app_handle.emit("report-error-toast", error_message) {
                log::error!("Failed to emit error to frontend: {}", emit_err);
            }
        }
    }
}

pub async fn load_offsets(app_handle: &tauri::AppHandle) -> Result<Vec<WuwaOffset>> {
    let cache_path = app_handle.path().app_config_dir()?
        .join(CACHE_FILE);

    // 1. 여러 서버에서 최신 오프셋 가져오기 시도
    match fetch_from_remotes().await {
        Ok(data) => {
            let _ = save_cache(&cache_path, &data); // 성공 시 캐시 업데이트
            Ok(data)
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

async fn fetch_from_remotes() -> Result<Vec<WuwaOffset>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;

    for url in get_remote_urls() {
        log::info!("Trying to fetch offsets from: {}", url);
        match client.get(url).send().await {
            Ok(res) => {
                if res.status().is_success() {
                    match res.json::<Vec<WuwaOffset>>().await {
                        Ok(data) => return Ok(data), // 성공 시 즉시 반환
                        Err(e) => log::warn!("JSON 파싱 에러 ({}): {}", url, e),
                    }
                } else {
                    log::warn!("서버 응답 에러 ({}): {}", url, res.status());
                }
            }
            Err(e) => log::warn!("네트워크 연결 실패 ({}): {}", url, e),
        }
    }

    // 모든 URL 시도가 실패했을 경우
    Err(anyhow!("모든 원격 저장소로부터 데이터를 가져오지 못했습니다."))
}

fn save_cache(path: &PathBuf, data: &Vec<WuwaOffset>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string(data)?;
    fs::write(path, json).context("캐시 저장 실패")
}

fn load_cache(path: &PathBuf) -> Result<Vec<WuwaOffset>> {
    let data = fs::read_to_string(path).context("저장된 캐시가 없습니다.")?;
    Ok(serde_json::from_str(&data)?)
}