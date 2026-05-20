# Syncplay parity notes — Sprint 1 baseline

Sprint 1 is intentionally a foundation sprint: no user-visible behavior changes are claimed here. These notes record the Python reference files examined and the Rust/Tauri modules/tests that now form the regression baseline for later lifecycle and protocol work.

## Repository rules checked

Searched under both repositories for `AGENTS.md`, `CLAUDE.md`, `.cursorrules`, and `CONTRIBUTING.md`.

- `/Volumes/Sources/syncplay-rs`: no `AGENTS.md` or equivalent rule file found.
- `/Volumes/Sources/syncplay`: no `AGENTS.md` or equivalent rule file found.

Only README files were present, so Sprint 1 follows the existing Rust style and keeps changes minimal.

## Original Syncplay reference files examined

### Protocol and client state

- `/Volumes/Sources/syncplay/syncplay/protocols.py`
  - `JSONCommandProtocol`: newline-delimited JSON messages; dispatches `Hello`, `Set`, `List`, `State`, `Error`, `Chat`, and `TLS`.
  - `SyncClientProtocol.connectionMade`: starts TLS negotiation when supported, otherwise sends `Hello`.
  - `SyncClientProtocol.sendHello`: sends compatibility `version = "1.2.255"`, `realversion`, room, password, and feature list.
  - `SyncClientProtocol.handleHello`: stores username/room/server version, marks connected, sends current file, and applies server features.
  - `SyncClientProtocol.handleState` / `sendState`: applies ping latency fields (`latencyCalculation`, `clientLatencyCalculation`, `clientRtt`, `serverRtt`), handles `ignoringOnTheFly`, updates global playstate, then always replies with local state/ping.
  - `SyncClientProtocol.dropWithError`: reports error, stops retries, and drops the transport.

- `/Volumes/Sources/syncplay/syncplay/client.py`
  - `SyncplayClient.askPlayer` and `checkIfConnected`: periodic player polling also checks `PROTOCOL_TIMEOUT` and drops stale server connections.
  - `updatePlayerStatus`: detects local pause/seek, readiness toggles, rewind/advance grace, and sends state changes.
  - `stop`: destroys protocol, drops player, drops UI, and stops reactor.
  - `_performRetryStateReset` / `manualReconnect`: reset transient protocol state and mark playlist restoration during reconnect.

### Player adapters

- `/Volumes/Sources/syncplay/syncplay/players/mpv.py`
  - `MpvPlayer.run`: checks mpv version (`>= 0.23.0`), records OSC capability (`>= 0.28.0`), and builds MPV JSON IPC arguments.
  - `_setProperty`, `setPaused`, `setSpeed`, `openFile`, `askForStatus`: JSON IPC control and status query behavior.
  - `drop`: closes the mpv listener/process path on client shutdown.

- `/Volumes/Sources/syncplay/syncplay/players/vlc.py`
  - `VLCClientFactory`: retries VLC RC connection for a short launch window, drops player on later connection loss, terminates process in `closeVLC`.
  - `VlcPlayer.initWhenConnected`: waits for readiness then calls `client.initPlayer`.
  - `_onFileUpdate`: refreshes file metadata, then reapplies global paused/position.
  - `askForStatus` / `getCalculatedPosition`: estimates position when VLC is late and warns on latency.
  - `drop`: closes VLC control/process state.

- `/Volumes/Sources/syncplay/syncplay/players/mpc.py`
  - `MpcHcApi.startMpc`: starts MPC in `/slave` mode and waits for window/API readiness.
  - `handleCommand`: tracks load state, play state, current file, seek notifications, current position, disconnect, and version callbacks.
  - `MpcPlayer.drop`: closes the MPC API/player lifecycle path.

- `/Volumes/Sources/syncplay/syncplay/players/mplayer.py`
  - `_preparePlayer`: pauses at startup, initializes player, and sends initial file update.
  - `askForStatus`: queries pause/position through slave stdin/stdout.
  - `openFile`: sends `loadfile`, refreshes file info, reapplies global paused/position.
  - `setPaused`: toggles via `pause` only when target differs from cached state.
  - `drop`: sends shutdown/cleanup to the slave listener.

## Rust/Tauri modules mapped for later sprints

- Protocol codec/messages: `/Volumes/Sources/syncplay-rs/src-tauri/src/network/protocol.rs`, `/Volumes/Sources/syncplay-rs/src-tauri/src/network/messages.rs`
- TCP/TLS connection loop: `/Volumes/Sources/syncplay-rs/src-tauri/src/network/connection.rs`, `/Volumes/Sources/syncplay-rs/src-tauri/src/network/tls.rs`
- Tauri connection/session commands: `/Volumes/Sources/syncplay-rs/src-tauri/src/commands/connection.rs`
- Global lifecycle state: `/Volumes/Sources/syncplay-rs/src-tauri/src/app_state.rs`
- Player abstraction and lifecycle controller: `/Volumes/Sources/syncplay-rs/src-tauri/src/player/backend.rs`, `/Volumes/Sources/syncplay-rs/src-tauri/src/player/controller.rs`
- MPV: `/Volumes/Sources/syncplay-rs/src-tauri/src/player/mpv_backend.rs`, `/Volumes/Sources/syncplay-rs/src-tauri/src/player/mpv_ipc.rs`
- VLC: `/Volumes/Sources/syncplay-rs/src-tauri/src/player/vlc_rc.rs`, `/Volumes/Sources/syncplay-rs/src-tauri/src/player/vlc_syncplay.rs`
- MPC: `/Volumes/Sources/syncplay-rs/src-tauri/src/player/mpc_api.rs`, `/Volumes/Sources/syncplay-rs/src-tauri/src/player/mpc_web.rs`
- MPlayer: `/Volumes/Sources/syncplay-rs/src-tauri/src/player/mplayer_slave.rs`

## Sprint 1 regression harness additions

- `src-tauri/src/player/backend.rs`
  - Adds `FakePlayerBackend` under `#[cfg(test)]` implementing `PlayerBackend` without spawning MPV/VLC/MPC/MPlayer.
  - Records player commands (`setPaused`, `setPosition`, `loadFile`, `shutdown`, OSD/chat, etc.) for lifecycle assertions.

- `src-tauri/src/network/fake_server.rs`
  - Adds `FakeSyncplayServer` under `#[cfg(test)]` using an ephemeral `127.0.0.1` listener and the production `SyncplayCodec`.
  - Allows cargo tests to assert protocol JSON exchange without public servers.

- Foundation tests:
  - `player::backend::tests::fake_player_records_commands_without_process`
  - `player::controller::tests::stop_player_shuts_down_fake_backend_and_is_idempotent`
  - `network::connection::tests::connection_exchanges_messages_with_fake_syncplay_server`


## Sprint 4 non-MPV lifecycle/control parity notes

Sprint 4 extends the Sprint 1 player mapping to VLC, MPC-HC/MPC-BE, and MPlayer. The current implementation intentionally keeps player-specific command details inside each backend, while file-change resynchronisation that depends on server/global state remains in `src-tauri/src/player/controller.rs`.

### VLC Syncplay interface backend (`vlc_syncplay.rs`)

- Pause uses Syncplay's VLC Lua interface command `set-playstate` with the target paused state.
- Seek uses `set-position` and speed uses `set-rate`, matching the original Syncplay VLC interface path.
- File loading uses `load-file` through the Lua interface, matching original Syncplay's dedicated Syncplay VLC interface rather than the generic RC playlist command.
- After a detected file update, Rust reapplies global pause and position from `sync_generic_after_file_change` in the controller. This mirrors original Syncplay's `_onFileUpdate()` intent, but the timing is controller-driven instead of being called from inside the VLC backend because Rust's backend trait does not own client/global state.

### VLC RC fallback backend (`vlc_rc.rs`)

- The RC backend is a fallback for VLC instances that expose only the standard RC interface. It sends `pause`/`play`, `seek`, and `rate` commands in the RC syntax.
- Deliberate deviation: RC file loading uses `add <path>` rather than original Syncplay's Lua-interface `load-file` command. Standard VLC RC does not expose the Syncplay Lua `load-file` command; the full parity path is `VlcSyncplayBackend`.
- RC write/shutdown calls are bounded by a 500 ms timeout and mark the backend disconnected on EOF/write failure so the controller can clear stale AppState.

### MPC-HC / MPC-BE API backend (`mpc_api.rs`)

- MPC API control uses the original command IDs: `CMD_PAUSE`/`CMD_PLAY` (plus `CMD_PLAYPAUSE` correction for affected versions), `CMD_SETPOSITION`, `CMD_SETSPEED`, and `CMD_OPENFILE`.
- Commands that require a loaded file retry briefly while MPC reports a ready load state, matching the original Syncplay retry/wait concept around the MPC slave API.
- After file changes, `sync_mpc_after_file_change` performs the original-style pause-stabilisation sequence before applying global pause and position. This remains in the controller so it can read current global Syncplay state.
- Windows-only verification note: the real MPC window-message API is compiled behind `#[cfg(windows)]` and cannot be executed on the current macOS development platform. Local verification covers non-Windows compile stubs and fake lifecycle tests; real MPC-HC/MPC-BE behavior remains unverified locally and must be smoke-tested on Windows.

### MPC Web backend (`mpc_web.rs`)

- The web backend uses MPC's HTTP command surface for pause, seek, speed, and open-file where the window-message API is unavailable or not selected.
- Poll/request failures mark the backend disconnected, allowing the controller to clear AppState rather than relying on stale `player_connected` presence.

### MPlayer slave backend (`mplayer_slave.rs`)

- Pause uses the original slave-mode semantics: send `pause` only when the cached pause state differs from the target.
- Seek uses `set_property time_pos <position>`, speed uses `set_property speed <speed>`, and file loading uses `loadfile "<path>"` without an extra mode argument, matching original Syncplay's MPlayer slave commands.
- Deliberate architecture-specific deviation: original Syncplay's `openFile()` immediately calls `_onFileUpdate()` inside the MPlayer adapter and then reapplies global paused/position there. In Rust, `MplayerBackend::load_file()` only sends `loadfile`; after the reader observes the file metadata change, `spawn_player_state_loop` sends the file update and `sync_generic_after_file_change` reapplies global pause/position from AppState. This preserves the command intent while avoiding a backend dependency on client/global state.

### AppState freshness and shutdown

- `AppState::is_player_connected()` now consults the backend's `is_connected()` result instead of only checking that a backend object is present.
- VLC RC/Syncplay, MPlayer, MPC Web, and MPC API disconnect/write-failure paths set backend connection state false. The controller state loop clears `state.player`, process/spawn metadata, and emits an empty `player-state-changed` event when a backend becomes disconnected.
- `stop_player` still removes the backend/process from AppState before awaiting shutdown and bounds backend/process teardown with timeouts, so repeated stop/drop calls remain idempotent even if IPC has already gone away.
