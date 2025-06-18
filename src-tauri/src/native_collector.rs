use crate::types::{CollectorMessage, NativeError};
use crate::win_proc::WinProc;
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Mutex};

/// 단순 Windows Process Wrapper
pub struct NativeCollector {
    win_proc: WinProc,
}

impl NativeCollector {
    pub async fn new(proc_name: &str) -> Result<Self> {
        let win_proc = WinProc::new(proc_name)?;
        Ok(Self { win_proc })
    }
}

pub async fn collection_loop(
    collector_arc: Arc<Mutex<Option<NativeCollector>>>,
    pm_tx: mpsc::Sender<CollectorMessage>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
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

            // 3. get_location을 호출하고 결과를 매칭합니다.
            match collector.win_proc.get_location() {
                // 성공 시 데이터 전송
                Ok(loc) => {
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
