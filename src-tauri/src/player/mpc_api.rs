use super::backend::{PlayerBackend, PlayerKind};
use super::properties::PlayerState;
use async_trait::async_trait;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};
use tokio::sync::oneshot;
use tokio::time::timeout;
use tracing::{debug, info, warn};

const MPC_OPEN_MAX_WAIT_TIME: Duration = Duration::from_secs(10);
const MPC_LOCK_WAIT_TIME: Duration = Duration::from_millis(200);
const MPC_RETRY_WAIT_TIME: Duration = Duration::from_millis(10);
const MPC_MAX_RETRIES: usize = 30;
const MPC_PAUSE_TOGGLE_DELAY: Duration = Duration::from_millis(50);
const MPC_OSD_POSITION: i32 = 1;
const MPC_MIN_VER: &str = "1.6.4";
const MPC_BE_MIN_VER: &str = "1.5.2.3123";

const CMD_CONNECT: u32 = 0x50000000;
const CMD_STATE: u32 = 0x50000001;
const CMD_PLAYMODE: u32 = 0x50000002;
const CMD_NOWPLAYING: u32 = 0x50000003;
const CMD_CURRENTPOSITION: u32 = 0x50000007;
const CMD_NOTIFYSEEK: u32 = 0x50000008;
const CMD_VERSION: u32 = 0x5000000A;
const CMD_DISCONNECT: u32 = 0x5000000B;
const CMD_OPENFILE: u32 = 0xA0000000;
const CMD_PLAYPAUSE: u32 = 0xA0000003;
const CMD_PLAY: u32 = 0xA0000004;
const CMD_PAUSE: u32 = 0xA0000005;
const CMD_SETPOSITION: u32 = 0xA0002000;
const CMD_GETCURRENTPOSITION: u32 = 0xA0003004;
const CMD_GETVERSION: u32 = 0xA0003006;
const CMD_SETSPEED: u32 = 0xA0004008;
const CMD_OSDSHOWMESSAGE: u32 = 0xA0005000;
const CMD_CLOSEAPP: u32 = 0xA0004006;

#[cfg(windows)]
mod win {
    use super::*;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::null;
    use std::sync::mpsc::{self, Receiver, Sender};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowLongPtrW,
        RegisterClassW, SendMessageW, SetWindowLongPtrW, CW_USEDEFAULT, GWLP_USERDATA, MSG,
        WM_COPYDATA, WNDCLASSW,
    };

    #[derive(Debug)]
    pub enum MpcEvent {
        Connected(isize),
        LoadState(i32),
        PlayState(i32),
        NowPlaying(String),
        Position(f64),
        Seek(f64),
        Version(String),
        Disconnected,
    }

    pub struct MpcListener {
        hwnd_raw: isize,
        mpc_handle: Arc<AtomicIsize>,
        event_tx: Sender<MpcEvent>,
    }

    impl MpcListener {
        pub fn spawn() -> anyhow::Result<(Self, Receiver<MpcEvent>)> {
            let (tx, rx) = mpsc::channel();
            let mpc_handle = Arc::new(AtomicIsize::new(0));
            let mpc_handle_clone = mpc_handle.clone();
            let (hwnd_tx, hwnd_rx) = mpsc::channel::<anyhow::Result<isize>>();

            let event_tx = tx.clone();
            std::thread::spawn(move || unsafe {
                let result = (|| -> anyhow::Result<isize> {
                    let class_name = widestr("MPCApiListener");
                    let title = widestr("MPC Listener");
                    let hinstance = GetModuleHandleW(PCWSTR(null()))?;
                    let wc = WNDCLASSW {
                        lpfnWndProc: Some(wndproc),
                        hInstance: hinstance.into(),
                        lpszClassName: PCWSTR(class_name.as_ptr()),
                        ..Default::default()
                    };
                    RegisterClassW(&wc);
                    let hwnd = CreateWindowExW(
                        Default::default(),
                        PCWSTR(class_name.as_ptr()),
                        PCWSTR(title.as_ptr()),
                        Default::default(),
                        0,
                        0,
                        0,
                        0,
                        None,
                        None,
                        Some(hinstance.into()),
                        None,
                    );
                    let hwnd = hwnd?;
                    let boxed = Box::new(MpcListenerState {
                        tx: event_tx.clone(),
                        mpc_handle: mpc_handle_clone,
                    });
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(boxed) as isize);
                    Ok(hwnd.0 as isize)
                })();

                match result {
                    Ok(hwnd_raw) => {
                        let _ = hwnd_tx.send(Ok(hwnd_raw));
                        let mut msg = MSG::default();
                        while GetMessageW(&mut msg, None, 0, 0).into() {
                            DispatchMessageW(&msg);
                        }
                    }
                    Err(err) => {
                        let _ = hwnd_tx.send(Err(err));
                    }
                }
            });

            let hwnd_raw = hwnd_rx
                .recv()
                .map_err(|_| anyhow::anyhow!("Failed to create MPC listener window"))??;
            Ok((
                Self {
                    hwnd_raw,
                    mpc_handle,
                    event_tx: tx,
                },
                rx,
            ))
        }

        pub fn hwnd(&self) -> HWND {
            HWND(self.hwnd_raw as *mut std::ffi::c_void)
        }

        pub fn hwnd_raw(&self) -> isize {
            self.hwnd_raw
        }

        pub fn set_mpc_handle(&self, hwnd: HWND) {
            self.mpc_handle.store(hwnd.0 as isize, Ordering::SeqCst);
        }

        pub fn clear_mpc_handle(&self) {
            self.mpc_handle.store(0, Ordering::SeqCst);
        }

        pub fn mpc_handle_raw(&self) -> Option<isize> {
            let raw = self.mpc_handle.load(Ordering::SeqCst);
            if raw == 0 {
                None
            } else {
                Some(raw)
            }
        }

        pub fn mpc_handle(&self) -> Option<HWND> {
            self.mpc_handle_raw()
                .map(|raw| HWND(raw as *mut std::ffi::c_void))
        }

        pub fn send_command(
            &self,
            cmd: u32,
            payload: Option<CommandPayload>,
        ) -> anyhow::Result<()> {
            let mpc_handle = self
                .mpc_handle()
                .ok_or_else(|| anyhow::anyhow!("MPC handle not available"))?;
            let (ptr, len, _payload_guard) = build_payload(payload);
            let cds = COPYDATASTRUCT {
                dwData: cmd as usize,
                cbData: len as u32,
                lpData: ptr as *mut std::ffi::c_void,
            };
            unsafe {
                SendMessageW(
                    mpc_handle,
                    WM_COPYDATA,
                    Some(WPARAM(self.hwnd_raw as usize)),
                    Some(LPARAM(&cds as *const _ as isize)),
                );
            }
            Ok(())
        }
    }

    struct MpcListenerState {
        tx: Sender<MpcEvent>,
        mpc_handle: Arc<AtomicIsize>,
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_COPYDATA {
            let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut MpcListenerState;
            if state_ptr.is_null() {
                return LRESULT(0);
            }
            let state = &*state_ptr;
            let cds = &*(lparam.0 as *const COPYDATASTRUCT);
            let cmd = cds.dwData as u32;
            let value = wide_ptr_to_string(cds.lpData as *const u16, cds.cbData as usize);
            match cmd {
                CMD_CONNECT => {
                    if let Ok(handle) = value.trim().parse::<isize>() {
                        state.mpc_handle.store(handle, Ordering::SeqCst);
                        let _ = state.tx.send(MpcEvent::Connected(handle));
                    }
                }
                CMD_STATE => {
                    if let Ok(state_val) = value.trim().parse::<i32>() {
                        let _ = state.tx.send(MpcEvent::LoadState(state_val));
                    }
                }
                CMD_PLAYMODE => {
                    if let Ok(play_state) = value.trim().parse::<i32>() {
                        let _ = state.tx.send(MpcEvent::PlayState(play_state));
                    }
                }
                CMD_NOWPLAYING => {
                    let _ = state.tx.send(MpcEvent::NowPlaying(value));
                }
                CMD_CURRENTPOSITION => {
                    if let Ok(pos) = value.trim().parse::<f64>() {
                        let _ = state.tx.send(MpcEvent::Position(pos));
                    }
                }
                CMD_NOTIFYSEEK => {
                    if let Ok(pos) = value.trim().parse::<f64>() {
                        let _ = state.tx.send(MpcEvent::Seek(pos));
                    }
                }
                CMD_VERSION => {
                    let _ = state.tx.send(MpcEvent::Version(value));
                }
                CMD_DISCONNECT => {
                    let _ = state.tx.send(MpcEvent::Disconnected);
                }
                _ => {}
            }
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    fn widestr(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn wide_ptr_to_string(ptr: *const u16, bytes: usize) -> String {
        if ptr.is_null() || bytes < 2 {
            return String::new();
        }
        let len = bytes / 2;
        let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
        let mut end = 0;
        while end < slice.len() {
            if slice[end] == 0 {
                break;
            }
            end += 1;
        }
        String::from_utf16_lossy(&slice[..end])
    }

    #[derive(Clone)]
    pub enum CommandPayload {
        Text(String),
        Osd {
            message: String,
            duration_ms: i32,
            position: i32,
        },
        Raw(Vec<u8>),
    }

    struct PayloadGuard {
        _wide: Option<Vec<u16>>,
        _raw: Option<Vec<u8>>,
    }

    fn build_payload(
        payload: Option<CommandPayload>,
    ) -> (*const std::ffi::c_void, usize, PayloadGuard) {
        match payload {
            Some(CommandPayload::Text(value)) => {
                let wide = widestr(&value);
                let ptr = wide.as_ptr() as *const std::ffi::c_void;
                let len = wide.len() * 2;
                (
                    ptr,
                    len,
                    PayloadGuard {
                        _wide: Some(wide),
                        _raw: None,
                    },
                )
            }
            Some(CommandPayload::Osd {
                message,
                duration_ms,
                position,
            }) => {
                let wide = widestr(&message);
                let mut raw = Vec::with_capacity(8 + wide.len() * 2);
                raw.extend_from_slice(&position.to_le_bytes());
                raw.extend_from_slice(&duration_ms.to_le_bytes());
                for unit in wide {
                    raw.extend_from_slice(&unit.to_le_bytes());
                }
                let ptr = raw.as_ptr() as *const std::ffi::c_void;
                let len = raw.len();
                (
                    ptr,
                    len,
                    PayloadGuard {
                        _wide: None,
                        _raw: Some(raw),
                    },
                )
            }
            Some(CommandPayload::Raw(raw)) => {
                let ptr = raw.as_ptr() as *const std::ffi::c_void;
                let len = raw.len();
                (
                    ptr,
                    len,
                    PayloadGuard {
                        _wide: None,
                        _raw: Some(raw),
                    },
                )
            }
            None => (
                null(),
                0,
                PayloadGuard {
                    _wide: None,
                    _raw: None,
                },
            ),
        }
    }

    pub fn start_listener() -> anyhow::Result<(MpcListener, Receiver<MpcEvent>)> {
        MpcListener::spawn()
    }
}

#[cfg(not(windows))]
mod win {
    use super::*;
    use std::sync::mpsc::{self, Receiver};

    #[derive(Debug)]
    pub enum MpcEvent {}

    pub struct MpcListener;

    impl MpcListener {
        pub fn hwnd(&self) {}

        pub fn set_mpc_handle(&self, _hwnd: ()) {}

        pub fn mpc_handle(&self) -> Option<()> {
            None
        }

        pub fn send_command(
            &self,
            _cmd: u32,
            _payload: Option<CommandPayload>,
        ) -> anyhow::Result<()> {
            anyhow::bail!("MPC backend is only supported on Windows")
        }
    }

    #[derive(Clone)]
    pub enum CommandPayload {
        Text(String),
        Osd {
            message: String,
            duration_ms: i32,
            position: i32,
        },
        Raw(Vec<u8>),
    }

    pub fn start_listener() -> anyhow::Result<(MpcListener, Receiver<MpcEvent>)> {
        let (_tx, rx) = mpsc::channel();
        Ok((MpcListener, rx))
    }
}

use win::{start_listener, CommandPayload, MpcEvent, MpcListener};

#[cfg(windows)]
pub struct MpcApiBackend {
    kind: PlayerKind,
    state: Arc<Mutex<PlayerState>>,
    listener: MpcListener,
    connected: Arc<AtomicBool>,
    switch_pause_calls: Arc<AtomicBool>,
    version: Arc<Mutex<Option<String>>>,
    position_waiter: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    version_waiter: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

#[cfg(windows)]
impl MpcApiBackend {
    pub async fn start(
        kind: PlayerKind,
        player_path: &str,
        args: &[String],
        initial_file: Option<&str>,
    ) -> anyhow::Result<(Self, Option<Child>)> {
        info!(
            "Starting MPC: kind={:?}, path={}, args={:?}, initial_file={:?}",
            kind, player_path, args, initial_file
        );
        let (listener, event_rx) = start_listener()?;

        let mut cmd = Command::new(player_path);
        let mut full_args = Vec::new();
        full_args.extend(args.iter().cloned());
        full_args.push("/slave".to_string());
        full_args.push(listener.hwnd_raw().to_string());
        cmd.args(&full_args);
        if let Some(path) = initial_file {
            cmd.arg(path);
        }
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let child = cmd.spawn().ok();

        let state = Arc::new(Mutex::new(PlayerState::default()));
        let connected = Arc::new(AtomicBool::new(false));
        let switch_pause_calls = Arc::new(AtomicBool::new(false));
        let version = Arc::new(Mutex::new(None));
        let position_waiter = Arc::new(Mutex::new(None));
        let version_waiter = Arc::new(Mutex::new(None));

        spawn_event_loop(EventLoopArgs {
            event_rx,
            listener_hwnd: listener.hwnd_raw(),
            state: state.clone(),
            connected: connected.clone(),
            switch_pause_calls: switch_pause_calls.clone(),
            version: version.clone(),
            position_waiter: position_waiter.clone(),
            version_waiter: version_waiter.clone(),
        });

        let backend = Self {
            kind,
            state,
            listener,
            connected,
            switch_pause_calls,
            version,
            position_waiter,
            version_waiter,
        };

        backend.wait_for_connect().await?;
        backend.check_version().await?;

        Ok((backend, child))
    }

    async fn wait_for_connect(&self) -> anyhow::Result<()> {
        let start = Instant::now();
        while start.elapsed() < MPC_OPEN_MAX_WAIT_TIME {
            if self.listener.mpc_handle().is_some() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        anyhow::bail!("Failed to connect to MPC slave API");
    }

    async fn check_version(&self) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        *self.version_waiter.lock() = Some(tx);
        let _ = self.listener.send_command(CMD_GETVERSION, None);
        let _ = timeout(Duration::from_millis(200), rx).await;
        let version = self.version.lock().clone().unwrap_or_default();
        if version.is_empty() {
            anyhow::bail!(min_version_message(self.kind));
        }
        if !meets_min_version(&version, self.min_version()) {
            anyhow::bail!(min_version_message(self.kind));
        }
        if is_switch_pause_version(&version) {
            self.switch_pause_calls.store(true, Ordering::SeqCst);
        }
        Ok(())
    }

    fn min_version(&self) -> &'static str {
        match self.kind {
            PlayerKind::MpcBe => MPC_BE_MIN_VER,
            _ => MPC_MIN_VER,
        }
    }

    async fn send_position_request(&self) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        *self.position_waiter.lock() = Some(tx);
        self.listener.send_command(CMD_GETCURRENTPOSITION, None)?;
        let _ = timeout(MPC_LOCK_WAIT_TIME, rx).await;
        Ok(())
    }

    fn file_ready(&self) -> bool {
        self.connected()
    }

    fn connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    fn mark_disconnected(&self) {
        self.connected.store(false, Ordering::SeqCst);
        self.listener.clear_mpc_handle();
    }

    fn send_osd(&self, message: &str, duration_ms: i32) -> anyhow::Result<()> {
        self.listener.send_command(
            CMD_OSDSHOWMESSAGE,
            Some(CommandPayload::Osd {
                message: message.to_string(),
                duration_ms,
                position: MPC_OSD_POSITION,
            }),
        )
    }

    fn send_command_retry(&self, cmd: u32, payload: Option<CommandPayload>) -> anyhow::Result<()> {
        for _ in 0..MPC_MAX_RETRIES {
            if self.file_ready() {
                match self.listener.send_command(cmd, payload.clone()) {
                    Ok(()) => return Ok(()),
                    Err(e) => {
                        self.mark_disconnected();
                        return Err(e);
                    }
                }
            }
            std::thread::sleep(MPC_RETRY_WAIT_TIME);
        }
        anyhow::bail!("MPC command failed after retries")
    }
}

#[cfg(windows)]
#[async_trait]
impl PlayerBackend for MpcApiBackend {
    fn kind(&self) -> PlayerKind {
        self.kind
    }

    fn name(&self) -> &'static str {
        self.kind.display_name()
    }

    fn get_state(&self) -> PlayerState {
        self.state.lock().clone()
    }

    async fn poll_state(&self) -> anyhow::Result<()> {
        if !self.file_ready() {
            return Ok(());
        }
        let _ = self.send_position_request().await;
        Ok(())
    }

    async fn set_position(&self, position: f64) -> anyhow::Result<()> {
        if !self.file_ready() {
            return Err(anyhow::anyhow!("MPC file not ready"));
        }
        self.send_command_retry(
            CMD_SETPOSITION,
            Some(CommandPayload::Text(position.to_string())),
        )?;
        Ok(())
    }

    async fn set_paused(&self, paused: bool) -> anyhow::Result<()> {
        if !self.file_ready() {
            return Err(anyhow::anyhow!("MPC file not ready"));
        }
        let mut value = paused;
        if self.switch_pause_calls.load(Ordering::SeqCst) {
            value = !value;
        }
        let cmd = if value { CMD_PAUSE } else { CMD_PLAY };
        self.send_command_retry(cmd, None)?;
        tokio::time::sleep(MPC_PAUSE_TOGGLE_DELAY).await;
        if let Some(current) = self.state.lock().paused {
            if current != paused {
                if let Err(e) = self.listener.send_command(CMD_PLAYPAUSE, None) {
                    warn!("Failed to toggle pause: {}", e);
                }
            }
        }
        Ok(())
    }

    async fn set_speed(&self, speed: f64) -> anyhow::Result<()> {
        if !self.file_ready() {
            return Err(anyhow::anyhow!("MPC file not ready"));
        }
        self.send_command_retry(CMD_SETSPEED, Some(CommandPayload::Text(speed.to_string())))?;
        Ok(())
    }

    async fn load_file(&self, path: &str) -> anyhow::Result<()> {
        self.listener
            .send_command(CMD_OPENFILE, Some(CommandPayload::Text(path.to_string())))
            .map_err(|e| {
                self.mark_disconnected();
                e
            })?;
        Ok(())
    }

    fn show_osd(&self, text: &str, duration_ms: Option<u64>) -> anyhow::Result<()> {
        let duration = duration_ms.unwrap_or(3000) as i32;
        self.send_osd(text, duration).map_err(|e| {
            self.mark_disconnected();
            e
        })
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        let _ = self.listener.send_command(CMD_CLOSEAPP, None);
        self.mark_disconnected();
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected()
    }
}

#[cfg(windows)]
struct EventLoopArgs {
    event_rx: std::sync::mpsc::Receiver<MpcEvent>,
    listener_hwnd: isize,
    state: Arc<Mutex<PlayerState>>,
    connected: Arc<AtomicBool>,
    switch_pause_calls: Arc<AtomicBool>,
    version: Arc<Mutex<Option<String>>>,
    position_waiter: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    version_waiter: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

#[cfg(windows)]
fn spawn_event_loop(args: EventLoopArgs) {
    let EventLoopArgs {
        event_rx,
        listener_hwnd,
        state,
        connected,
        switch_pause_calls,
        version,
        position_waiter,
        version_waiter,
    } = args;
    std::thread::spawn(move || {
        for event in event_rx {
            match event {
                MpcEvent::Connected(hwnd) => {
                    connected.store(true, Ordering::SeqCst);
                    debug!("MPC connected: {} (listener {})", hwnd, listener_hwnd);
                }
                MpcEvent::LoadState(state_code) => {
                    let ready = !matches!(state_code, 0 | 1 | 3);
                    connected.store(ready, Ordering::SeqCst);
                    if !ready {
                        let mut guard = state.lock();
                        guard.paused = None;
                    }
                }
                MpcEvent::PlayState(play_state) => {
                    let paused = play_state != 0;
                    state.lock().paused = Some(paused);
                }
                MpcEvent::NowPlaying(value) => {
                    let parts = split_mpc_fields(&value);
                    if parts.len() >= 5 {
                        let path = parts[3].clone();
                        let filename = mpc_filename_from_path(&path);
                        let duration = parts[4].parse::<f64>().ok();
                        let mut guard = state.lock();
                        guard.path = Some(path);
                        guard.filename = filename;
                        guard.duration = duration;
                    }
                }
                MpcEvent::Position(pos) => {
                    state.lock().position = Some(pos);
                    if let Some(tx) = position_waiter.lock().take() {
                        let _ = tx.send(());
                    }
                }
                MpcEvent::Seek(pos) => {
                    state.lock().position = Some(pos);
                }
                MpcEvent::Version(value) => {
                    *version.lock() = Some(value.clone());
                    if let Some(tx) = version_waiter.lock().take() {
                        let _ = tx.send(());
                    }
                    if is_switch_pause_version(&value) {
                        switch_pause_calls.store(true, Ordering::SeqCst);
                    }
                }
                MpcEvent::Disconnected => {
                    warn!("MPC disconnected");
                    connected.store(false, Ordering::SeqCst);
                }
            }
        }
    });
}

fn split_mpc_fields(input: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut previous_is_backslash = false;

    for ch in input.chars() {
        if ch == '|' && !previous_is_backslash {
            parts.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
        previous_is_backslash = ch == '\\';
    }

    if !parts.is_empty() || !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn mpc_filename_from_path(path: &str) -> Option<String> {
    path.trim_end_matches(['\\', '/'])
        .rsplit(['\\', '/'])
        .find(|segment| !segment.is_empty())
        .map(|segment| segment.to_string())
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

fn is_switch_pause_version(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.first() == Some(&"1") && parts.get(1) == Some(&"6") && parts.get(2) == Some(&"4")
}

fn min_version_message(kind: PlayerKind) -> String {
    match kind {
        PlayerKind::MpcBe => format!(
            "MPC version not sufficient, please use `mpc-be` >= `{}`",
            MPC_BE_MIN_VER
        ),
        _ => format!(
            "MPC version not sufficient, please use `mpc-hc` >= `{}`",
            MPC_MIN_VER
        ),
    }
}

#[cfg(not(windows))]
pub struct MpcApiBackend;

#[cfg(not(windows))]
impl MpcApiBackend {
    pub async fn start(
        _kind: PlayerKind,
        _player_path: &str,
        _args: &[String],
        _initial_file: Option<&str>,
    ) -> anyhow::Result<(Self, Option<Child>)> {
        anyhow::bail!("MPC backend is only supported on Windows")
    }
}

#[cfg(not(windows))]
#[async_trait]
impl PlayerBackend for MpcApiBackend {
    fn kind(&self) -> PlayerKind {
        PlayerKind::Unknown
    }

    fn name(&self) -> &'static str {
        "MPC"
    }

    fn get_state(&self) -> PlayerState {
        PlayerState::default()
    }

    async fn poll_state(&self) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("MPC backend is only supported on Windows"))
    }

    async fn set_position(&self, _position: f64) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("MPC backend is only supported on Windows"))
    }

    async fn set_paused(&self, _paused: bool) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("MPC backend is only supported on Windows"))
    }

    async fn set_speed(&self, _speed: f64) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("MPC backend is only supported on Windows"))
    }

    async fn load_file(&self, _path: &str) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("MPC backend is only supported on Windows"))
    }

    fn show_osd(&self, _text: &str, _duration_ms: Option<u64>) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("MPC backend is only supported on Windows"))
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::{mpc_filename_from_path, split_mpc_fields};

    #[test]
    fn split_mpc_fields_keeps_windows_path_separators() {
        let input = "0|1|Movie|C:\\Videos\\Example.mkv|123.4";
        let parts = split_mpc_fields(input);

        assert_eq!(
            parts.get(3).map(std::string::String::as_str),
            Some(r"C:\Videos\Example.mkv")
        );
    }

    #[test]
    fn split_mpc_fields_keeps_escaped_pipe_sequence() {
        let input = r"0|1|Name\|Part|\\server\share\Clip.mkv|321";
        let parts = split_mpc_fields(input);

        assert_eq!(
            parts.get(2).map(std::string::String::as_str),
            Some(r"Name\|Part")
        );
        assert_eq!(
            parts.get(3).map(std::string::String::as_str),
            Some(r"\\server\share\Clip.mkv")
        );
    }

    #[test]
    fn split_mpc_fields_keeps_trailing_empty_field() {
        let parts = split_mpc_fields("0|1|2|3|");

        assert_eq!(
            parts,
            vec![
                "0".to_string(),
                "1".to_string(),
                "2".to_string(),
                "3".to_string(),
                "".to_string()
            ]
        );
    }

    #[test]
    fn mpc_filename_from_path_extracts_windows_basename() {
        assert_eq!(
            mpc_filename_from_path(r"C:\Videos\Example.mkv").as_deref(),
            Some("Example.mkv")
        );
    }

    #[test]
    fn mpc_filename_from_path_handles_doubled_separators() {
        assert_eq!(
            mpc_filename_from_path(r"C:\\Videos\\Example.mkv").as_deref(),
            Some("Example.mkv")
        );
        assert_eq!(
            mpc_filename_from_path(r"\\server\share\Clip.mkv").as_deref(),
            Some("Clip.mkv")
        );
    }
}
