use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedPlayer {
    pub name: String,
    pub path: String,
    pub version: Option<String>,
}

/// Detect available media players on the system
pub fn detect_players() -> Vec<DetectedPlayer> {
    let mut players = Vec::new();

    // Detect MPV
    if let Some(mpv) = detect_mpv() {
        players.push(mpv);
    }

    // Detect mpv.net (Windows only)
    #[cfg(target_os = "windows")]
    if let Some(mpvnet) = detect_mpvnet() {
        players.push(mpvnet);
    }

    // Detect VLC
    if let Some(vlc) = detect_vlc() {
        players.push(vlc);
    }

    // Detect IINA (macOS only)
    #[cfg(target_os = "macos")]
    if let Some(iina) = detect_iina() {
        players.push(iina);
    }

    // Detect MPlayer
    if let Some(mplayer) = detect_mplayer() {
        players.push(mplayer);
    }

    // Detect MPC-HC / MPC-BE (Windows only)
    #[cfg(target_os = "windows")]
    {
        if let Some(mpc_hc) = detect_mpc_hc() {
            players.push(mpc_hc);
        }
        if let Some(mpc_be) = detect_mpc_be() {
            players.push(mpc_be);
        }
    }

    players
}

fn detect_mpv() -> Option<DetectedPlayer> {
    let paths = get_mpv_paths();

    for path in paths {
        if let Ok(output) = Command::new(&path).arg("--version").output() {
            if output.status.success() {
                let version_str = String::from_utf8_lossy(&output.stdout);
                let version = parse_mpv_version(&version_str);

                return Some(DetectedPlayer {
                    name: "MPV".to_string(),
                    path: path.to_string_lossy().to_string(),
                    version,
                });
            }
        }
    }

    None
}

fn detect_vlc() -> Option<DetectedPlayer> {
    let paths = get_vlc_paths();

    for path in paths {
        if let Ok(output) = Command::new(&path).arg("--version").output() {
            if output.status.success() {
                let version_str = String::from_utf8_lossy(&output.stdout);
                let version = parse_vlc_version(&version_str);

                return Some(DetectedPlayer {
                    name: "VLC".to_string(),
                    path: path.to_string_lossy().to_string(),
                    version,
                });
            }
        }
    }

    None
}

#[cfg(target_os = "windows")]
fn detect_mpvnet() -> Option<DetectedPlayer> {
    let paths = get_mpvnet_paths();

    for path in paths {
        if path.exists() {
            let version = Command::new(&path)
                .arg("--version")
                .output()
                .ok()
                .and_then(|output| {
                    if output.status.success() {
                        let version_str = String::from_utf8_lossy(&output.stdout);
                        parse_mpv_version(&version_str)
                    } else {
                        None
                    }
                });

            return Some(DetectedPlayer {
                name: "mpv.net".to_string(),
                path: path.to_string_lossy().to_string(),
                version,
            });
        }
    }

    None
}

fn detect_mplayer() -> Option<DetectedPlayer> {
    let paths = get_mplayer_paths();

    for path in paths {
        if let Ok(output) = Command::new(&path).arg("-version").output() {
            if output.status.success() {
                let version_str = String::from_utf8_lossy(&output.stdout);
                let version = parse_mplayer_version(&version_str);
                return Some(DetectedPlayer {
                    name: "MPlayer".to_string(),
                    path: path.to_string_lossy().to_string(),
                    version,
                });
            }
        }
    }

    None
}

#[cfg(target_os = "windows")]
fn detect_mpc_hc() -> Option<DetectedPlayer> {
    let paths = get_mpc_hc_paths();
    for path in paths {
        if path.exists() {
            return Some(DetectedPlayer {
                name: "MPC-HC".to_string(),
                path: path.to_string_lossy().to_string(),
                version: None,
            });
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn detect_mpc_be() -> Option<DetectedPlayer> {
    let paths = get_mpc_be_paths();
    for path in paths {
        if path.exists() {
            return Some(DetectedPlayer {
                name: "MPC-BE".to_string(),
                path: path.to_string_lossy().to_string(),
                version: None,
            });
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn detect_iina() -> Option<DetectedPlayer> {
    let paths = vec![PathBuf::from(
        "/Applications/IINA.app/Contents/MacOS/iina-cli",
    )];

    for path in paths {
        if path.exists() {
            return Some(DetectedPlayer {
                name: "IINA".to_string(),
                path: path.to_string_lossy().to_string(),
                version: None,
            });
        }
    }

    None
}

fn get_mpv_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "macos")]
    {
        paths.push(PathBuf::from("/usr/local/bin/mpv"));
        paths.push(PathBuf::from("/opt/homebrew/bin/mpv"));
        paths.push(PathBuf::from("/Applications/mpv.app/Contents/MacOS/mpv"));
    }

    #[cfg(target_os = "linux")]
    {
        paths.push(PathBuf::from("/usr/bin/mpv"));
        paths.push(PathBuf::from("/usr/local/bin/mpv"));
    }

    #[cfg(target_os = "windows")]
    {
        paths.push(PathBuf::from("C:\\Program Files\\mpv\\mpv.exe"));
        paths.push(PathBuf::from("C:\\Program Files (x86)\\mpv\\mpv.exe"));
    }

    // Also check PATH
    if let Ok(output) = Command::new("which").arg("mpv").output() {
        if output.status.success() {
            let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path_str.is_empty() {
                paths.push(PathBuf::from(path_str));
            }
        }
    }

    paths
}

#[cfg(target_os = "windows")]
fn get_mpvnet_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    paths.push(PathBuf::from("C:\\Program Files\\mpv.net\\mpvnet.exe"));
    paths.push(PathBuf::from(
        "C:\\Program Files (x86)\\mpv.net\\mpvnet.exe",
    ));
    if let Ok(local_appdata) = std::env::var("LOCALAPPDATA") {
        paths.push(PathBuf::from(format!(
            "{}\\Microsoft\\WindowsApps\\mpvnet.exe",
            local_appdata
        )));
        paths.push(PathBuf::from(format!(
            "{}\\Programs\\mpv.net\\mpvnet.exe",
            local_appdata
        )));
    }
    paths
}

fn get_vlc_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "macos")]
    {
        paths.push(PathBuf::from("/Applications/VLC.app/Contents/MacOS/VLC"));
    }

    #[cfg(target_os = "linux")]
    {
        paths.push(PathBuf::from("/usr/bin/vlc"));
        paths.push(PathBuf::from("/usr/local/bin/vlc"));
    }

    #[cfg(target_os = "windows")]
    {
        paths.push(PathBuf::from("C:\\Program Files\\VideoLAN\\VLC\\vlc.exe"));
        paths.push(PathBuf::from(
            "C:\\Program Files (x86)\\VideoLAN\\VLC\\vlc.exe",
        ));
    }

    // Also check PATH
    if let Ok(output) = Command::new("which").arg("vlc").output() {
        if output.status.success() {
            let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path_str.is_empty() {
                paths.push(PathBuf::from(path_str));
            }
        }
    }

    paths
}

fn get_mplayer_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "linux")]
    {
        paths.push(PathBuf::from("/usr/bin/mplayer"));
        paths.push(PathBuf::from("/usr/local/bin/mplayer"));
        paths.push(PathBuf::from("/usr/bin/mplayer2"));
    }

    #[cfg(target_os = "macos")]
    {
        paths.push(PathBuf::from("/usr/local/bin/mplayer"));
        paths.push(PathBuf::from("/opt/homebrew/bin/mplayer"));
    }

    #[cfg(target_os = "windows")]
    {
        paths.push(PathBuf::from("C:\\Program Files\\mplayer\\mplayer.exe"));
        paths.push(PathBuf::from("C:\\Program Files (x86)\\mplayer\\mplayer.exe"));
    }

    if let Ok(output) = Command::new("which").arg("mplayer").output() {
        if output.status.success() {
            let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path_str.is_empty() {
                paths.push(PathBuf::from(path_str));
            }
        }
    }

    paths
}

#[cfg(target_os = "windows")]
fn get_mpc_hc_paths() -> Vec<PathBuf> {
    vec![
        PathBuf::from("C:\\Program Files\\MPC-HC\\mpc-hc.exe"),
        PathBuf::from("C:\\Program Files\\MPC-HC\\mpc-hc64.exe"),
        PathBuf::from("C:\\Program Files (x86)\\MPC-HC\\mpc-hc.exe"),
        PathBuf::from("C:\\Program Files\\MPC-HC\\mpc-hc64.exe"),
        PathBuf::from("C:\\Program Files\\K-Lite Codec Pack\\MPC-HC\\mpc-hc.exe"),
        PathBuf::from("C:\\Program Files (x86)\\K-Lite Codec Pack\\MPC-HC\\mpc-hc.exe"),
    ]
}

#[cfg(target_os = "windows")]
fn get_mpc_be_paths() -> Vec<PathBuf> {
    vec![
        PathBuf::from("C:\\Program Files\\MPC-BE\\mpc-be.exe"),
        PathBuf::from("C:\\Program Files\\MPC-BE\\mpc-be64.exe"),
        PathBuf::from("C:\\Program Files (x86)\\MPC-BE\\mpc-be.exe"),
    ]
}

fn parse_mpv_version(output: &str) -> Option<String> {
    // Parse version from output like "mpv 0.35.0 Copyright ..."
    output
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1).map(|v| v.to_string()))
}

fn parse_vlc_version(output: &str) -> Option<String> {
    // Parse version from VLC output
    output
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(2).map(|v| v.to_string()))
}

fn parse_mplayer_version(output: &str) -> Option<String> {
    output
        .lines()
        .find(|line| line.to_ascii_lowercase().contains("mplayer"))
        .and_then(|line| line.split_whitespace().nth(1).map(|v| v.to_string()))
}
