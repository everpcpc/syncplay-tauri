import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { SyncplayConfig } from "../types/config";

// Type definitions matching backend events
interface ConnectionState {
  connected: boolean;
  server: string | null;
}

type TlsStatus =
  | "unknown"
  | "pending"
  | "accepted"
  | "enabled"
  | "unsupported"
  | "rejected"
  | "certificate-invalid"
  | "closed";

interface User {
  username: string;
  room: string;
  file: string | null;
  fileSize?: number | string | null;
  fileDuration?: number | null;
  isReady: boolean;
  isController: boolean;
}

interface ChatMessage {
  timestamp: string;
  username: string | null;
  message: string;
  messageType: string;
}

interface PlaylistState {
  items: string[];
  currentIndex: number | null;
}

interface PlayerState {
  filename: string | null;
  position: number | null;
  duration: number | null;
  paused: boolean | null;
  speed: number | null;
}

interface SyncplayStore {
  // State
  connection: ConnectionState;
  tlsStatus: TlsStatus;
  users: User[];
  messages: ChatMessage[];
  playlist: PlaylistState;
  player: PlayerState;
  rttMs: number | null;
  syncOffsetSeconds: number | null;
  config: SyncplayConfig | null;
  mediaIndexVersion: number;
  mediaIndexRefreshing: boolean;

  // Actions
  setConnectionStatus: (status: ConnectionState) => void;
  setTlsStatus: (status: TlsStatus) => void;
  setUsers: (users: User[]) => void;
  addMessage: (message: ChatMessage) => void;
  setPlaylist: (playlist: PlaylistState) => void;
  setPlayerState: (state: PlayerState) => void;
  setRttMs: (rttMs: number | null) => void;
  setSyncOffset: (offsetSeconds: number | null) => void;
  setConfig: (config: SyncplayConfig) => void;
  setMediaIndexVersion: (version: number) => void;
  setMediaIndexRefreshing: (refreshing: boolean) => void;

  // Event listener setup
  setupEventListeners: () => void;
}

let listenersInitialized = false;

const MAX_CHAT_MESSAGES = 1000;
const appendChatMessage = (messages: ChatMessage[], message: ChatMessage) =>
  [...messages, message].slice(-MAX_CHAT_MESSAGES);

export const useSyncplayStore = create<SyncplayStore>((set) => ({
  // Initial state
  connection: {
    connected: false,
    server: null,
  },
  tlsStatus: "unknown",
  users: [],
  messages: [],
  playlist: {
    items: [],
    currentIndex: null,
  },
  player: {
    filename: null,
    position: null,
    duration: null,
    paused: true,
    speed: 1.0,
  },
  rttMs: null,
  syncOffsetSeconds: null,
  config: null,
  mediaIndexVersion: 0,
  mediaIndexRefreshing: false,

  // Actions
  setConnectionStatus: (status) =>
    set(() => ({
      connection: status,
    })),

  setTlsStatus: (status) =>
    set(() => ({
      tlsStatus: status,
    })),

  setUsers: (users) =>
    set(() => ({
      users,
    })),

  addMessage: (message) =>
    set((state) => ({
      messages: appendChatMessage(state.messages, message),
    })),

  setPlaylist: (playlist) =>
    set(() => ({
      playlist,
    })),

  setPlayerState: (playerState) =>
    set((state) => ({
      player: { ...state.player, ...playerState },
    })),

  setRttMs: (rttMs) =>
    set(() => ({
      rttMs,
    })),

  setSyncOffset: (offsetSeconds) =>
    set(() => ({
      syncOffsetSeconds: offsetSeconds,
    })),

  setConfig: (config) =>
    set(() => ({
      config,
    })),

  setMediaIndexVersion: (version) =>
    set(() => ({
      mediaIndexVersion: version,
    })),

  setMediaIndexRefreshing: (refreshing) =>
    set(() => ({
      mediaIndexRefreshing: refreshing,
    })),

  // Setup event listeners from Tauri backend
  setupEventListeners: () => {
    if (listenersInitialized) {
      return;
    }
    listenersInitialized = true;

    const listenSafe = <T>(eventName: string, handler: (event: { payload: T }) => void) => {
      listen<T>(eventName, handler).catch((error) => {
        console.error(`Failed to listen for ${eventName}`, error);
      });
    };

    // Connection status changes
    listenSafe<ConnectionState>("connection-status-changed", (event) => {
      set(() => ({
        connection: event.payload,
        rttMs: null,
        syncOffsetSeconds: null,
      }));
    });

    listenSafe<{ status: TlsStatus }>("tls-status-changed", (event) => {
      set(() => ({
        tlsStatus: event.payload.status,
      }));
    });

    // User list updates
    listenSafe<{ users: User[] }>("user-list-updated", (event) => {
      set(() => ({
        users: event.payload.users,
      }));
    });

    // Chat messages
    listenSafe<ChatMessage>("chat-message-received", (event) => {
      set((state) => ({
        messages: appendChatMessage(state.messages, event.payload),
      }));
    });

    // Playlist updates
    listenSafe<PlaylistState>("playlist-updated", (event) => {
      set(() => ({
        playlist: event.payload,
      }));
    });

    // Player state updates
    listenSafe<PlayerState>("player-state-changed", (event) => {
      set((state) => ({
        player: { ...state.player, ...event.payload },
        // No file loaded (e.g. player disconnected) → offset is meaningless
        syncOffsetSeconds: event.payload.filename === null ? null : state.syncOffsetSeconds,
      }));
    });

    // Own offset from the room-global playback position
    listenSafe<{ offsetSeconds: number }>("sync-offset-updated", (event) => {
      set(() => ({
        syncOffsetSeconds: event.payload.offsetSeconds,
      }));
    });

    listenSafe<{ rttMs: number }>("ping-updated", (event) => {
      set(() => ({
        rttMs: event.payload.rttMs,
      }));
    });

    // Config updates
    listenSafe<SyncplayConfig>("config-updated", (event) => {
      set(() => ({
        config: event.payload,
      }));
    });

    listenSafe<{ timestamp: string }>("media-index-updated", (event) => {
      const parsed = Date.parse(event.payload.timestamp);
      set(() => ({
        mediaIndexVersion: Number.isNaN(parsed) ? Date.now() : parsed,
      }));
    });

    listenSafe<{ refreshing: boolean }>("media-index-refreshing", (event) => {
      set(() => ({
        mediaIndexRefreshing: event.payload.refreshing,
      }));
    });

    void invoke<boolean>("get_media_index_refreshing")
      .then((refreshing) => {
        set(() => ({
          mediaIndexRefreshing: refreshing,
        }));
      })
      .catch((error) => {
        console.error("Failed to read media index status", error);
      });
  },
}));
