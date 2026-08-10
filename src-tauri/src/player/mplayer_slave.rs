use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, info, warn};

use super::backend::{PlayerBackend, PlayerKind};
use super::media_update::{
    MediaMetadataField, MediaRefreshOutcome, MediaSnapshot, OrderedMediaRefresh,
};
use super::properties::PlayerState;

const MPLAYER_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);
const MPLAYER_MEDIA_PROPERTY_RETRY_LIMIT: u8 = 3;
const MPLAYER_OSD_LEVEL: u8 = 1;
const MPLAYER_STATUS_QUERY_COMMANDS: [&str; 2] = ["get_property pause", "get_property time_pos"];
const MPLAYER_ARGS: &[&str] = &[
    "-slave",
    "--hr-seek=always",
    "-nomsgcolor",
    "-msglevel",
    "all=1:global=4:cplayer=4",
    "-af-add",
    "scaletempo",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ResponseKey {
    Position,
    Duration,
    Filename,
    Path,
    Pause,
    Speed,
}

pub struct MplayerBackend {
    kind: PlayerKind,
    stdin: Arc<TokioMutex<Option<ChildStdin>>>,
    state: Arc<Mutex<PlayerState>>,
    media_refresh: Arc<Mutex<OrderedMediaRefresh>>,
    connected: Arc<AtomicBool>,
}

impl MplayerBackend {
    pub async fn start(
        player_path: &str,
        args: &[String],
        initial_file: Option<&str>,
    ) -> anyhow::Result<(Self, Child)> {
        info!(
            "Starting player: kind=Mplayer, path={}, args={:?}, initial_file={:?}",
            player_path, args, initial_file
        );
        #[allow(unused_mut)]
        let mut launch_file = initial_file.map(|s| s.to_string());
        #[allow(unused_mut)]
        let mut delayed_file: Option<String> = None;
        #[cfg(windows)]
        {
            if let Some(path) = launch_file.as_ref() {
                if !path.is_ascii() {
                    delayed_file = launch_file.take();
                }
            }
        }

        let mut cmd = Command::new(player_path);
        cmd.kill_on_drop(true);
        let launch_arguments = build_launch_arguments(args, launch_file.as_deref());
        cmd.args(launch_arguments);
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
        if let Some(working_directory) =
            mplayer_working_directory(launch_file.as_deref(), home.as_deref(), appdata.as_deref())
        {
            cmd.current_dir(working_directory);
        }
        cmd.env_remove("TERM");
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().context("Failed to start MPlayer")?;
        let stdin = child
            .stdin
            .take()
            .context("Failed to capture MPlayer stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("Failed to capture MPlayer stdout")?;
        let stderr = child
            .stderr
            .take()
            .context("Failed to capture MPlayer stderr")?;

        let state = Arc::new(Mutex::new(PlayerState {
            paused: None,
            ..PlayerState::default()
        }));
        let stdin = Arc::new(TokioMutex::new(Some(stdin)));
        let connected = Arc::new(AtomicBool::new(true));
        let media_refresh = Arc::new(Mutex::new(OrderedMediaRefresh::new([
            MediaMetadataField::Filename,
            MediaMetadataField::Duration,
            MediaMetadataField::Path,
        ])));
        let state_clone = state.clone();
        let connected_clone = connected.clone();
        let stdin_clone = stdin.clone();
        let media_refresh_clone = media_refresh.clone();

        tokio::spawn(async move {
            let mut stdout_lines = BufReader::new(stdout).lines();
            let mut stderr_lines = BufReader::new(stderr).lines();
            let mut stdout_open = true;
            let mut stderr_open = true;
            let mut media_property_retries = HashMap::<ResponseKey, u8>::new();

            while stdout_open || stderr_open {
                let (from_stdout, result) = tokio::select! {
                    result = stdout_lines.next_line(), if stdout_open => (true, result),
                    result = stderr_lines.next_line(), if stderr_open => (false, result),
                };
                match result {
                    Ok(Some(line)) => {
                        let outcome = handle_line(&state_clone, &media_refresh_clone, &line);
                        if let Some(field) = outcome.completed_media_field {
                            media_property_retries.remove(&field);
                        }
                        if let Some(field) = outcome.failed_media_field {
                            let retry_result = match record_media_property_failure(
                                &mut media_property_retries,
                                field,
                            ) {
                                MediaPropertyFailureAction::Retry => {
                                    send_media_query_command(&stdin_clone, &connected_clone, field)
                                        .await
                                }
                                MediaPropertyFailureAction::SettleUnavailable => {
                                    match apply_media_field_response(
                                        &state_clone,
                                        &media_refresh_clone,
                                        field,
                                        None,
                                    ) {
                                        MediaRefreshOutcome::Pending => {
                                            send_next_media_query_command(
                                                &stdin_clone,
                                                &connected_clone,
                                                field,
                                            )
                                            .await
                                        }
                                        MediaRefreshOutcome::Restarted => {
                                            media_property_retries.clear();
                                            start_media_query_cycle(&stdin_clone, &connected_clone)
                                                .await
                                        }
                                        MediaRefreshOutcome::Ignored
                                        | MediaRefreshOutcome::Committed(_) => Ok(()),
                                    }
                                }
                            };
                            if let Err(error) = retry_result {
                                media_refresh_clone.lock().abort();
                                warn!("Failed to recover MPlayer media metadata: {}", error);
                            }
                        }
                        if let Some(field) = outcome.next_media_field {
                            if let Err(error) =
                                send_media_query_command(&stdin_clone, &connected_clone, field)
                                    .await
                            {
                                media_refresh_clone.lock().abort();
                                warn!("Failed to continue MPlayer media metadata: {}", error);
                            }
                        }
                        if outcome.restart_media_refresh {
                            media_property_retries.clear();
                            if let Err(error) =
                                start_media_query_cycle(&stdin_clone, &connected_clone).await
                            {
                                media_refresh_clone.lock().abort();
                                warn!(
                                    "Failed to restart MPlayer media metadata refresh: {}",
                                    error
                                );
                            }
                        }
                        if let Some(reason) = outcome.exit_reason {
                            if reason.eq_ignore_ascii_case("Quit") {
                                debug!("MPlayer exited normally");
                            } else {
                                warn!("MPlayer exited: {}", reason);
                            }
                            break;
                        }
                    }
                    Ok(None) => {
                        if from_stdout {
                            stdout_open = false;
                        } else {
                            stderr_open = false;
                        }
                    }
                    Err(error) => {
                        let stream = if from_stdout { "stdout" } else { "stderr" };
                        warn!("Failed to read MPlayer {}: {}", stream, error);
                        if from_stdout {
                            stdout_open = false;
                        } else {
                            stderr_open = false;
                        }
                    }
                }
            }
            connected_clone.store(false, Ordering::SeqCst);
        });

        let backend = Self {
            kind: PlayerKind::Mplayer,
            stdin,
            state,
            media_refresh,
            connected,
        };

        if let Some(path) = delayed_file {
            let _ = backend.load_file(&path).await;
        } else if initial_file.is_some() {
            backend.request_media_refresh().await?;
        }

        Ok((backend, child))
    }

    async fn send_command(&self, command: &str) -> anyhow::Result<()> {
        send_mplayer_command(&self.stdin, &self.connected, command).await
    }

    async fn request_media_refresh(&self) -> anyhow::Result<()> {
        let should_send = {
            let mut refresh = self.media_refresh.lock();
            refresh.start_if_idle()
        };
        if !should_send {
            return Ok(());
        }
        if let Err(error) = start_media_query_cycle(&self.stdin, &self.connected).await {
            self.media_refresh.lock().abort();
            return Err(error);
        }
        Ok(())
    }

    async fn close(&self) -> anyhow::Result<()> {
        self.connected.store(false, Ordering::SeqCst);
        let mut guard = self.stdin.lock().await;
        if let Some(mut stdin) = guard.take() {
            let _ = tokio::time::timeout(MPLAYER_COMMAND_TIMEOUT, async {
                let _ = stdin.write_all(b"quit\n").await;
                stdin.shutdown().await
            })
            .await;
        }
        Ok(())
    }
}

fn build_launch_arguments(user_args: &[String], initial_file: Option<&str>) -> Vec<String> {
    initial_file
        .into_iter()
        .map(str::to_string)
        .chain(user_args.iter().cloned())
        .chain(MPLAYER_ARGS.iter().map(|argument| (*argument).to_string()))
        .collect()
}

fn mplayer_working_directory(
    initial_media: Option<&str>,
    home: Option<&Path>,
    appdata: Option<&Path>,
) -> Option<PathBuf> {
    let media = Path::new(initial_media?);
    if media.is_file() {
        return media.parent().map(Path::to_path_buf);
    }
    home.or(appdata).map(Path::to_path_buf)
}

async fn send_mplayer_command(
    stdin: &Arc<TokioMutex<Option<ChildStdin>>>,
    connected: &Arc<AtomicBool>,
    command: &str,
) -> anyhow::Result<()> {
    if !connected.load(Ordering::SeqCst) {
        anyhow::bail!("MPlayer slave pipe is disconnected");
    }
    let mut guard = stdin.lock().await;
    let Some(stdin) = guard.as_mut() else {
        connected.store(false, Ordering::SeqCst);
        anyhow::bail!("MPlayer slave pipe is disconnected");
    };
    match tokio::time::timeout(MPLAYER_COMMAND_TIMEOUT, async {
        stdin.write_all(format!("{}\n", command).as_bytes()).await?;
        stdin.flush().await
    })
    .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => {
            connected.store(false, Ordering::SeqCst);
            *guard = None;
            Err(error.into())
        }
        Err(_) => {
            connected.store(false, Ordering::SeqCst);
            *guard = None;
            anyhow::bail!("Timed out writing to MPlayer slave pipe")
        }
    }
}

async fn start_media_query_cycle(
    stdin: &Arc<TokioMutex<Option<ChildStdin>>>,
    connected: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    send_media_query_command(stdin, connected, ResponseKey::Filename).await
}

async fn send_next_media_query_command(
    stdin: &Arc<TokioMutex<Option<ChildStdin>>>,
    connected: &Arc<AtomicBool>,
    completed_field: ResponseKey,
) -> anyhow::Result<()> {
    if let Some(field) = next_media_field(completed_field) {
        send_media_query_command(stdin, connected, field).await?;
    }
    Ok(())
}

async fn send_media_query_command(
    stdin: &Arc<TokioMutex<Option<ChildStdin>>>,
    connected: &Arc<AtomicBool>,
    field: ResponseKey,
) -> anyhow::Result<()> {
    let property = match field {
        ResponseKey::Filename => "filename",
        ResponseKey::Duration => "length",
        ResponseKey::Path => "path",
        _ => anyhow::bail!("Not an MPlayer media property: {field:?}"),
    };
    send_mplayer_command(stdin, connected, &format!("get_property {property}")).await
}

#[derive(Debug, Default, PartialEq, Eq)]
struct MplayerLineOutcome {
    exit_reason: Option<String>,
    restart_media_refresh: bool,
    failed_media_field: Option<ResponseKey>,
    completed_media_field: Option<ResponseKey>,
    next_media_field: Option<ResponseKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaPropertyFailureAction {
    Retry,
    SettleUnavailable,
}

fn record_media_property_failure(
    retries: &mut HashMap<ResponseKey, u8>,
    field: ResponseKey,
) -> MediaPropertyFailureAction {
    let failures = retries.entry(field).or_default();
    *failures = failures.saturating_add(1);
    if *failures <= MPLAYER_MEDIA_PROPERTY_RETRY_LIMIT {
        MediaPropertyFailureAction::Retry
    } else {
        retries.remove(&field);
        MediaPropertyFailureAction::SettleUnavailable
    }
}

fn handle_line(
    state: &Arc<Mutex<PlayerState>>,
    media_refresh: &Arc<Mutex<OrderedMediaRefresh>>,
    line: &str,
) -> MplayerLineOutcome {
    let line = normalize_output_line(line);
    if line.is_empty() {
        return MplayerLineOutcome::default();
    }
    debug!("mplayer >> {}", line);
    if let Some(reason) = parse_exit_reason(&line) {
        return MplayerLineOutcome {
            exit_reason: Some(reason),
            restart_media_refresh: false,
            failed_media_field: None,
            completed_media_field: None,
            next_media_field: None,
        };
    }
    if let Some(field) = parse_failed_media_field(&line) {
        return MplayerLineOutcome {
            failed_media_field: Some(field),
            ..MplayerLineOutcome::default()
        };
    }
    if let Some((key, value)) = parse_response(&line) {
        match key {
            ResponseKey::Duration | ResponseKey::Filename | ResponseKey::Path => {
                let outcome = apply_media_field_response(state, media_refresh, key, value);
                return MplayerLineOutcome {
                    restart_media_refresh: matches!(outcome, MediaRefreshOutcome::Restarted),
                    completed_media_field: (!matches!(outcome, MediaRefreshOutcome::Ignored))
                        .then_some(key),
                    next_media_field: matches!(outcome, MediaRefreshOutcome::Pending)
                        .then(|| next_media_field(key))
                        .flatten(),
                    ..MplayerLineOutcome::default()
                };
            }
            ResponseKey::Position => {
                if let Some(position) = value.and_then(|value| value.parse::<f64>().ok()) {
                    state.lock().observe_position(Some(position));
                }
            }
            ResponseKey::Pause => {
                let paused = value.as_deref().and_then(|value| match value.trim() {
                    "yes" | "true" | "1" => Some(true),
                    "no" | "false" | "0" => Some(false),
                    _ => None,
                });
                if paused.is_some() {
                    state.lock().observe_paused(paused);
                }
            }
            ResponseKey::Speed => {
                if let Some(speed) = value.and_then(|value| value.parse::<f64>().ok()) {
                    state.lock().speed = Some(speed);
                }
            }
        }
    }
    MplayerLineOutcome::default()
}

fn apply_media_field_response(
    state: &Arc<Mutex<PlayerState>>,
    media_refresh: &Arc<Mutex<OrderedMediaRefresh>>,
    key: ResponseKey,
    value: Option<String>,
) -> MediaRefreshOutcome {
    let outcome = {
        let mut refresh = media_refresh.lock();
        match key {
            ResponseKey::Duration => {
                refresh.push_duration(value.as_deref().and_then(|value| value.parse::<f64>().ok()))
            }
            ResponseKey::Filename => refresh.push_filename(value),
            ResponseKey::Path => refresh.push_path(value),
            _ => return MediaRefreshOutcome::Ignored,
        }
    };
    if let MediaRefreshOutcome::Committed(snapshot) = &outcome {
        apply_media_snapshot(&mut state.lock(), snapshot.clone());
    }
    outcome
}

fn next_media_field(field: ResponseKey) -> Option<ResponseKey> {
    match field {
        ResponseKey::Filename => Some(ResponseKey::Duration),
        ResponseKey::Duration => Some(ResponseKey::Path),
        ResponseKey::Path => None,
        _ => None,
    }
}

fn parse_failed_media_field(line: &str) -> Option<ResponseKey> {
    let line = line.to_ascii_lowercase();
    if !line.contains("failed to get value of property") {
        return None;
    }
    if line.contains("filename") {
        Some(ResponseKey::Filename)
    } else if line.contains("length") {
        Some(ResponseKey::Duration)
    } else if line.contains("path") {
        Some(ResponseKey::Path)
    } else {
        None
    }
}

fn parse_response(line: &str) -> Option<(ResponseKey, Option<String>)> {
    let line = line.trim();
    if !line.get(..4)?.eq_ignore_ascii_case("ANS_") {
        return None;
    }
    let (key, value) = line[4..].split_once('=')?;
    let value = value
        .trim()
        .trim_matches(|character| character == '"' || character == '\'')
        .to_string();
    let value =
        (!value.is_empty() && !value.eq_ignore_ascii_case("(unavailable)")).then_some(value);

    let key = match key.to_ascii_lowercase().as_str() {
        "time_position" | "time_pos" => ResponseKey::Position,
        "length" | "time_length" => ResponseKey::Duration,
        "filename" | "file_name" => ResponseKey::Filename,
        "path" => ResponseKey::Path,
        "pause" => ResponseKey::Pause,
        "speed" => ResponseKey::Speed,
        _ => return None,
    };
    Some((key, value))
}

fn apply_media_snapshot(state: &mut PlayerState, snapshot: MediaSnapshot) {
    state.filename = snapshot.filename;
    state.path = snapshot.path;
    state.duration = snapshot.duration;
}

fn parse_exit_reason(line: &str) -> Option<String> {
    line.strip_prefix("Exiting... (")
        .and_then(|reason| reason.strip_suffix(')'))
        .map(str::to_string)
}

fn normalize_output_line(line: &str) -> String {
    let mut normalized = strip_ansi_sequences(line);
    for prefix in ["[cplayer] ", "[term-msg] ", "   cplayer: ", "  term-msg: "] {
        normalized = normalized.replace(prefix, "");
    }
    normalized.trim().to_string()
}

fn strip_ansi_sequences(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut characters = input.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\u{1b}' {
            output.push(character);
            continue;
        }

        match characters.next() {
            Some('[') => {
                for code in characters.by_ref() {
                    if ('@'..='~').contains(&code) {
                        break;
                    }
                }
            }
            Some(']') => {
                let mut escaped = false;
                for code in characters.by_ref() {
                    if code == '\u{7}' || (escaped && code == '\\') {
                        break;
                    }
                    escaped = code == '\u{1b}';
                }
            }
            Some(_) | None => {}
        }
    }
    output
}

#[async_trait]
impl PlayerBackend for MplayerBackend {
    fn kind(&self) -> PlayerKind {
        self.kind
    }

    fn name(&self) -> &'static str {
        "MPlayer"
    }

    fn get_state(&self) -> PlayerState {
        self.state.lock().clone()
    }

    async fn poll_state(&self) -> anyhow::Result<()> {
        for command in MPLAYER_STATUS_QUERY_COMMANDS {
            if let Err(error) = self.send_command(command).await {
                warn!("Failed to request MPlayer status with {command:?}: {error}");
            }
        }
        Ok(())
    }

    async fn set_position(&self, position: f64) -> anyhow::Result<()> {
        self.state.lock().position = Some(position.max(0.0));
        self.send_command(&set_position_command(position)).await
    }

    async fn set_paused(&self, paused: bool) -> anyhow::Result<()> {
        let current = self.state.lock().paused.unwrap_or(false);
        if paused != current {
            self.send_command("pause").await?;
            self.state.lock().paused = Some(paused);
        } else {
            return Ok(());
        }
        Ok(())
    }

    async fn set_speed(&self, speed: f64) -> anyhow::Result<()> {
        self.send_command(&format!("set_property speed {}", speed))
            .await
    }

    async fn load_file(&self, path: &str) -> anyhow::Result<()> {
        let should_send_refresh = self.media_refresh.lock().restart_after_active();
        if let Err(error) = self.send_command(&load_file_command(path)).await {
            self.media_refresh.lock().abort();
            return Err(error);
        }
        if !should_send_refresh {
            return Ok(());
        }
        if let Err(error) = start_media_query_cycle(&self.stdin, &self.connected).await {
            self.media_refresh.lock().abort();
            return Err(error);
        }
        Ok(())
    }

    fn show_osd(&self, text: &str, duration_ms: Option<u64>) -> anyhow::Result<()> {
        let cmd = osd_command(text, duration_ms);
        let stdin = self.stdin.clone();
        tokio::spawn(async move {
            let mut guard = stdin.lock().await;
            if let Some(stdin) = guard.as_mut() {
                let _ = stdin.write_all(format!("{}\n", cmd).as_bytes()).await;
                let _ = stdin.flush().await;
            }
        });
        Ok(())
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        self.close().await
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}

fn set_position_command(position: f64) -> String {
    format!("set_property time_pos {}", position)
}

fn load_file_command(path: &str) -> String {
    format!("loadfile {}", quote_argument(path))
}

fn quote_argument(argument: &str) -> String {
    let argument = argument
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('"', "\\\"")
        .replace(['\r', '\n'], "");
    format!("\"{}\"", argument)
}

fn osd_command(text: &str, duration_ms: Option<u64>) -> String {
    format!(
        "osd_show_text \"{}\" {} {}",
        sanitize_osd_text(text),
        duration_ms.unwrap_or(3000),
        MPLAYER_OSD_LEVEL
    )
}

fn sanitize_osd_text(text: &str) -> String {
    const NEWLINE_PLACEHOLDER: &str = "<NEWLINE>";
    const QUOTE_PLACEHOLDER: &str = "<SYNCPLAY_QUOTE>";

    text.replace("\\n", NEWLINE_PLACEHOLDER)
        .replace(['\r', '\n'], "")
        .replace("\\\"", QUOTE_PLACEHOLDER)
        .replace('"', QUOTE_PLACEHOLDER)
        .replace('%', "%%")
        .replace('\\', "\\\\")
        .replace('{', "\\\\{")
        .replace('}', "\\\\}")
        .replace(QUOTE_PLACEHOLDER, "\\\"")
        .replace(NEWLINE_PLACEHOLDER, "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media_refresh() -> Arc<Mutex<OrderedMediaRefresh>> {
        Arc::new(Mutex::new(OrderedMediaRefresh::new([
            MediaMetadataField::Filename,
            MediaMetadataField::Duration,
            MediaMetadataField::Path,
        ])))
    }

    fn assert_media(state: &Arc<Mutex<PlayerState>>, name: &str, duration: f64, path: &str) {
        let state = state.lock();
        assert_eq!(state.filename.as_deref(), Some(name));
        assert_eq!(state.duration, Some(duration));
        assert_eq!(state.path.as_deref(), Some(path));
    }

    #[test]
    fn parser_normalizes_original_mplayer_prefixes_and_ansi_sequences() {
        let state = Arc::new(Mutex::new(PlayerState::default()));
        let media_refresh = media_refresh();
        assert!(media_refresh.lock().start_if_idle());

        let _ = handle_line(
            &state,
            &media_refresh,
            "\u{1b}[?1l\u{1b}>[cplayer] ANS_filename=Example.mkv",
        );
        let _ = handle_line(&state, &media_refresh, "ans_LENGTH=42");
        let _ = handle_line(
            &state,
            &media_refresh,
            "   cplayer: ANS_PATH=/tmp/Example.mkv",
        );

        assert_media(&state, "Example.mkv", 42.0, "/tmp/Example.mkv");
    }

    #[test]
    fn parser_maps_answer_keys_case_insensitively() {
        assert_eq!(
            parse_response("ANS_time_pos=12.5"),
            Some((ResponseKey::Position, Some("12.5".to_string())))
        );
        assert_eq!(
            parse_response("ans_LENGTH=42"),
            Some((ResponseKey::Duration, Some("42".to_string())))
        );
        assert_eq!(
            parse_response("ANS_FILE_NAME='Example.mkv'"),
            Some((ResponseKey::Filename, Some("Example.mkv".to_string())))
        );
        assert_eq!(
            parse_response("ANS_path=\"/tmp/Example.mkv\""),
            Some((ResponseKey::Path, Some("/tmp/Example.mkv".to_string())))
        );
        assert_eq!(
            parse_response("ANS_filename=(unavailable)"),
            Some((ResponseKey::Filename, None))
        );
    }

    #[test]
    fn parser_reports_mplayer_exit_reason() {
        let state = Arc::new(Mutex::new(PlayerState::default()));
        let media_refresh = media_refresh();

        assert_eq!(
            handle_line(&state, &media_refresh, "[cplayer] Exiting... (End of file)",),
            MplayerLineOutcome {
                exit_reason: Some("End of file".to_string()),
                restart_media_refresh: false,
                ..MplayerLineOutcome::default()
            }
        );
        assert_eq!(
            handle_line(&state, &media_refresh, "Exiting... (Quit)"),
            MplayerLineOutcome {
                exit_reason: Some("Quit".to_string()),
                restart_media_refresh: false,
                ..MplayerLineOutcome::default()
            }
        );
    }

    #[test]
    fn failed_media_property_retries_are_bounded_and_the_ordered_cycle_settles() {
        let state = Arc::new(Mutex::new(PlayerState::default()));
        let media_refresh = media_refresh();
        let mut retries = HashMap::new();
        assert!(media_refresh.lock().start_if_idle());

        for _ in 0..MPLAYER_MEDIA_PROPERTY_RETRY_LIMIT {
            let outcome = handle_line(
                &state,
                &media_refresh,
                "Failed to get value of property 'filename'",
            );
            assert_eq!(outcome.failed_media_field, Some(ResponseKey::Filename));
            assert_eq!(
                record_media_property_failure(&mut retries, ResponseKey::Filename),
                MediaPropertyFailureAction::Retry
            );
        }

        let outcome = handle_line(
            &state,
            &media_refresh,
            "Failed to get value of property 'filename'",
        );
        assert_eq!(outcome.failed_media_field, Some(ResponseKey::Filename));
        assert_eq!(
            record_media_property_failure(&mut retries, ResponseKey::Filename),
            MediaPropertyFailureAction::SettleUnavailable
        );
        assert!(matches!(
            apply_media_field_response(&state, &media_refresh, ResponseKey::Filename, None,),
            MediaRefreshOutcome::Pending
        ));
        assert_eq!(
            next_media_field(ResponseKey::Filename),
            Some(ResponseKey::Duration)
        );

        let duration = handle_line(&state, &media_refresh, "ANS_length=42");
        assert_eq!(duration.next_media_field, Some(ResponseKey::Path));
        let path = handle_line(&state, &media_refresh, "ANS_path=/media/example.mkv");
        assert_eq!(path.next_media_field, None);
        let state = state.lock();
        assert_eq!(state.filename, None);
        assert_eq!(state.duration, Some(42.0));
        assert_eq!(state.path.as_deref(), Some("/media/example.mkv"));
        drop(state);
        assert!(media_refresh.lock().start_if_idle());
    }

    #[test]
    fn stale_queued_responses_cannot_starve_fields_after_a_filename_retry() {
        let state = Arc::new(Mutex::new(PlayerState::default()));
        let media_refresh = media_refresh();
        assert!(media_refresh.lock().start_if_idle());

        let failure = handle_line(
            &state,
            &media_refresh,
            "Failed to get value of property 'filename'",
        );
        assert_eq!(failure.failed_media_field, Some(ResponseKey::Filename));

        let stale_duration = handle_line(&state, &media_refresh, "ANS_length=1");
        let stale_path = handle_line(&state, &media_refresh, "ANS_path=/stale/movie.mkv");
        assert_eq!(stale_duration.completed_media_field, None);
        assert_eq!(stale_duration.next_media_field, None);
        assert_eq!(stale_path.completed_media_field, None);
        assert_eq!(stale_path.next_media_field, None);

        let filename = handle_line(&state, &media_refresh, "ANS_filename=movie.mkv");
        assert_eq!(filename.next_media_field, Some(ResponseKey::Duration));
        let duration = handle_line(&state, &media_refresh, "ANS_length=120");
        assert_eq!(duration.next_media_field, Some(ResponseKey::Path));
        let path = handle_line(&state, &media_refresh, "ANS_path=/media/movie.mkv");
        assert_eq!(path.next_media_field, None);
        assert_media(&state, "movie.mkv", 120.0, "/media/movie.mkv");
        assert!(media_refresh.lock().start_if_idle());
    }

    #[test]
    fn failed_media_property_parser_identifies_each_original_property_name() {
        assert_eq!(
            parse_failed_media_field("Failed to get value of property 'filename'"),
            Some(ResponseKey::Filename)
        );
        assert_eq!(
            parse_failed_media_field("Failed to get value of property 'length'"),
            Some(ResponseKey::Duration)
        );
        assert_eq!(
            parse_failed_media_field("Failed to get value of property 'path'"),
            Some(ResponseKey::Path)
        );
    }

    #[test]
    fn interleaved_file_change_never_exposes_mixed_mplayer_metadata() {
        let state = Arc::new(Mutex::new(PlayerState::default()));
        let media_refresh = media_refresh();

        assert!(media_refresh.lock().start_if_idle());
        let _ = handle_line(&state, &media_refresh, "ANS_filename=A.mkv");
        let _ = handle_line(&state, &media_refresh, "ANS_length=100");
        let _ = handle_line(&state, &media_refresh, "ANS_path=/media/A.mkv");
        assert_media(&state, "A.mkv", 100.0, "/media/A.mkv");

        assert!(media_refresh.lock().start_if_idle());
        let _ = handle_line(&state, &media_refresh, "ANS_filename=A.mkv");
        assert!(!media_refresh.lock().restart_after_active());
        let _ = handle_line(&state, &media_refresh, "ANS_length=200");
        let outcome = handle_line(&state, &media_refresh, "ANS_path=/media/B.mkv");
        assert!(outcome.restart_media_refresh);
        assert_media(&state, "A.mkv", 100.0, "/media/A.mkv");

        let _ = handle_line(&state, &media_refresh, "ANS_filename=B.mkv");
        assert_media(&state, "A.mkv", 100.0, "/media/A.mkv");
        let _ = handle_line(&state, &media_refresh, "ANS_length=200");
        assert_media(&state, "A.mkv", 100.0, "/media/A.mkv");
        let _ = handle_line(&state, &media_refresh, "ANS_path=/media/B.mkv");
        assert_media(&state, "B.mkv", 200.0, "/media/B.mkv");
    }

    #[test]
    fn no_input_clears_mplayer_metadata_atomically() {
        let state = Arc::new(Mutex::new(PlayerState {
            filename: Some("A.mkv".to_string()),
            duration: Some(100.0),
            path: Some("/media/A.mkv".to_string()),
            ..PlayerState::default()
        }));
        let media_refresh = media_refresh();
        assert!(media_refresh.lock().start_if_idle());

        let _ = handle_line(&state, &media_refresh, "ANS_filename=");
        let _ = handle_line(&state, &media_refresh, "ANS_length=(unavailable)");
        assert_media(&state, "A.mkv", 100.0, "/media/A.mkv");
        let _ = handle_line(&state, &media_refresh, "ANS_path=");

        let state = state.lock();
        assert_eq!(state.filename, None);
        assert_eq!(state.duration, None);
        assert_eq!(state.path, None);
    }

    #[test]
    fn set_position_command_matches_original_mplayer_property_set() {
        assert_eq!(set_position_command(12.5), "set_property time_pos 12.5");
    }

    #[test]
    fn initial_file_precedes_user_and_slave_arguments() {
        assert_eq!(
            build_launch_arguments(&["-profile".into(), "cinema".into()], Some("movie.mkv")),
            vec![
                "movie.mkv",
                "-profile",
                "cinema",
                "-slave",
                "--hr-seek=always",
                "-nomsgcolor",
                "-msglevel",
                "all=1:global=4:cplayer=4",
                "-af-add",
                "scaletempo",
            ]
        );
    }

    #[test]
    fn initial_local_media_uses_its_parent_as_mplayer_working_directory() {
        let directory = tempfile::TempDir::new().unwrap();
        let media = directory.path().join("movie.mkv");
        std::fs::write(&media, b"media").unwrap();

        assert_eq!(
            mplayer_working_directory(
                media.to_str(),
                Some(Path::new("/fallback-home")),
                Some(Path::new("/fallback-appdata")),
            ),
            Some(directory.path().to_path_buf())
        );
    }

    #[test]
    fn non_file_initial_media_prefers_home_then_appdata() {
        let url = Some("https://example.com/movie.mkv");
        assert_eq!(
            mplayer_working_directory(
                url,
                Some(Path::new("/home/example")),
                Some(Path::new("/appdata/example")),
            ),
            Some(PathBuf::from("/home/example"))
        );
        assert_eq!(
            mplayer_working_directory(url, None, Some(Path::new("/appdata/example"))),
            Some(PathBuf::from("/appdata/example"))
        );
        assert_eq!(mplayer_working_directory(url, None, None), None);
    }

    #[test]
    fn delayed_or_missing_launch_file_does_not_set_mplayer_working_directory() {
        let delayed_launch_file = None;
        assert_eq!(
            mplayer_working_directory(
                delayed_launch_file,
                Some(Path::new("/home/example")),
                Some(Path::new("/appdata/example")),
            ),
            None
        );
    }

    #[test]
    fn load_file_command_does_not_append_mode_argument() {
        assert_eq!(
            load_file_command("/tmp/example.mkv"),
            "loadfile \"/tmp/example.mkv\""
        );
    }

    #[test]
    fn status_poll_matches_original_pause_then_position_sequence() {
        assert_eq!(
            MPLAYER_STATUS_QUERY_COMMANDS,
            ["get_property pause", "get_property time_pos"]
        );
    }

    #[test]
    fn load_file_command_quotes_special_characters() {
        assert_eq!(
            load_file_command("/tmp/a \"quoted\" file.mkv"),
            "loadfile \"/tmp/a \\\"quoted\\\" file.mkv\""
        );
    }

    #[test]
    fn load_file_command_removes_line_breaks_and_escapes_slave_syntax() {
        assert_eq!(
            load_file_command("/tmp/a\\b'c\nquit\r.mkv"),
            "loadfile \"/tmp/a\\\\b\\'cquit.mkv\""
        );
    }

    #[test]
    fn osd_command_matches_original_duration_level_and_sanitization() {
        assert_eq!(
            osd_command("line 1\n\"line 2\" \\n {value} 50%", Some(1250)),
            "osd_show_text \"line 1\\\"line 2\\\" \\n \\\\{value\\\\} 50%%\" 1250 1"
        );
    }
}
