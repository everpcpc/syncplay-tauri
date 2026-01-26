# Syncplay Tauri

A modern, cross-platform Syncplay client built with Tauri (Rust backend + React frontend).

## Overview

Syncplay Tauri is a complete rewrite of the Syncplay client using modern technologies. It provides synchronized video playback across multiple users, allowing you to watch videos together in real-time.

### Features

- **Cross-platform**: Works on Windows, macOS, and Linux
- **Modern UI**: Clean, responsive interface built with React and Tailwind CSS
- **MPV Integration**: Full support for MPV player via JSON IPC
- **Real-time Sync**: Smart synchronization algorithm with configurable thresholds
- **Chat System**: Built-in chat with command support
- **Playlist Management**: Shared playlist with navigation controls
- **Configuration**: Persistent settings with JSON-based storage
- **Notifications**: Real-time error and status notifications

## Quick Start

### Prerequisites

- **Rust**: 1.70 or later
- **Node.js**: 18 or later
- **MPV**: Latest version with JSON IPC support

### Development

```bash
# Install dependencies
npm install

# Run in development mode
npm run tauri dev
```

### Building

```bash
# Build for production
npm run tauri build

# Run tests
cd src-tauri && cargo test
```

## Usage

### Connecting to a Server

1. Click "Connect" in the header
2. Enter server details:
   - **Server**: syncplay.pl (default)
   - **Port**: 8999 (default)
   - **Username**: Your display name (required)
   - **Room**: Room name (default: "default")
   - **Password**: Optional server password
3. Click "Connect"

### Chat Commands

- `/room <name>` - Change to a different room
- `/list` - List all users in the current room
- `/help` - Show available commands
- `/ready` - Mark yourself as ready
- `/unready` - Mark yourself as not ready

### Playlist Management

- **Add File**: Click "Add File" button (file picker integration)
- **Remove**: Click ✕ next to any playlist item
- **Navigate**: Use Previous/Next buttons
- **Clear**: Remove all items from playlist

### Settings

Click "Settings" to configure:

- **Server Tab**: Default server, port, and password
- **User Tab**: Username, default room, OSD settings, UI preferences
- **Advanced Tab**: Sync thresholds, slowdown rate, OSD duration

## Configuration

Settings are stored in JSON format at platform-specific locations:

- **Linux**: `~/.config/syncplay-tauri/config.json`
- **macOS**: `~/Library/Application Support/com.syncplay.syncplay-tauri/config.json`
- **Windows**: `%APPDATA%\syncplay\syncplay-tauri\config\config.json`

### Configuration Structure

```json
{
  "server": {
    "host": "syncplay.pl",
    "port": 8999,
    "password": null
  },
  "user": {
    "username": "YourName",
    "default_room": "default",
    "seek_threshold_rewind": 4.0,
    "seek_threshold_fastforward": 5.0,
    "slowdown_threshold": 1.5,
    "slowdown_reset_threshold": 0.5,
    "slowdown_rate": 0.95,
    "show_osd": true,
    "osd_duration": 3000,
    "show_playlist": true,
    "auto_connect": false
  },
  "recent_servers": []
}
```

## Architecture

```
┌─────────────────────────────────────────┐
│         React Frontend (TypeScript)      │
│  - Zustand state management             │
│  - Component-based UI                   │
│  - Real-time event handling             │
└──────────────┬──────────────────────────┘
               │ Tauri Commands (IPC)
┌──────────────┴──────────────────────────┐
│         Rust Backend (Tokio)            │
│  - Network: TCP + TLS + JSON protocol   │
│  - Client: Sync logic + state mgmt      │
│  - Player: MPV JSON IPC integration     │
│  - Config: JSON persistence             │
└──────────────┬──────────────────────────┘
               │ JSON IPC
         ┌─────┴─────┐
         │ MPV Player │
         └───────────┘
```

### Project Structure

```
syncplay-tauri/
├── src/                      # React frontend
│   ├── components/          # UI components
│   │   ├── chat/           # Chat panel
│   │   ├── connection/     # Connection dialog
│   │   ├── layout/         # Main layout
│   │   ├── notifications/  # Notification system
│   │   ├── playlist/       # Playlist panel
│   │   ├── player/         # Player status
│   │   ├── settings/       # Settings dialog
│   │   └── users/          # User list
│   ├── store/              # Zustand state management
│   └── services/           # Tauri API wrappers
├── src-tauri/               # Rust backend
│   └── src/
│       ├── app_state.rs    # Global application state
│       ├── client/         # Client logic
│       │   ├── chat.rs     # Chat system
│       │   ├── playlist.rs # Playlist management
│       │   ├── state.rs    # Client state
│       │   └── sync.rs     # Sync engine
│       ├── commands/       # Tauri command handlers
│       ├── config/         # Configuration system
│       ├── network/        # Network protocol
│       └── player/         # MPV integration
└── README.md               # This file
```

## Synchronization Algorithm

The client uses a smart synchronization algorithm:

1. **Position Difference**: Calculates difference between local and global playback positions
2. **Seek Threshold**: If difference exceeds threshold (4s rewind, 5s fastforward), seeks to correct position
3. **Slowdown**: If difference is moderate (>1.5s), temporarily slows playback to 0.95x speed
4. **Reset**: When difference is small (<0.5s), resets speed to 1.0x
5. **Pause Sync**: Automatically syncs pause/play state with other users

## Development

### Running Tests

```bash
# Rust tests
cd src-tauri
cargo test

# All tests should pass (31 unit tests)
```

### Code Quality

```bash
# Rust formatting
cargo fmt

# Rust linting
cargo clippy

# TypeScript checking
npm run type-check
```

## Protocol Compatibility

This client is compatible with Syncplay protocol version 1.7.x and can connect to official Syncplay servers.

### Supported Message Types

- **Hello**: Initial handshake with server
- **Set**: Update settings (file, position, paused, playlist)
- **State**: Synchronization state updates
- **List**: User list updates
- **Chat**: Chat messages
- **Error**: Error notifications
- **TLS**: TLS negotiation

## Troubleshooting

### MPV Connection Issues

If the client can't connect to MPV:

1. Ensure MPV is installed and in your PATH
2. Start MPV with IPC enabled:
   ```bash
   mpv --input-ipc-server=/tmp/mpvsocket video.mp4
   ```
3. Check the socket path matches the client configuration

### Connection Failures

If you can't connect to a server:

1. Check your internet connection
2. Verify the server address and port
3. Check if the server requires a password
4. Try the official server: syncplay.pl:8999

### Sync Issues

If synchronization isn't working:

1. Check that all users are in the same room
2. Verify everyone is playing the same file
3. Adjust sync thresholds in Settings > Advanced
4. Check network latency

## Contributing

Contributions are welcome! Please:

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Add tests if applicable
5. Submit a pull request

## License

Apache-2.0

## Acknowledgments

- Original Syncplay project: https://syncplay.pl/
- Tauri framework: https://tauri.app/
- MPV player: https://mpv.io/

## Support

For issues and questions:
- GitHub Issues: [Report a bug](https://github.com/everpcpc/syncplay-rs/issues)
- Original Syncplay: https://syncplay.pl/

---

**Status**: 🎉 Production Ready
