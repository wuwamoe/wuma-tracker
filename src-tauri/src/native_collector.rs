#[cfg(target_os = "macos")]
use crate::mac_proc::MacProc as PlatformProc;
use crate::offsets::TrackerConfig;
use crate::process_backend::ProcessBackend;
use crate::types::{CollectorMessage, NativeError};
#[cfg(windows)]
use crate::win_proc_driver::WinProcDriver as PlatformProc;

#[cfg(not(any(windows, target_os = "macos")))]
compile_error!("Native process tracking is supported only on Windows and macOS.");
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

/// OS별 게임 프로세스 래퍼.
///
/// attach와 좌표 조회를 구분하지 않는다 — `PlatformProc::poll()` 한 번이 "이미 붙어
/// 있으면 읽고, 아니면 다시 찾아서 읽는" 것까지 전부 처리한다. 여기서는 그 결과를
/// `NativeError::ProcessTerminated` 여부만으로 Disconnected/Connected 두 상태로만
/// 나눠 다룬다.
pub struct NativeCollector {
    proc: PlatformProc,
}

impl NativeCollector {
    pub async fn new(proc_name: &str, cache_dir: PathBuf, scan_config: Option<crate::offsets::GWorldScanConfig>) -> Result<Self> {
        let proc_name = proc_name.to_string();
        let proc =
            tokio::task::spawn_blocking(move || PlatformProc::new(&proc_name, cache_dir, scan_config)).await??;
        Ok(Self { proc })
    }
}

pub async fn collection_loop(
    collector_arc: Arc<Mutex<Option<NativeCollector>>>,
    pm_tx: mpsc::Sender<CollectorMessage>,
    cancel: CancellationToken,
    offsets_arc: Arc<Mutex<Option<TrackerConfig>>>,
) {
    let mut reported_offset: Option<String> = None;
    let mut last_error_emit: Option<std::time::Instant> = None;
    loop {
        let offsets_snapshot = offsets_arc.lock().await.as_ref().map(|c| c.offsets.clone());

        let (result, diagnostics) = {
            let mut collector_opt_guard = collector_arc.lock().await;
            let Some(collector) = &mut *collector_opt_guard else {
                log::info!("Collection loop exiting: collector is None");
                break;
            };

            let result = match &offsets_snapshot {
                Some(offsets) => collector.proc.poll(offsets),
                None => Err(NativeError::PointerChainError {
                    message: "오프셋 데이터를 불러오는 중입니다...".to_string(),
                }),
            };
            let diagnostics = collector.proc.diagnostics();
            (result, diagnostics)
        };

        match result {
            Ok(loc) => {
                last_error_emit = None;
                if reported_offset.as_deref() != Some(diagnostics.as_str()) {
                    if pm_tx.send(CollectorMessage::OffsetFound(diagnostics.clone())).await.is_err() {
                        log::info!("Collection loop exiting: no receiver");
                        break;
                    }
                    reported_offset = Some(diagnostics);
                }
                if pm_tx.send(CollectorMessage::Data(loc)).await.is_err() {
                    log::info!("Collection loop exiting: no receiver");
                    break;
                }
            }

            // 대상 프로세스 자체가 없다는 신호만 "연결 해제"로 취급한다.
            Err(NativeError::ProcessTerminated) => {
                log::info!("Collection loop exiting: process is terminated");
                let _ = pm_tx.send(CollectorMessage::Terminated).await;
                break;
            }

            // 그 외 모든 오류(헬퍼/드라이버 불가, 포인터 체인 실패 등)는 연결은 유지된
            // 채로의 일시적 오류로 간주한다 (5초에 1번만 전송).
            Err(e) => {
                let should_emit = last_error_emit
                    .map_or(true, |t| t.elapsed() >= Duration::from_secs(5));
                if should_emit {
                    last_error_emit = Some(std::time::Instant::now());
                    log::warn!("collect: {}", e);
                    if pm_tx
                        .send(CollectorMessage::TemporalError(e.user_message().to_string()))
                        .await
                        .is_err()
                    {
                        log::info!("collect loop: no receiver");
                        break;
                    }
                }
            }
        }

        tokio::select! {
            _ = cancel.cancelled() => {
                log::info!("Collection loop exiting: exit signal received");
                break;
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }
}
