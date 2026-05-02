use super::backend::PlayerBackend;
use super::properties::PlayerState;
use async_trait::async_trait;
use futures::StreamExt;
use parking_lot::Mutex;
use rand::Rng;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{tcp::OwnedReadHalf, tcp::OwnedWriteHalf, TcpStream};
use tokio::process::{Child, Command};
use tokio::sync::Mutex as TokioMutex;
use tokio_util::codec::{FramedRead, LinesCodec};
use tracing::{debug, info, warn};

const VLC_MIN_VERSION: &str = "2.2.1";
const VLC_INTERFACE_VERSION: &str = "0.3.7";
const VLC_OPEN_MAX_WAIT_TIME: Duration = Duration::from_secs(20);
const VLC_MIN_PORT: u16 = 10000;
const VLC_MAX_PORT: u16 = 55000;
const VLC_LATENCY_ERROR_THRESHOLD: f64 = 2.0;

const VLC_ARGS: &[&str] = &[
    "--extraintf=luaintf",
    "--lua-intf=syncplay",
    "--no-quiet",
    "--no-input-fast-seek",
    "--play-and-pause",
    "--start-time=0",
];

#[derive(Clone)]
struct Connection {
    writer: Arc<TokioMutex<OwnedWriteHalf>>,
}

impl Connection {
    async fn send_line(&self, line: &str) -> anyhow::Result<()> {
        let mut guard = self.writer.lock().await;
        guard.write_all(format!("{}\n", line).as_bytes()).await?;
        guard.flush().await?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct VlcPositionHistory {
    previous_previous: f64,
    previous: f64,
}

impl Default for VlcPositionHistory {
    fn default() -> Self {
        Self {
            previous_previous: -2.0,
            previous: -1.0,
        }
    }
}

pub struct VlcSyncplayBackend {
    state: Arc<Mutex<PlayerState>>,
    connection: Connection,
    last_position_update: Arc<Mutex<Option<Instant>>>,
    last_duration: Arc<Mutex<Option<f64>>>,
    last_loaded: Arc<Mutex<Option<String>>>,
}

impl VlcSyncplayBackend {
    pub async fn start(
        player_path: &str,
        args: &[String],
        initial_file: Option<&str>,
        syncplay_lua_path: PathBuf,
    ) -> anyhow::Result<(Self, Child)> {
        info!(
            "Starting VLC: path={}, args={:?}, initial_file={:?}",
            player_path, args, initial_file
        );

        let port = pick_vlc_port();
        let (intf_path, user_path) = resolve_vlc_paths(player_path)?;
        install_syncplay_lua(&user_path, &syncplay_lua_path)?;

        let module_path = format!("{}/modules/?.luac", intf_path.replace('\\', "/"));
        let mut cmd = Command::new(player_path);
        cmd.args(VLC_ARGS);
        cmd.arg(format!(
            "--lua-config=syncplay={{modulepath=\"{}\",port=\"{}\"}}",
            module_path, port
        ));
        cmd.args(build_vlc_extra_args(player_path));
        cmd.args(args);
        if let Some(path) = initial_file {
            let arg = if is_ascii_path(path) && !is_url(path) {
                path.to_string()
            } else {
                build_mrl(path)
            };
            cmd.arg(arg);
        }
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        let child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to start VLC: {}", e))?;

        let stream = connect_with_retry(port).await?;
        let (read_half, write_half) = stream.into_split();
        let connection = Connection {
            writer: Arc::new(TokioMutex::new(write_half)),
        };

        let state = Arc::new(Mutex::new(PlayerState::default()));
        let last_position_update = Arc::new(Mutex::new(None));
        let last_duration = Arc::new(Mutex::new(None));
        let last_loaded = Arc::new(Mutex::new(initial_file.map(|s| s.to_string())));
        let position_history = Arc::new(Mutex::new(VlcPositionHistory::default()));

        spawn_reader(
            connection.clone(),
            read_half,
            state.clone(),
            last_position_update.clone(),
            last_duration.clone(),
            last_loaded.clone(),
            position_history,
        );

        let backend = Self {
            state,
            connection,
            last_position_update,
            last_duration,
            last_loaded,
        };

        let _ = backend.connection.send_line("get-vlc-version").await;
        backend.request_file_info().await?;
        Ok((backend, child))
    }

    async fn request_status(&self) -> anyhow::Result<()> {
        self.connection.send_line(".").await
    }

    async fn request_file_info(&self) -> anyhow::Result<()> {
        self.connection.send_line("get-duration").await?;
        self.connection.send_line("get-filepath").await?;
        self.connection.send_line("get-filename").await?;
        Ok(())
    }
}

#[async_trait]
impl PlayerBackend for VlcSyncplayBackend {
    fn kind(&self) -> super::backend::PlayerKind {
        super::backend::PlayerKind::Vlc
    }

    fn name(&self) -> &'static str {
        "VLC"
    }

    fn get_state(&self) -> PlayerState {
        let mut snapshot = self.state.lock().clone();
        let last_update = *self.last_position_update.lock();
        if snapshot.paused == Some(false) {
            if let (Some(position), Some(last_update)) = (snapshot.position, last_update) {
                let diff = last_update.elapsed().as_secs_f64();
                if diff > 0.1 {
                    if diff > VLC_LATENCY_ERROR_THRESHOLD {
                        warn!("VLC position update delayed: {}s", diff);
                    }
                    snapshot.position = Some(position + diff);
                }
            }
        }
        snapshot
    }

    async fn poll_state(&self) -> anyhow::Result<()> {
        if let Err(e) = self.request_status().await {
            warn!("Failed to query VLC status: {}", e);
        }
        Ok(())
    }

    async fn set_position(&self, position: f64) -> anyhow::Result<()> {
        *self.last_position_update.lock() = Some(Instant::now());
        self.connection
            .send_line(&format!("set-position: {}", position))
            .await
    }

    async fn set_paused(&self, paused: bool) -> anyhow::Result<()> {
        let target = if paused { "paused" } else { "playing" };
        if !paused {
            *self.last_position_update.lock() = Some(Instant::now());
        }
        self.state.lock().paused = Some(paused);
        self.connection
            .send_line(&format!("set-playstate: {}", target))
            .await
    }

    async fn set_speed(&self, speed: f64) -> anyhow::Result<()> {
        self.connection
            .send_line(&format!("set-rate: {:.2}", speed))
            .await
    }

    async fn load_file(&self, path: &str) -> anyhow::Result<()> {
        let arg = if is_ascii_path(path) && !is_url(path) {
            path.to_string()
        } else {
            build_mrl(path)
        };
        *self.last_loaded.lock() = Some(path.to_string());
        self.connection
            .send_line(&format!("load-file: {}", arg))
            .await
    }

    fn show_osd(&self, text: &str, duration_ms: Option<u64>) -> anyhow::Result<()> {
        let duration = duration_ms.unwrap_or(3000) as f64 / 1000.0;
        let message = text.replace('"', "'");
        let command = format!("display-osd: top-right, {}, {}", duration, message);
        let connection = self.connection.clone();
        tokio::spawn(async move {
            let _ = connection.send_line(&command).await;
        });
        Ok(())
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        self.connection.send_line("close-vlc").await
    }
}

fn spawn_reader(
    connection: Connection,
    read_half: OwnedReadHalf,
    state: Arc<Mutex<PlayerState>>,
    last_position_update: Arc<Mutex<Option<Instant>>>,
    last_duration: Arc<Mutex<Option<f64>>>,
    _last_loaded: Arc<Mutex<Option<String>>>,
    position_history: Arc<Mutex<VlcPositionHistory>>,
) {
    tokio::spawn(async move {
        let reader = BufReader::new(read_half);
        let mut lines = FramedRead::new(reader, LinesCodec::new());
        while let Some(Ok(line)) = lines.next().await {
            if line.trim().is_empty() {
                continue;
            }
            handle_line(
                &connection,
                &state,
                &last_position_update,
                &last_duration,
                &position_history,
                &line,
            )
            .await;
        }
    });
}

async fn handle_line(
    connection: &Connection,
    state: &Arc<Mutex<PlayerState>>,
    last_position_update: &Arc<Mutex<Option<Instant>>>,
    last_duration: &Arc<Mutex<Option<f64>>>,
    position_history: &Arc<Mutex<VlcPositionHistory>>,
    line: &str,
) {
    debug!("vlc >> {}", line);
    if line == "filepath-change-notification" {
        let _ = connection.send_line("get-duration").await;
        let _ = connection.send_line("get-filepath").await;
        let _ = connection.send_line("get-filename").await;
        return;
    }

    let (command, argument) = parse_line(line);
    match command.as_str() {
        "playstate" if !argument.is_empty() => {
            let mut paused = argument != "playing";
            if !paused
                && should_treat_vlc_playing_as_eof_pause(
                    state,
                    last_position_update,
                    position_history,
                )
            {
                paused = true;
                let _ = connection.send_line("set-playstate: paused").await;
            }
            state.lock().paused = Some(paused);
        }
        "position" => {
            if argument != "no-input" {
                if let Ok(pos) = argument.replace(',', ".").parse::<f64>() {
                    if should_ignore_duplicate_vlc_position(state, position_history, pos) {
                        return;
                    }
                    store_vlc_position(state, position_history, pos);
                    *last_position_update.lock() = Some(Instant::now());
                }
            } else {
                state.lock().position = None;
            }
        }
        "duration" | "duration-change" => {
            if argument == "no-input" {
                state.lock().duration = None;
            } else if argument == "invalid-32-bit-value" {
                warn!("VLC reported invalid duration value");
                state.lock().duration = None;
            } else if let Ok(value) = argument.replace(',', ".").parse::<f64>() {
                state.lock().duration = Some(value);
                *last_duration.lock() = Some(value);
            }
        }
        "filepath" => {
            if argument == "no-input" {
                state.lock().path = None;
            } else {
                let mut value = argument.clone();
                if value.starts_with("file://") {
                    value = value.trim_start_matches("file://").to_string();
                    if !Path::new(&value).exists() {
                        value = value.trim_start_matches('/').to_string();
                    }
                } else if is_url(&value) {
                    value = urlencoding::decode(&value)
                        .unwrap_or_else(|_| value.clone().into())
                        .to_string();
                }
                state.lock().path = Some(value.clone());
                state.lock().filename = Path::new(&value)
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string());
            }
        }
        "filename" if argument != "no-input" => {
            state.lock().filename = Some(argument.clone());
        }
        "inputstate-change" if argument == "no-input" => {
            let mut guard = state.lock();
            guard.path = None;
            guard.filename = None;
            guard.duration = None;
            guard.position = None;
        }
        "vlc-version" if !meets_min_version(&argument, VLC_MIN_VERSION) => {
            warn!(
                "VLC version {} is below minimum {}",
                argument, VLC_MIN_VERSION
            );
        }
        _ => {}
    }
}

fn should_treat_vlc_playing_as_eof_pause(
    state: &Arc<Mutex<PlayerState>>,
    last_position_update: &Arc<Mutex<Option<Instant>>>,
    position_history: &Arc<Mutex<VlcPositionHistory>>,
) -> bool {
    let snapshot = state.lock().clone();
    let history = position_history.lock().clone();
    let diff = last_position_update
        .lock()
        .map(|instant| instant.elapsed().as_secs_f64())
        .unwrap_or(0.0);
    matches!(
        (snapshot.position, snapshot.duration),
        (Some(position), Some(duration))
            if position == history.previous_previous
                && history.previous == position
                && duration > 10.0
                && duration - position < 2.0
                && diff > VLC_LATENCY_ERROR_THRESHOLD
    )
}

fn should_ignore_duplicate_vlc_position(
    state: &Arc<Mutex<PlayerState>>,
    position_history: &Arc<Mutex<VlcPositionHistory>>,
    new_position: f64,
) -> bool {
    let snapshot = state.lock().clone();
    let history = position_history.lock().clone();
    if snapshot.paused != Some(false) {
        return false;
    }
    if Some(new_position) == snapshot.duration {
        return false;
    }
    if new_position != history.previous {
        return false;
    }
    let mut history = position_history.lock();
    history.previous_previous = history.previous;
    if let Some(position) = snapshot.position {
        history.previous = position;
    }
    true
}

fn store_vlc_position(
    state: &Arc<Mutex<PlayerState>>,
    position_history: &Arc<Mutex<VlcPositionHistory>>,
    position: f64,
) {
    let previous_position = state.lock().position;
    {
        let mut history = position_history.lock();
        history.previous_previous = history.previous;
        if let Some(previous_position) = previous_position {
            history.previous = previous_position;
        }
    }
    state.lock().position = Some(position);
}

fn parse_line(line: &str) -> (String, String) {
    if let Some((cmd, arg)) = line.split_once(": ") {
        (cmd.trim().to_string(), arg.trim().to_string())
    } else if let Some((cmd, arg)) = line.split_once(':') {
        (cmd.trim().to_string(), arg.trim().to_string())
    } else {
        (line.trim().to_string(), String::new())
    }
}

fn pick_vlc_port() -> u16 {
    let mut rng = rand::thread_rng();
    rng.gen_range(VLC_MIN_PORT..=VLC_MAX_PORT)
}

async fn connect_with_retry(port: u16) -> anyhow::Result<TcpStream> {
    let start = Instant::now();
    loop {
        match TcpStream::connect(("127.0.0.1", port)).await {
            Ok(stream) => return Ok(stream),
            Err(_) => {
                if start.elapsed() >= VLC_OPEN_MAX_WAIT_TIME {
                    return Err(anyhow::anyhow!(
                        "Failed to connect to VLC syncplay interface"
                    ));
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        }
    }
}

#[allow(unused_variables)]
fn build_vlc_extra_args(player_path: &str) -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        vec!["--verbose=2".to_string(), "--no-file-logging".to_string()]
    }
    #[cfg(not(target_os = "macos"))]
    {
        if player_path.to_ascii_lowercase().contains("vlcportable.exe") {
            Vec::new()
        } else {
            vec![
                "--no-one-instance".to_string(),
                "--no-one-instance-when-started-from-file".to_string(),
            ]
        }
    }
}

#[allow(unused_variables)]
fn resolve_vlc_paths(player_path: &str) -> anyhow::Result<(String, String)> {
    let player_path_str = player_path;
    let player_path = Path::new(player_path);
    #[cfg(target_os = "linux")]
    {
        if player_path_str.contains("snap") {
            let intf = "/snap/vlc/current/usr/lib/vlc/lua/intf/".to_string();
            let user = format!(
                "{}/snap/vlc/current/.local/share/vlc/lua/intf/",
                std::env::var("HOME").unwrap_or_default()
            );
            Ok((intf, user))
        } else {
            let intf = "/usr/lib/vlc/lua/intf/".to_string();
            let user = format!(
                "{}/.local/share/vlc/lua/intf/",
                std::env::var("HOME").unwrap_or_default()
            );
            Ok((intf, user))
        }
    }

    #[cfg(target_os = "macos")]
    {
        let intf = "/Applications/VLC.app/Contents/MacOS/share/lua/intf/".to_string();
        let user = format!(
            "{}/Library/Application Support/org.videolan.vlc/lua/intf/",
            std::env::var("HOME").unwrap_or_default()
        );
        Ok((intf, user))
    }

    #[cfg(target_os = "windows")]
    {
        let player_path_str = player_path.to_string_lossy().to_string();
        let lower = player_path_str.to_ascii_lowercase();
        if lower.contains("vlcportable.exe") {
            let base = player_path
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .to_path_buf();
            let intf = base.join("App/vlc/lua/intf/").to_string_lossy().to_string();
            Ok((intf.clone(), intf))
        } else {
            let base = player_path
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .to_path_buf();
            let intf = base.join("lua/intf/").to_string_lossy().to_string();
            let appdata = std::env::var("APPDATA").unwrap_or_default();
            let user = Path::new(&appdata)
                .join("VLC/lua/intf/")
                .to_string_lossy()
                .to_string();
            Ok((intf, user))
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let intf = "/usr/local/lib/vlc/lua/intf/".to_string();
        let user = format!(
            "{}/.local/share/vlc/lua/intf/",
            std::env::var("HOME").unwrap_or_default()
        );
        Ok((intf, user))
    }
}

fn install_syncplay_lua(target_dir: &str, source_path: &Path) -> anyhow::Result<()> {
    let target_path = Path::new(target_dir);
    std::fs::create_dir_all(target_path)?;
    let destination = target_path.join("syncplay.lua");
    std::fs::copy(source_path, &destination)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o755));
    }
    Ok(())
}

fn is_ascii_path(path: &str) -> bool {
    path.is_ascii()
}

fn is_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

fn build_mrl(path: &str) -> String {
    if is_url(path) {
        return encode_with_safe(path, b":/?&=#+!$,;'@()*%~");
    }
    let mut value = path.replace('\\', "/");
    value = encode_with_safe(&value, b"/:");
    if cfg!(target_os = "windows") {
        format!("file:///{}", value)
    } else {
        format!("file://{}", value)
    }
}

fn encode_with_safe(input: &str, safe: &[u8]) -> String {
    let mut output = String::new();
    for b in input.as_bytes() {
        let c = *b;
        if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b'.' || safe.contains(&c) {
            output.push(c as char);
        } else if c == b' ' {
            output.push_str("%20");
        } else {
            output.push_str(&format!("%{:02X}", c));
        }
    }
    output
}

fn meets_min_version(version: &str, min: &str) -> bool {
    let parse = |value: &str| -> Vec<i32> {
        value
            .split('.')
            .map(|part| part.parse::<i32>().unwrap_or(0))
            .collect()
    };
    let v = parse(version);
    let m = parse(min);
    let max_len = v.len().max(m.len());
    for idx in 0..max_len {
        let va = *v.get(idx).unwrap_or(&0);
        let ma = *m.get(idx).unwrap_or(&0);
        if va > ma {
            return true;
        }
        if va < ma {
            return false;
        }
    }
    true
}
