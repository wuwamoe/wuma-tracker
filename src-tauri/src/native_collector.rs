#[cfg(target_os = "macos")]
use crate::mac_proc::MacProc as PlatformProc;
use crate::offsets::WuwaOffset;
use crate::process_backend::{ProcessBackend, select_player_info};
use crate::types::NativeError::PointerChainError;
use crate::types::{CollectorMessage, NativeError};
#[cfg(windows)]
use crate::win_proc::WinProc as PlatformProc;

#[cfg(not(any(windows, target_os = "macos")))]
compile_error!("Native process tracking is supported only on Windows and macOS.");
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

// 500ms 루프 기준 60회 = 30초
const MAX_GWORLD_RESCAN_FAILURES: u32 = 60;

/// OS별 게임 프로세스 래퍼
pub struct NativeCollector {
    proc: PlatformProc,
    offset: Option<WuwaOffset>,
    consecutive_failures: u32,
    rescan_done: bool,
}

impl NativeCollector {
    pub async fn new(proc_name: &str, cache_dir: PathBuf) -> Result<Self> {
        let proc_name = proc_name.to_string();
        let proc =
            tokio::task::spawn_blocking(move || PlatformProc::new(&proc_name, cache_dir)).await??;
        Ok(Self { proc, offset: None, consecutive_failures: 0, rescan_done: false })
    }

    fn get_location(
        &mut self,
        available_offsets: &Option<Vec<WuwaOffset>>,
    ) -> Result<crate::types::PlayerInfo, NativeError> {
        let Some(variants) = available_offsets else {
            return Err(PointerChainError {
                message: "오프셋 데이터를 불러오는 중입니다...".to_string(),
            });
        };

        match select_player_info(&self.proc, &mut self.offset, variants) {
            Ok(info) => {
                self.consecutive_failures = 0;
                Ok(info)
            }
            Err(e) => {
                self.consecutive_failures += 1;
                if self.consecutive_failures >= MAX_GWORLD_RESCAN_FAILURES && !self.rescan_done {
                    log::info!("{}회 연속 실패 → GWorld 재스캔", MAX_GWORLD_RESCAN_FAILURES);
                    self.proc.rescan_gworld();
                    self.rescan_done = true;
                    self.offset = None;
                }
                Err(e)
            }
        }
    }

    fn get_active_offset_name(&self) -> Option<String> {
        self.offset
            .as_ref()
            .map(|offset| self.proc.active_offset_name(offset))
    }
}

pub async fn collection_loop(
    collector_arc: Arc<Mutex<Option<NativeCollector>>>,
    pm_tx: mpsc::Sender<CollectorMessage>,
    cancel: CancellationToken,
    offsets_arc: Arc<Mutex<Option<Vec<WuwaOffset>>>>,
) {
    let mut reported_offset: Option<String> = None;
    loop {
        let offsets_snapshot = offsets_arc.lock().await.clone();
        let result = {
            // 1. 상태 관리자를 잠그고 공유 상태에 접근합니다.
            let mut collector_opt_guard = collector_arc.lock().await;

            // 2. Option이 Some일 때만 로직을 수행합니다.
            //    (다른 곳에서 이미 None으로 만들었다면 루프를 종료합니다)
            let Some(collector) = &mut *collector_opt_guard else {
                log::info!("Collection loop exiting: collector is None");
                break;
            };

            // 3. get_location을 호출하고 결과를 매칭합니다.
            match collector.get_location(&offsets_snapshot) {
                // 성공 시 데이터 전송
                Ok(loc) => {
                    let offset_name = collector.get_active_offset_name();
                    Ok((loc, offset_name))
                }

                // '프로세스 종료'는 치명적 오류
                Err(NativeError::ProcessTerminated) => Err(NativeError::ProcessTerminated),

                // 그 외 모든 오류는 일시적인 것으로 간주
                Err(e) => Err(e),
            }
        };

        match result {
            Ok((loc, offset_name)) => {
                if let Some(name) = offset_name {
                    if reported_offset.as_deref() != Some(name.as_str()) {
                        // RtcSupervisor에게 OffsetFound 메시지를 보냅니다.
                        if pm_tx
                            .send(CollectorMessage::OffsetFound(name.clone()))
                            .await
                            .is_err()
                        {
                            log::info!("Collection loop exiting: no receiver");
                            break;
                        }
                        reported_offset = Some(name);
                    }
                }
                if pm_tx.send(CollectorMessage::Data(loc)).await.is_err() {
                    log::info!("Collection loop exiting: no receiver");
                    break;
                }
            }

            // '프로세스 종료'는 치명적 오류
            Err(NativeError::ProcessTerminated) => {
                log::info!("Collection loop exiting: process is terminated");
                let _ = pm_tx.send(CollectorMessage::Terminated).await;
                break;
            }

            // 그 외 모든 오류는 일시적인 것으로 간주
            Err(e) => {
                if pm_tx
                    .send(CollectorMessage::TemporalError(e.to_string()))
                    .await
                    .is_err()
                {
                    log::info!("Collection loop exiting: no receiver");
                    break;
                }
            }
        }
        // Sleep Phase
        tokio::select! {
            _ = cancel.cancelled() => {
                log::info!("Collection loop exiting: exit signal received");
                break;
            }
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
        }
    }
}
