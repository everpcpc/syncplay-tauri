use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::env;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;

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
    detect_from_paths("MPV", get_mpv_paths(), &["mpv.exe"])
}

fn detect_vlc() -> Option<DetectedPlayer> {
    detect_from_paths("VLC", get_vlc_paths(), &["vlc.exe", "VLCPortable.exe"])
}

#[cfg(target_os = "windows")]
fn detect_mpvnet() -> Option<DetectedPlayer> {
    detect_from_paths("mpv.net", get_mpvnet_paths(), &["mpvnet.exe"])
}

fn detect_mplayer() -> Option<DetectedPlayer> {
    detect_from_paths("MPlayer", get_mplayer_paths(), &["mplayer.exe"])
}

#[cfg(target_os = "windows")]
fn detect_mpc_hc() -> Option<DetectedPlayer> {
    detect_from_paths("MPC-HC", get_mpc_hc_paths(), &MPC_HC_EXECUTABLES)
}

#[cfg(target_os = "windows")]
fn detect_mpc_be() -> Option<DetectedPlayer> {
    detect_from_paths("MPC-BE", get_mpc_be_paths(), &MPC_BE_EXECUTABLES)
}

#[cfg(target_os = "macos")]
fn detect_iina() -> Option<DetectedPlayer> {
    let paths = vec![
        PathBuf::from("/Applications/IINA.app/Contents/MacOS/IINA"),
        PathBuf::from("/Applications/IINA.app/Contents/MacOS/iina-cli"),
    ];

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

fn base_mpv_paths() -> Vec<PathBuf> {
    vec![PathBuf::from("mpv"), PathBuf::from("/opt/mpv/mpv")]
}

#[cfg(target_os = "macos")]
fn get_mpv_paths() -> Vec<PathBuf> {
    let mut paths = base_mpv_paths();
    paths.push(PathBuf::from("/Applications/mpv.app/Contents/MacOS/mpv"));
    paths
}

#[cfg(target_os = "windows")]
fn get_mpv_paths() -> Vec<PathBuf> {
    let mut paths = base_mpv_paths();
    paths.push(PathBuf::from("C:\\Program Files\\mpv\\mpv.exe"));
    paths.push(PathBuf::from("C:\\Program Files\\mpv-player\\mpv.exe"));
    paths.push(PathBuf::from("C:\\Program Files (x86)\\mpv\\mpv.exe"));
    paths.push(PathBuf::from(
        "C:\\Program Files (x86)\\mpv-player\\mpv.exe",
    ));

    if let Ok(local_appdata) = std::env::var("LOCALAPPDATA") {
        paths.push(PathBuf::from(format!(
            "{}\\Microsoft\\WindowsApps\\mpv.exe",
            local_appdata
        )));
    }

    paths
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn get_mpv_paths() -> Vec<PathBuf> {
    base_mpv_paths()
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

#[cfg(target_os = "windows")]
fn get_vlc_paths() -> Vec<PathBuf> {
    vec![
        PathBuf::from("C:\\Program Files (x86)\\VideoLAN\\VLC\\vlc.exe"),
        PathBuf::from("C:\\Program Files\\VideoLAN\\VLC\\vlc.exe"),
        PathBuf::from("/usr/bin/vlc"),
        PathBuf::from("/usr/bin/vlc-wrapper"),
        PathBuf::from("/Applications/VLC.app/Contents/MacOS/VLC"),
        PathBuf::from("/usr/local/bin/vlc"),
        PathBuf::from("/usr/local/bin/vlc-wrapper"),
        PathBuf::from("/snap/bin/vlc"),
        PathBuf::from("vlc"),
    ]
}

#[cfg(not(target_os = "windows"))]
fn get_vlc_paths() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/usr/bin/vlc"),
        PathBuf::from("/usr/bin/vlc-wrapper"),
        PathBuf::from("/Applications/VLC.app/Contents/MacOS/VLC"),
        PathBuf::from("/usr/local/bin/vlc"),
        PathBuf::from("/usr/local/bin/vlc-wrapper"),
        PathBuf::from("/snap/bin/vlc"),
        PathBuf::from("vlc"),
    ]
}

#[cfg(target_os = "windows")]
fn get_mplayer_paths() -> Vec<PathBuf> {
    vec![
        PathBuf::from("mplayer2"),
        PathBuf::from("mplayer"),
        PathBuf::from("C:\\Program Files\\mplayer\\mplayer.exe"),
        PathBuf::from("C:\\Program Files (x86)\\mplayer\\mplayer.exe"),
    ]
}

#[cfg(not(target_os = "windows"))]
fn get_mplayer_paths() -> Vec<PathBuf> {
    vec![PathBuf::from("mplayer2"), PathBuf::from("mplayer")]
}

#[cfg(target_os = "windows")]
fn get_mpc_hc_paths() -> Vec<PathBuf> {
    vec![
        PathBuf::from("C:\\Program Files (x86)\\MPC-HC\\mpc-hc.exe"),
        PathBuf::from("C:\\Program Files\\MPC-HC\\mpc-hc.exe"),
        PathBuf::from("C:\\Program Files\\MPC-HC\\mpc-hc64.exe"),
        PathBuf::from("C:\\Program Files\\Media Player Classic - Home Cinema\\mpc-hc.exe"),
        PathBuf::from("C:\\Program Files\\Media Player Classic - Home Cinema\\mpc-hc64.exe"),
        PathBuf::from("C:\\Program Files (x86)\\Media Player Classic - Home Cinema\\mpc-hc.exe"),
        PathBuf::from("C:\\Program Files (x86)\\K-Lite Codec Pack\\MPC-HC\\mpc-hc.exe"),
        PathBuf::from("C:\\Program Files\\K-Lite Codec Pack\\Media Player Classic\\mpc-hc.exe"),
        PathBuf::from("C:\\Program Files\\K-Lite Codec Pack\\MPC-HC64\\mpc-hc64.exe"),
        PathBuf::from("C:\\Program Files (x86)\\K-Lite Codec Pack\\MPC-HC64\\mpc-hc64.exe"),
        PathBuf::from("C:\\Program Files (x86)\\Combined Community Codec Pack\\MPC\\mpc-hc.exe"),
        PathBuf::from("C:\\Program Files\\Combined Community Codec Pack\\MPC\\mpc-hc.exe"),
        PathBuf::from("C:\\Program Files\\MPC HomeCinema (x64)\\mpc-hc64.exe"),
        PathBuf::from("C:\\Program Files (x86)\\LAV Filters\\x86\\mpc-hc\\shoukaku.exe"),
        PathBuf::from("C:\\Program Files (x86)\\LAV Filters\\x64\\mpc-hc\\shoukaku.exe"),
    ]
}

#[cfg(target_os = "windows")]
fn get_mpc_be_paths() -> Vec<PathBuf> {
    vec![
        PathBuf::from("C:\\Program Files\\MPC-BE x64\\mpc-be64.exe"),
        PathBuf::from("C:\\Program Files\\MPC-BE x64\\mpc-be.exe"),
        PathBuf::from("C:\\Program Files\\MPC-BE\\mpc-be64.exe"),
        PathBuf::from("C:\\Program Files\\MPC-BE\\mpc-be.exe"),
    ]
}

fn detect_from_paths(
    name: &str,
    paths: Vec<PathBuf>,
    executable_suffixes: &[&str],
) -> Option<DetectedPlayer> {
    expand_player_paths(paths, executable_suffixes)
        .into_iter()
        .next()
        .map(|path| DetectedPlayer {
            name: name.to_string(),
            path: path.to_string_lossy().to_string(),
            version: None,
        })
}

fn expand_player_paths(paths: Vec<PathBuf>, executable_suffixes: &[&str]) -> Vec<PathBuf> {
    let mut expanded = Vec::new();
    let mut seen = HashSet::new();

    for path in paths {
        for candidate in expand_player_path(&path, executable_suffixes) {
            let key = normalize_path_key(&candidate);
            if seen.insert(key) {
                expanded.push(candidate);
            }
        }
    }

    expanded
}

fn expand_player_path(path: &Path, executable_suffixes: &[&str]) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if is_runnable_file(path) {
        candidates.push(path.to_path_buf());
        return candidates;
    }

    for suffix in executable_suffixes {
        let direct = PathBuf::from(format!("{}{}", path.to_string_lossy(), suffix));
        if is_runnable_file(&direct) {
            candidates.push(direct);
            return candidates;
        }

        let joined = path.join(suffix);
        if is_runnable_file(&joined) {
            candidates.push(joined);
            return candidates;
        }
    }

    if let Some(file_name) = path.file_name().and_then(|name| name.to_str()) {
        candidates.extend(find_on_path(file_name));
    }

    candidates
}

fn find_on_path(program: &str) -> Vec<PathBuf> {
    let Some(paths) = env::var_os("PATH") else {
        return Vec::new();
    };

    let mut candidates = Vec::new();
    for dir in env::split_paths(&paths) {
        for candidate in path_candidates(&dir, program) {
            if is_runnable_file(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    candidates
}

fn path_candidates(dir: &Path, program: &str) -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let mut candidates = vec![dir.join(program)];
        let has_extension = Path::new(program).extension().is_some();
        if !has_extension {
            for extension in windows_path_extensions() {
                candidates.push(dir.join(format!("{}{}", program, extension)));
            }
        }
        candidates
    }

    #[cfg(not(target_os = "windows"))]
    {
        vec![dir.join(program)]
    }
}

#[cfg(target_os = "windows")]
fn windows_path_extensions() -> Vec<String> {
    env::var_os("PATHEXT")
        .map(|value| {
            env::split_paths(&value)
                .map(|extension| extension.to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_else(|| {
            vec![
                ".COM".to_string(),
                ".EXE".to_string(),
                ".BAT".to_string(),
                ".CMD".to_string(),
            ]
        })
}

#[cfg(target_os = "windows")]
fn is_runnable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
fn is_runnable_file(path: &Path) -> bool {
    path.is_file()
        && path
            .metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(any(target_os = "windows", unix)))]
fn is_runnable_file(path: &Path) -> bool {
    path.is_file()
}

fn normalize_path_key(path: &Path) -> String {
    path.to_string_lossy().to_ascii_lowercase()
}

#[cfg(target_os = "windows")]
const MPC_HC_EXECUTABLES: [&str; 6] = [
    "mpc-hc.exe",
    "mpc-hc64.exe",
    "mpc-hcportable.exe",
    "mpc-hc_nvo.exe",
    "mpc-hc64_nvo.exe",
    "shoukaku.exe",
];

#[cfg(target_os = "windows")]
const MPC_BE_EXECUTABLES: [&str; 3] = ["mpc-be.exe", "mpc-beportable.exe", "mpc-be64.exe"];

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[cfg(unix)]
    fn mark_runnable(path: &Path) {
        let mut permissions = path.metadata().unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(not(unix))]
    fn mark_runnable(_path: &Path) {}

    #[test]
    fn expanded_path_appends_player_executable_in_directory() {
        let temp_dir = tempfile::tempdir().unwrap();
        let vlc_path = temp_dir.path().join("vlc.exe");
        File::create(&vlc_path).unwrap();
        mark_runnable(&vlc_path);

        let expanded = expand_player_path(temp_dir.path(), &["vlc.exe"]);

        assert_eq!(expanded, vec![vlc_path]);
    }

    #[test]
    fn detection_scan_does_not_collect_versions() {
        let temp_dir = tempfile::tempdir().unwrap();
        let player_path = temp_dir.path().join("mpv.exe");
        File::create(&player_path).unwrap();
        mark_runnable(&player_path);

        let detected = detect_from_paths("MPV", vec![player_path.clone()], &["mpv.exe"]).unwrap();

        assert_eq!(detected.name, "MPV");
        assert_eq!(detected.path, player_path.to_string_lossy());
        assert_eq!(detected.version, None);
    }
}
