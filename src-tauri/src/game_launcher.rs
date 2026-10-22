use anyhow::Result;

#[cfg(windows)]
const WIN_RELATIVE: &str = "Program Files\\Wuthering Waves\\Wuthering Waves Game\\Client\\Binaries\\Win64\\Client-Win64-Shipping.exe";

/// 사용자가 설정에 적은 커스텀 실행 인자 문자열을 토큰으로 쪼갠다. 공백으로
/// 나누되 큰따옴표로 묶인 구간은 하나의 인자로 유지한다(경로에 공백이 있는 경우).
/// 예: `-krqlv=uhd "foo bar"` → ["-krqlv=uhd", "foo bar"].
#[cfg(any(windows, target_os = "macos"))]
fn split_launch_args(args: Option<&str>) -> Vec<String> {
    let Some(raw) = args else {
        return Vec::new();
    };

    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut has_token = false;

    for ch in raw.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                has_token = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_token {
                    tokens.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            c => {
                current.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        tokens.push(current);
    }
    tokens
}

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
pub fn spawn_game(path: &str, args: Option<&str>) -> Result<()> {
    let game_dir = std::path::Path::new(path)
        .parent()
        .ok_or_else(|| anyhow::anyhow!("잘못된 경로: {}", path))?
        .to_path_buf();

    std::process::Command::new(path)
        .current_dir(&game_dir)
        .args(split_launch_args(args))
        .spawn()
        .map_err(|e| anyhow::anyhow!("게임 실행 실패: {}", e))?;

    Ok(())
}

#[cfg(target_os = "macos")]
pub fn spawn_game(path: &str, args: Option<&str>) -> Result<()> {
    let mut command = std::process::Command::new("open");
    command.arg(path);

    // `open`에 넘긴 인자는 앱(.app)이 아니라 open 자신에게 전달되므로,
    // 커스텀 인자가 있을 때만 `--args` 뒤에 붙여 게임 본체로 전달한다.
    let launch_args = split_launch_args(args);
    if !launch_args.is_empty() {
        command.arg("--args").args(launch_args);
    }

    command
        .spawn()
        .map_err(|e| anyhow::anyhow!("게임 실행 실패: {}", e))?;
    Ok(())
}
