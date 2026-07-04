use anyhow::Result;

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

/// 게임을 실행만 하고 바로 놓아준다(fire-and-forget). attach는 항상 이름 기반으로
/// 별도 수행되므로(`RtcSupervisor::attach_process`), 여기서는 프로세스 핸들을
/// 붙잡아 둘 필요가 없다.
#[cfg(windows)]
pub fn spawn_game(path: &str) -> Result<()> {
    let game_dir = std::path::Path::new(path)
        .parent()
        .ok_or_else(|| anyhow::anyhow!("잘못된 경로: {}", path))?
        .to_path_buf();

    std::process::Command::new(path)
        .current_dir(&game_dir)
        .spawn()
        .map_err(|e| anyhow::anyhow!("게임 실행 실패: {}", e))?;

    Ok(())
}

#[cfg(target_os = "macos")]
pub fn spawn_game(path: &str) -> Result<()> {
    std::process::Command::new("open")
        .arg(path)
        .spawn()
        .map_err(|e| anyhow::anyhow!("게임 실행 실패: {}", e))?;
    Ok(())
}
