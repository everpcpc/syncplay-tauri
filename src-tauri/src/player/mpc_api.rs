use super::backend::{PlayerBackend, PlayerKind};
use super::properties::PlayerState;
use async_trait::async_trait;
#[cfg(any(windows, test))]
use parking_lot::Condvar;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};
use tokio::sync::oneshot;
use tokio::time::timeout;
use tracing::{debug, info, warn};

const MPC_OPEN_MAX_WAIT_TIME: Duration = Duration::from_secs(10);
const MPC_EXISTENCE_CHECK_INTERVAL: Duration = Duration::from_secs(10);
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

#[cfg(any(windows, test))]
#[derive(Clone, Default)]
struct MpcShutdownSignal {
    inner: Arc<MpcShutdownSignalInner>,
}

#[cfg(any(windows, test))]
#[derive(Default)]
struct MpcShutdownSignalInner {
    requested: Mutex<bool>,
    wake: Condvar,
}

#[cfg(any(windows, test))]
impl MpcShutdownSignal {
    fn request(&self) -> bool {
        let mut requested = self.inner.requested.lock();
        let first_request = !*requested;
        *requested = true;
        drop(requested);
        self.inner.wake.notify_all();
        first_request
    }

    fn wait_timeout(&self, duration: Duration) -> bool {
        let deadline = Instant::now() + duration;
        let mut requested = self.inner.requested.lock();
        while !*requested {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            self.inner.wake.wait_for(&mut requested, remaining);
        }
        *requested
    }
}

#[cfg(windows)]
fn join_owned_thread(owner: &Mutex<Option<std::thread::JoinHandle<()>>>, thread_name: &str) {
    let Some(handle) = owner.lock().take() else {
        return;
    };
    if handle.join().is_err() {
        warn!(thread_name, "MPC lifecycle thread panicked during teardown");
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum MpcMediaSettleAction {
    SetPaused { paused: bool, delay: Duration },
    TogglePaused { delay: Duration },
    SetPosition(f64),
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MpcMediaSettleStage {
    ForcePause { remaining: usize },
    ApplyTarget,
    VerifyTarget,
    RefreshPause { remaining: usize },
    VerifyRefresh,
    SetPosition,
    Complete,
}

#[derive(Debug, Clone)]
pub(crate) struct MpcMediaSettle {
    target_paused: bool,
    target_position: f64,
    attempts_remaining: usize,
    stage: MpcMediaSettleStage,
}

impl MpcMediaSettle {
    pub(crate) fn new(target_paused: bool, target_position: f64) -> Self {
        Self {
            target_paused,
            target_position,
            attempts_remaining: MPC_MAX_RETRIES,
            stage: MpcMediaSettleStage::ForcePause {
                remaining: MPC_MAX_RETRIES,
            },
        }
    }

    pub(crate) fn needs_pause_observation(&self) -> bool {
        matches!(
            self.stage,
            MpcMediaSettleStage::VerifyTarget | MpcMediaSettleStage::VerifyRefresh
        )
    }

    pub(crate) fn next(&mut self, observed_paused: Option<bool>) -> Option<MpcMediaSettleAction> {
        match self.stage {
            MpcMediaSettleStage::ForcePause { remaining } => {
                self.stage = if remaining > 1 {
                    MpcMediaSettleStage::ForcePause {
                        remaining: remaining - 1,
                    }
                } else {
                    MpcMediaSettleStage::ApplyTarget
                };
                Some(MpcMediaSettleAction::SetPaused {
                    paused: true,
                    delay: MPC_RETRY_WAIT_TIME,
                })
            }
            MpcMediaSettleStage::ApplyTarget => {
                self.stage = MpcMediaSettleStage::VerifyTarget;
                Some(MpcMediaSettleAction::SetPaused {
                    paused: self.target_paused,
                    delay: Duration::ZERO,
                })
            }
            MpcMediaSettleStage::VerifyTarget => {
                if mpc_is_paused(observed_paused) == self.target_paused {
                    self.stage = MpcMediaSettleStage::SetPosition;
                } else {
                    self.stage = MpcMediaSettleStage::RefreshPause { remaining: 2 };
                }
                self.next(observed_paused)
            }
            MpcMediaSettleStage::RefreshPause { remaining } => {
                self.stage = if remaining > 1 {
                    MpcMediaSettleStage::RefreshPause {
                        remaining: remaining - 1,
                    }
                } else {
                    MpcMediaSettleStage::VerifyRefresh
                };
                Some(MpcMediaSettleAction::TogglePaused {
                    delay: MPC_PAUSE_TOGGLE_DELAY,
                })
            }
            MpcMediaSettleStage::VerifyRefresh => {
                if mpc_is_paused(observed_paused) == self.target_paused {
                    self.stage = MpcMediaSettleStage::SetPosition;
                    return self.next(observed_paused);
                }
                if self.attempts_remaining > 1 {
                    self.attempts_remaining -= 1;
                    self.stage = MpcMediaSettleStage::ForcePause {
                        remaining: MPC_MAX_RETRIES,
                    };
                    return self.next(observed_paused);
                }
                self.stage = MpcMediaSettleStage::Complete;
                Some(MpcMediaSettleAction::Failed)
            }
            MpcMediaSettleStage::SetPosition => {
                self.stage = MpcMediaSettleStage::Complete;
                Some(MpcMediaSettleAction::SetPosition(self.target_position))
            }
            MpcMediaSettleStage::Complete => None,
        }
    }
}

fn mpc_is_paused(observed_paused: Option<bool>) -> bool {
    observed_paused.unwrap_or(false)
}

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
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        GetWindowLongPtrW, IsWindow, PostMessageW, PostQuitMessage, PostThreadMessageW,
        RegisterClassW, SendMessageW, SetWindowLongPtrW, CREATESTRUCTW, GWLP_USERDATA, MSG,
        WM_CLOSE, WM_COPYDATA, WM_DESTROY, WM_NCCREATE, WM_NCDESTROY, WM_QUIT, WNDCLASSW,
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
        Shutdown,
    }

    pub struct MpcListener {
        hwnd_raw: isize,
        listener_thread_id: u32,
        mpc_handle: Arc<AtomicIsize>,
        event_tx: Sender<MpcEvent>,
        shutdown: MpcShutdownSignal,
        teardown_lock: Mutex<()>,
        window_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
        liveness_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    }

    impl MpcListener {
        pub fn spawn() -> anyhow::Result<(Self, Receiver<MpcEvent>)> {
            let (tx, rx) = mpsc::channel();
            let mpc_handle = Arc::new(AtomicIsize::new(0));
            let mpc_handle_clone = mpc_handle.clone();
            let (hwnd_tx, hwnd_rx) = mpsc::channel::<anyhow::Result<(isize, u32)>>();

            let event_tx = tx.clone();
            let window_thread = std::thread::spawn(move || unsafe {
                let result = (|| -> anyhow::Result<(HWND, u32)> {
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
                    let mut create_params = MpcCreateParams {
                        state: Some(Box::new(MpcListenerState::new(event_tx, mpc_handle_clone))),
                    };
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
                        Some(&mut create_params as *mut MpcCreateParams as *const _),
                    )?;
                    Ok((hwnd, GetCurrentThreadId()))
                })();

                match result {
                    Ok((hwnd, thread_id)) => {
                        let _ = hwnd_tx.send(Ok((hwnd.0 as isize, thread_id)));
                        let mut msg = MSG::default();
                        loop {
                            let status = GetMessageW(&mut msg, None, 0, 0);
                            if status.0 <= 0 {
                                break;
                            }
                            DispatchMessageW(&msg);
                        }
                        if IsWindow(Some(hwnd)).as_bool() {
                            let _ = DestroyWindow(hwnd);
                        }
                    }
                    Err(err) => {
                        let _ = hwnd_tx.send(Err(err));
                    }
                }
            });

            let (hwnd_raw, listener_thread_id) = match hwnd_rx.recv() {
                Ok(Ok(window)) => window,
                Ok(Err(error)) => {
                    let _ = window_thread.join();
                    return Err(error);
                }
                Err(_) => {
                    let _ = window_thread.join();
                    anyhow::bail!("Failed to create MPC listener window");
                }
            };
            Ok((
                Self {
                    hwnd_raw,
                    listener_thread_id,
                    mpc_handle,
                    event_tx: tx,
                    shutdown: MpcShutdownSignal::default(),
                    teardown_lock: Mutex::new(()),
                    window_thread: Mutex::new(Some(window_thread)),
                    liveness_thread: Mutex::new(None),
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

        pub fn spawn_liveness_probe(&self) {
            let mut owner = self.liveness_thread.lock();
            if owner.is_some() {
                return;
            }
            let mpc_handle = self.mpc_handle.clone();
            let event_tx = self.event_tx.clone();
            let shutdown = self.shutdown.clone();
            *owner = Some(std::thread::spawn(move || loop {
                if shutdown.wait_timeout(MPC_EXISTENCE_CHECK_INTERVAL) {
                    break;
                }
                let raw = mpc_handle.load(Ordering::SeqCst);
                if raw == 0 {
                    break;
                }
                let hwnd = HWND(raw as *mut std::ffi::c_void);
                if unsafe { IsWindow(Some(hwnd)).as_bool() } {
                    continue;
                }
                if mpc_handle.swap(0, Ordering::SeqCst) != 0 {
                    let _ = event_tx.send(MpcEvent::Disconnected);
                }
                break;
            }));
        }

        pub fn shutdown(&self) {
            let _teardown = self.teardown_lock.lock();
            let first_request = self.shutdown.request();
            self.mpc_handle.store(0, Ordering::SeqCst);
            if first_request {
                let _ = self.event_tx.send(MpcEvent::Shutdown);
                let close_result =
                    unsafe { PostMessageW(Some(self.hwnd()), WM_CLOSE, WPARAM(0), LPARAM(0)) };
                if close_result.is_err() {
                    let _ = unsafe {
                        PostThreadMessageW(self.listener_thread_id, WM_QUIT, WPARAM(0), LPARAM(0))
                    };
                }
            }
            join_owned_thread(&self.window_thread, "MPC listener window");
            join_owned_thread(&self.liveness_thread, "MPC liveness probe");
        }

        #[cfg(test)]
        pub fn owned_thread_count(&self) -> usize {
            usize::from(self.window_thread.lock().is_some())
                + usize::from(self.liveness_thread.lock().is_some())
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
            if !unsafe { IsWindow(Some(mpc_handle)).as_bool() } {
                if self.mpc_handle.swap(0, Ordering::SeqCst) != 0 {
                    let _ = self.event_tx.send(MpcEvent::Disconnected);
                }
                anyhow::bail!("MPC window is no longer available");
            }
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

    impl Drop for MpcListener {
        fn drop(&mut self) {
            self.shutdown();
        }
    }

    struct MpcCreateParams {
        state: Option<Box<MpcListenerState>>,
    }

    struct MpcListenerState {
        tx: Sender<MpcEvent>,
        mpc_handle: Arc<AtomicIsize>,
    }

    #[cfg(test)]
    static LIVE_LISTENER_STATES: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    impl MpcListenerState {
        fn new(tx: Sender<MpcEvent>, mpc_handle: Arc<AtomicIsize>) -> Self {
            #[cfg(test)]
            LIVE_LISTENER_STATES.fetch_add(1, Ordering::SeqCst);
            Self { tx, mpc_handle }
        }
    }

    impl Drop for MpcListenerState {
        fn drop(&mut self) {
            #[cfg(test)]
            LIVE_LISTENER_STATES.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[cfg(test)]
    pub fn live_listener_state_count() -> usize {
        LIVE_LISTENER_STATES.load(Ordering::SeqCst)
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            let params = create.lpCreateParams as *mut MpcCreateParams;
            if params.is_null() {
                return LRESULT(0);
            }
            let Some(state) = (&mut *params).state.take() else {
                return LRESULT(0);
            };
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        if msg == WM_CLOSE {
            let _ = DestroyWindow(hwnd);
            return LRESULT(0);
        }
        if msg == WM_DESTROY {
            let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut MpcListenerState;
            if !state_ptr.is_null() {
                let state = &*state_ptr;
                state.mpc_handle.store(0, Ordering::SeqCst);
                let _ = state.tx.send(MpcEvent::Disconnected);
            }
            PostQuitMessage(0);
            return LRESULT(0);
        }
        if msg == WM_NCDESTROY {
            let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut MpcListenerState;
            if !state_ptr.is_null() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(state_ptr));
            }
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
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
                    state.mpc_handle.store(0, Ordering::SeqCst);
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
    file_ready: Arc<AtomicBool>,
    switch_pause_calls: Arc<AtomicBool>,
    version: Arc<Mutex<Option<String>>>,
    position_waiter: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    version_waiter: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    event_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    teardown_lock: Mutex<()>,
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
        cmd.kill_on_drop(true);
        let full_args = mpc_startup_arguments(args, listener.hwnd_raw());
        cmd.args(&full_args);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let child = Some(
            cmd.spawn()
                .map_err(|error| anyhow::anyhow!("Failed to start MPC: {error}"))?,
        );

        let state = Arc::new(Mutex::new(PlayerState::default()));
        let connected = Arc::new(AtomicBool::new(false));
        let file_ready = Arc::new(AtomicBool::new(false));
        let switch_pause_calls = Arc::new(AtomicBool::new(false));
        let version = Arc::new(Mutex::new(None));
        let position_waiter = Arc::new(Mutex::new(None));
        let version_waiter = Arc::new(Mutex::new(None));

        let event_thread = spawn_event_loop(EventLoopArgs {
            event_rx,
            listener_hwnd: listener.hwnd_raw(),
            state: state.clone(),
            connected: connected.clone(),
            file_ready: file_ready.clone(),
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
            file_ready,
            switch_pause_calls,
            version,
            position_waiter,
            version_waiter,
            event_thread: Mutex::new(Some(event_thread)),
            teardown_lock: Mutex::new(()),
        };

        backend.wait_for_connect().await?;
        backend.listener.spawn_liveness_probe();
        backend.check_version().await?;
        if let Some(path) = initial_file {
            backend.load_initial_file(path)?;
        }

        Ok((backend, child))
    }

    fn load_initial_file(&self, path: &str) -> anyhow::Result<()> {
        self.listener
            .send_command(CMD_OPENFILE, Some(CommandPayload::Text(path.to_string())))
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
        self.file_ready.load(Ordering::SeqCst)
    }

    fn connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    fn mark_disconnected(&self) {
        self.connected.store(false, Ordering::SeqCst);
        self.file_ready.store(false, Ordering::SeqCst);
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

    fn send_paused_command(&self, paused: bool) -> anyhow::Result<()> {
        let value = if self.switch_pause_calls.load(Ordering::SeqCst) {
            !paused
        } else {
            paused
        };
        self.send_command_retry(if value { CMD_PAUSE } else { CMD_PLAY }, None)
    }

    fn teardown(&self) {
        let _teardown = self.teardown_lock.lock();
        let _ = self.listener.send_command(CMD_CLOSEAPP, None);
        self.mark_disconnected();
        self.position_waiter.lock().take();
        self.version_waiter.lock().take();
        self.listener.shutdown();
        join_owned_thread(&self.event_thread, "MPC event receiver");
    }
}

#[cfg(windows)]
impl Drop for MpcApiBackend {
    fn drop(&mut self) {
        self.teardown();
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
        self.send_command_retry(
            CMD_SETPOSITION,
            Some(CommandPayload::Text(position.to_string())),
        )?;
        Ok(())
    }

    async fn set_paused(&self, paused: bool) -> anyhow::Result<()> {
        self.send_paused_command(paused)
    }

    async fn settle_media_change(&self, paused: bool, position: f64) -> anyhow::Result<()> {
        let mut settle = MpcMediaSettle::new(paused, position);
        loop {
            let observed_paused = self.state.lock().paused;
            let Some(action) = settle.next(observed_paused) else {
                return Ok(());
            };
            match action {
                MpcMediaSettleAction::SetPaused { paused, delay } => {
                    self.send_paused_command(paused)?;
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                }
                MpcMediaSettleAction::TogglePaused { delay } => {
                    self.send_command_retry(CMD_PLAYPAUSE, None)?;
                    tokio::time::sleep(delay).await;
                }
                MpcMediaSettleAction::SetPosition(position) => {
                    self.send_command_retry(
                        CMD_SETPOSITION,
                        Some(CommandPayload::Text(position.to_string())),
                    )?;
                }
                MpcMediaSettleAction::Failed => {
                    anyhow::bail!("MPC pause state did not settle after retries");
                }
            }
        }
    }

    async fn set_speed(&self, speed: f64) -> anyhow::Result<()> {
        self.send_command_retry(CMD_SETSPEED, Some(CommandPayload::Text(speed.to_string())))?;
        Ok(())
    }

    async fn load_file(&self, path: &str) -> anyhow::Result<()> {
        self.listener
            .send_command(CMD_OPENFILE, Some(CommandPayload::Text(path.to_string())))
            .inspect_err(|_e| {
                self.mark_disconnected();
            })?;
        Ok(())
    }

    fn show_osd(&self, text: &str, duration_ms: Option<u64>) -> anyhow::Result<()> {
        let duration = duration_ms.unwrap_or(3000) as i32;
        self.send_osd(text, duration).inspect_err(|_e| {
            self.mark_disconnected();
        })
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        self.teardown();
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
    file_ready: Arc<AtomicBool>,
    switch_pause_calls: Arc<AtomicBool>,
    version: Arc<Mutex<Option<String>>>,
    position_waiter: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    version_waiter: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

#[cfg(windows)]
fn spawn_event_loop(args: EventLoopArgs) -> std::thread::JoinHandle<()> {
    let EventLoopArgs {
        event_rx,
        listener_hwnd,
        state,
        connected,
        file_ready,
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
                    apply_mpc_load_state(&state, &file_ready, state_code);
                }
                MpcEvent::PlayState(play_state) => {
                    let paused = play_state != 0;
                    state.lock().observe_paused(Some(paused));
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
                    state.lock().observe_position(Some(pos));
                    if let Some(tx) = position_waiter.lock().take() {
                        let _ = tx.send(());
                    }
                }
                MpcEvent::Seek(pos) => {
                    state.lock().observe_position(Some(pos));
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
                    file_ready.store(false, Ordering::SeqCst);
                }
                MpcEvent::Shutdown => break,
            }
        }
        connected.store(false, Ordering::SeqCst);
        file_ready.store(false, Ordering::SeqCst);
    })
}

fn mpc_startup_arguments(args: &[String], listener_hwnd: isize) -> Vec<String> {
    let mut full_args = args.to_vec();
    if !full_args
        .iter()
        .any(|arg| arg.eq_ignore_ascii_case("/open"))
    {
        full_args.push("/open".to_string());
    }
    if !full_args.iter().any(|arg| arg.eq_ignore_ascii_case("/new")) {
        full_args.push("/new".to_string());
    }
    full_args.push("/slave".to_string());
    full_args.push(listener_hwnd.to_string());
    full_args
}

fn mpc_load_state_ready(state_code: i32) -> bool {
    !matches!(state_code, 0 | 1 | 3)
}

fn apply_mpc_load_state(
    state: &Arc<Mutex<PlayerState>>,
    file_ready: &Arc<AtomicBool>,
    state_code: i32,
) {
    let ready = mpc_load_state_ready(state_code);
    file_ready.store(ready, Ordering::SeqCst);
    if !ready {
        state.lock().observe_paused(None);
    }
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
    use super::{
        apply_mpc_load_state, mpc_filename_from_path, mpc_load_state_ready, mpc_startup_arguments,
        split_mpc_fields, MpcMediaSettle, MpcMediaSettleAction, MpcShutdownSignal,
        MPC_EXISTENCE_CHECK_INTERVAL, MPC_MAX_RETRIES, MPC_PAUSE_TOGGLE_DELAY, MPC_RETRY_WAIT_TIME,
    };
    use crate::player::properties::PlayerState;
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[test]
    fn mpc_shutdown_signal_is_idempotent_and_wakes_waiters() {
        assert!(!MpcShutdownSignal::default().wait_timeout(std::time::Duration::ZERO));

        let signal = MpcShutdownSignal::default();
        let waiter_signal = signal.clone();
        let waiter = std::thread::spawn(move || {
            waiter_signal.wait_timeout(std::time::Duration::from_secs(1))
        });

        assert!(signal.request());
        assert!(!signal.request());
        assert!(waiter.join().expect("shutdown waiter should not panic"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn mpc_listener_teardown_releases_threads_window_state_and_startup_failure() {
        use crate::player::backend::PlayerKind;
        use windows::Win32::UI::WindowsAndMessaging::IsWindow;

        let baseline_states = super::win::live_listener_state_count();
        let (listener, event_rx) = super::start_listener().expect("listener should start");
        let hwnd = listener.hwnd();
        assert!(unsafe { IsWindow(Some(hwnd)).as_bool() });

        let event_thread = super::spawn_event_loop(super::EventLoopArgs {
            event_rx,
            listener_hwnd: listener.hwnd_raw(),
            state: Arc::new(Mutex::new(PlayerState::default())),
            connected: Arc::new(AtomicBool::new(false)),
            file_ready: Arc::new(AtomicBool::new(false)),
            switch_pause_calls: Arc::new(AtomicBool::new(false)),
            version: Arc::new(Mutex::new(None)),
            position_waiter: Arc::new(Mutex::new(None)),
            version_waiter: Arc::new(Mutex::new(None)),
        });
        listener.spawn_liveness_probe();
        assert_eq!(listener.owned_thread_count(), 2);

        listener.shutdown();
        event_thread
            .join()
            .expect("event receiver should stop after listener shutdown");

        assert_eq!(listener.owned_thread_count(), 0);
        assert!(!unsafe { IsWindow(Some(hwnd)).as_bool() });
        assert_eq!(super::win::live_listener_state_count(), baseline_states);

        let missing_player = std::env::temp_dir().join(format!(
            "syncplay-missing-mpc-{}-{}.exe",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time should follow the Unix epoch")
                .as_nanos()
        ));
        let result = super::MpcApiBackend::start(
            PlayerKind::MpcHc,
            &missing_player.to_string_lossy(),
            &[],
            None,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(super::win::live_listener_state_count(), baseline_states);
    }

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
    fn mpc_startup_arguments_match_original_slave_launch() {
        let args = mpc_startup_arguments(&[], 42);
        assert_eq!(args, vec!["/open", "/new", "/slave", "42"]);
    }

    #[test]
    fn mpc_startup_arguments_do_not_duplicate_original_flags() {
        let args = mpc_startup_arguments(&["/new".to_string(), "/open".to_string()], 42);
        assert_eq!(args, vec!["/new", "/open", "/slave", "42"]);
    }

    #[test]
    fn mpc_load_state_ready_matches_original_not_ready_states() {
        assert!(!mpc_load_state_ready(0));
        assert!(!mpc_load_state_ready(1));
        assert!(!mpc_load_state_ready(3));
        assert!(mpc_load_state_ready(2));
    }

    #[test]
    fn mpc_load_state_not_ready_clears_file_ready_only() {
        let state = Arc::new(Mutex::new(PlayerState {
            paused: Some(false),
            ..PlayerState::default()
        }));
        let file_ready = Arc::new(AtomicBool::new(true));

        apply_mpc_load_state(&state, &file_ready, 3);

        assert!(!file_ready.load(Ordering::SeqCst));
        assert_eq!(state.lock().paused, None);
    }

    #[test]
    fn mpc_liveness_probe_matches_original_ten_second_interval() {
        assert_eq!(
            MPC_EXISTENCE_CHECK_INTERVAL,
            std::time::Duration::from_secs(10)
        );
    }

    #[test]
    fn mpc_media_settle_force_pauses_before_target_and_seek() {
        let mut settle = MpcMediaSettle::new(true, 42.5);
        let mut actions = Vec::new();
        while let Some(action) = settle.next(Some(true)) {
            actions.push(action);
        }

        assert_eq!(actions.len(), MPC_MAX_RETRIES + 2);
        assert!(actions[..MPC_MAX_RETRIES].iter().all(|action| {
            *action
                == MpcMediaSettleAction::SetPaused {
                    paused: true,
                    delay: MPC_RETRY_WAIT_TIME,
                }
        }));
        assert_eq!(
            actions[MPC_MAX_RETRIES],
            MpcMediaSettleAction::SetPaused {
                paused: true,
                delay: std::time::Duration::ZERO,
            }
        );
        assert_eq!(
            actions.last(),
            Some(&MpcMediaSettleAction::SetPosition(42.5))
        );
    }

    #[test]
    fn mpc_media_settle_double_toggles_when_target_does_not_apply() {
        let mut settle = MpcMediaSettle::new(true, 19.0);
        let mut actions = Vec::new();
        let mut toggles = 0;
        loop {
            let observed = if settle.needs_pause_observation() {
                Some(toggles == 2)
            } else {
                None
            };
            let Some(action) = settle.next(observed) else {
                break;
            };
            if matches!(action, MpcMediaSettleAction::TogglePaused { .. }) {
                toggles += 1;
            }
            actions.push(action);
        }

        assert_eq!(toggles, 2);
        assert_eq!(
            actions
                .iter()
                .filter(|action| {
                    **action
                        == MpcMediaSettleAction::TogglePaused {
                            delay: MPC_PAUSE_TOGGLE_DELAY,
                        }
                })
                .count(),
            2
        );
        assert_eq!(
            actions.last(),
            Some(&MpcMediaSettleAction::SetPosition(19.0))
        );
    }

    #[test]
    fn mpc_media_settle_never_seeks_when_pause_does_not_converge() {
        let mut settle = MpcMediaSettle::new(true, 7.0);
        let mut actions = Vec::new();
        while let Some(action) = settle.next(Some(false)) {
            actions.push(action);
        }

        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, MpcMediaSettleAction::TogglePaused { .. }))
                .count(),
            MPC_MAX_RETRIES * 2
        );
        assert_eq!(actions.last(), Some(&MpcMediaSettleAction::Failed));
        assert!(!actions
            .iter()
            .any(|action| matches!(action, MpcMediaSettleAction::SetPosition(_))));
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
