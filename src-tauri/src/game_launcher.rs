use anyhow::Result;
use std::path::PathBuf;

#[cfg(windows)]
const WIN_RELATIVE: &str = "Program Files\\Wuthering Waves\\Wuthering Waves Game\\Client\\Binaries\\Win64\\Client-Win64-Shipping.exe";

#[cfg(target_os = "macos")]
const MACOS_APP_PATH: &str = "/Applications/WutheringWaves.app";

pub fn scan_game_candidates() -> Vec<String> {
    #[cfg(windows)]
    {
        ('A'..='Z')
            .map(|c| format!("{}:\\{}", c, WIN_RELATIVE))
            .filter(|p| std::path::Path::new(p).is_file())
            .collect()
    }
    #[cfg(target_os = "macos")]
    {
        if std::path::Path::new(MACOS_APP_PATH).exists() {
            vec![MACOS_APP_PATH.to_string()]
        } else {
            vec![]
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        vec![]
    }
}

#[cfg(windows)]
pub fn launch_and_create_proc(
    path: &str,
    cache_dir: PathBuf,
    scan_config: Option<crate::offsets::GWorldScanConfig>,
) -> Result<crate::win_proc::WinProc> {
    use std::os::windows::io::AsRawHandle;
    use winapi::shared::minwindef::{DWORD, FALSE};
    use winapi::um::handleapi::DuplicateHandle;
    use winapi::um::processthreadsapi::GetCurrentProcess;
    use winapi::um::winnt::HANDLE;

    const DUPLICATE_SAME_ACCESS: DWORD = 0x00000002;

    let game_dir = std::path::Path::new(path)
        .parent()
        .ok_or_else(|| anyhow::anyhow!("잘못된 경로: {}", path))?
        .to_path_buf();

    let child = std::process::Command::new(path)
        .current_dir(&game_dir)
        .spawn()
        .map_err(|e| anyhow::anyhow!("게임 실행 실패: {}", e))?;

    let pid = child.id();
    let src_handle = child.as_raw_handle() as HANDLE;

    let mut dup_handle: HANDLE = std::ptr::null_mut();
    let ok = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            src_handle,
            GetCurrentProcess(),
            &mut dup_handle,
            0,
            FALSE,
            DUPLICATE_SAME_ACCESS,
        )
    };

    // Releasing the child handle here; dup_handle is an independent reference to the process.
    drop(child);

    if ok == 0 || dup_handle.is_null() {
        anyhow::bail!("핸들 복제 실패: {}", std::io::Error::last_os_error());
    }

    log::info!("게임 실행됨, PID: {}, 핸들 복제 완료. 모듈 로드 대기 중...", pid);
    crate::win_proc::WinProc::from_handle(dup_handle, pid, cache_dir, scan_config)
}

#[cfg(target_os = "macos")]
pub fn launch_game(path: &str) -> Result<()> {
    std::process::Command::new("open")
        .arg(path)
        .spawn()
        .map_err(|e| anyhow::anyhow!("게임 실행 실패: {}", e))?;
    Ok(())
}
