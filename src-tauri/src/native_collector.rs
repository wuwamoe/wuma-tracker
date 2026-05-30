#[cfg(target_os = "macos")]
use crate::mac_proc::MacProc as PlatformProc;
use crate::offsets::WuwaOffset;
use crate::types::{CollectorMessage, NativeError};
#[cfg(windows)]
use crate::win_proc::WinProc as PlatformProc;

#[cfg(not(any(windows, target_os = "macos")))]
compile_error!("Native process tracking is supported only on Windows and macOS.");
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc, oneshot};

/// OS별 게임 프로세스 래퍼
pub struct NativeCollector {
    proc: PlatformProc,
}

impl NativeCollector {
    pub async fn new(proc_name: &str, cache_dir: PathBuf) -> Result<Self> {
        let proc_name = proc_name.to_string();
        let proc =
            tokio::task::spawn_blocking(move || PlatformProc::new(&proc_name, cache_dir)).await??;
        Ok(Self { proc })
    }
}

pub async fn collection_loop(
    collector_arc: Arc<Mutex<Option<NativeCollector>>>,
    pm_tx: mpsc::Sender<CollectorMessage>,
    mut shutdown_rx: oneshot::Receiver<()>,
    offsets_arc: Arc<Mutex<Option<Vec<WuwaOffset>>>>,
) {
    let mut reported_offset: Option<String> = None;
    loop {
        // Work Phase
        {
            // 1. 상태 관리자를 잠그고 공유 상태에 접근합니다.
            let mut collector_opt_guard = collector_arc.lock().await;

            // 2. Option이 Some일 때만 로직을 수행합니다.
            //    (다른 곳에서 이미 None으로 만들었다면 루프를 종료합니다)
            let Some(collector) = &mut *collector_opt_guard else {
                log::info!("Collection loop exiting: collector is None");
                break;
            };

            let offsets_guard = offsets_arc.lock().await;

            // 3. get_location을 호출하고 결과를 매칭합니다.
            match collector.proc.get_location(&*offsets_guard).await {
                // 성공 시 데이터 전송
                Ok(loc) => {
                    if let Some(name) = collector.proc.get_active_offset_name() {
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
        }
        // Sleep Phase
        tokio::select! {
            // 외부(PeerManager)로부터의 종료 신호 처리
            _ = &mut shutdown_rx => {
                log::info!("Collection loop exiting: exit signal received");
                break;
            }

            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
        }
    }
}
