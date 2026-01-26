import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";

// Type definitions matching backend events
interface ConnectionState {
  connected: boolean;
  server: string | null;
}

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
  users: User[];
  messages: ChatMessage[];
  playlist: PlaylistState;
  player: PlayerState;

  // Actions
  setConnectionStatus: (status: ConnectionState) => void;
  setUsers: (users: User[]) => void;
  addMessage: (message: ChatMessage) => void;
  setPlaylist: (playlist: PlaylistState) => void;
  setPlayerState: (state: PlayerState) => void;

  // Event listener setup
  setupEventListeners: () => void;
}

export const useSyncplayStore = create<SyncplayStore>((set) => ({
  // Initial state
  connection: {
    connected: false,
    server: null,
  },
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

  // Actions
  setConnectionStatus: (status) =>
    set(() => ({
      connection: status,
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

  // Setup event listeners from Tauri backend
  setupEventListeners: () => {
    // Connection status changes
    listen<ConnectionState>("connection-status-changed", (event) => {
      set(() => ({
        connection: event.payload,
      }));
    });

    // User list updates
    listen<{ users: User[] }>("user-list-updated", (event) => {
      set(() => ({
        users: event.payload.users,
      }));
    });

    // Chat messages
    listen<ChatMessage>("chat-message-received", (event) => {
      set((state) => ({
        messages: [...state.messages, event.payload],
      }));
    });

    // Playlist updates
    listen<PlaylistState>("playlist-updated", (event) => {
      set(() => ({
        playlist: event.payload,
      }));
    });

    // Player state updates
    listen<PlayerState>("player-state-changed", (event) => {
      set((state) => ({
        player: { ...state.player, ...event.payload },
      }));
    });
  },
}));
