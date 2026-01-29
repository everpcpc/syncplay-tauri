import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";
import { SyncplayConfig } from "../types/config";

// Type definitions matching backend events
interface ConnectionState {
  connected: boolean;
  server: string | null;
}

type TlsStatus = "unknown" | "pending" | "enabled" | "unsupported";

interface User {
  username: string;
  room: string;
  file: string | null;
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
  config: SyncplayConfig | null;

  // Actions
  setConnectionStatus: (status: ConnectionState) => void;
  setTlsStatus: (status: TlsStatus) => void;
  setUsers: (users: User[]) => void;
  addMessage: (message: ChatMessage) => void;
  setPlaylist: (playlist: PlaylistState) => void;
  setPlayerState: (state: PlayerState) => void;
  setConfig: (config: SyncplayConfig) => void;

  // Event listener setup
  setupEventListeners: () => void;
}

let listenersInitialized = false;

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
  config: null,

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
      messages: [...state.messages, message],
    })),

  setPlaylist: (playlist) =>
    set(() => ({
      playlist,
    })),

  setPlayerState: (playerState) =>
    set((state) => ({
      player: { ...state.player, ...playerState },
    })),

  setConfig: (config) =>
    set(() => ({
      config,
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
        messages: [...state.messages, event.payload],
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
      }));
    });

    // Config updates
    listenSafe<SyncplayConfig>("config-updated", (event) => {
      set(() => ({
        config: event.payload,
      }));
    });
  },
}));
