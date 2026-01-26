# Syncplay Tauri

A modern, cross-platform Syncplay client built with Tauri (Rust backend + React frontend).

## Project Status

This is a rewrite of the Syncplay client using modern technologies. The client is protocol-compatible with existing Syncplay servers (version 1.7.x).

### Implementation Progress

- [x] Phase 1: Project Setup
- [ ] Phase 2: Network Layer
- [ ] Phase 3: MPV Integration
- [ ] Phase 4: Core Client Logic
- [ ] Phase 5: Playlist Management
- [ ] Phase 6: Chat System
- [ ] Phase 7: Tauri Commands
- [ ] Phase 8: React Frontend
- [ ] Phase 9: Configuration
- [ ] Phase 10: Polish & Testing

## Architecture

```
┌─────────────────────────────────────────┐
│         React Frontend (TypeScript)      │
│  - Room/User/Chat/Playlist components   │
│  - Zustand state management             │
│  - Tauri API integration                │
└──────────────┬──────────────────────────┘
               │ Tauri Commands (IPC)
┌──────────────┴──────────────────────────┐
│         Rust Backend (Tokio)            │
│  - Network: TCP + TLS + JSON protocol   │
│  - Client: Sync logic + state mgmt      │
│  - Player: MPV JSON IPC integration     │
│  - Config: Settings persistence         │
└──────────────┬──────────────────────────┘
               │ JSON IPC
         ┌─────┴─────┐
         │ MPV Player │
         └───────────┘
```

## Development

### Prerequisites

- Rust 1.70+
- Node.js 18+
- npm or yarn

### Setup

```bash
# Install dependencies
npm install

# Run in development mode
npm run tauri dev

# Build for production
npm run tauri build
```

### Project Structure

```
syncplay-rs/
├── src/                    # React frontend
│   ├── components/         # React components
│   ├── store/             # Zustand state management
│   ├── services/          # Tauri API wrappers
│   └── hooks/             # Custom React hooks
├── src-tauri/             # Rust backend
│   └── src/
│       ├── network/       # Protocol & connection
│       ├── player/        # MPV integration
│       ├── client/        # Sync logic
│       ├── commands/      # Tauri commands
│       └── config/        # Settings
└── dist/                  # Built frontend
```

## Features

### Planned

- Connect to Syncplay servers
- Synchronize video playback with other users
- Chat with room members
- Shared playlist management
- MPV player integration
- Ready state system
- TLS/SSL support
- Cross-platform (Windows, macOS, Linux)

## License

Apache-2.0

## References

- Original Syncplay: https://github.com/Syncplay/syncplay
- Tauri: https://tauri.app/
- MPV: https://mpv.io/
