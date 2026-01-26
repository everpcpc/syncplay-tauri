import { create } from "zustand";

interface ConnectionState {
  connected: boolean;
  serverHost: string | null;
  serverPort: number | null;
}

interface UserListState {
  users: Array<{
    username: string;
    room: string;
    file: string | null;
    isReady: boolean;
  }>;
}

interface ChatState {
  messages: Array<{
    timestamp: Date;
    username: string;
    message: string;
  }>;
}

interface PlaylistState {
  items: string[];
  currentIndex: number;
}

interface PlayerState {
  filename: string | null;
  position: number;
  duration: number;
  paused: boolean;
}

interface SyncplayStore {
  connection: ConnectionState;
  userList: UserListState;
  chat: ChatState;
  playlist: PlaylistState;
  player: PlayerState;

  // Actions
  setConnected: (connected: boolean, host?: string, port?: number) => void;
  addChatMessage: (username: string, message: string) => void;
  updateUserList: (users: UserListState["users"]) => void;
  updatePlaylist: (items: string[], currentIndex: number) => void;
  updatePlayerState: (state: Partial<PlayerState>) => void;
}

export const useSyncplayStore = create<SyncplayStore>((set) => ({
  connection: {
    connected: false,
    serverHost: null,
    serverPort: null,
  },
  userList: {
    users: [],
  },
  chat: {
    messages: [],
  },
  playlist: {
    items: [],
    currentIndex: 0,
  },
  player: {
    filename: null,
    position: 0,
    duration: 0,
    paused: true,
  },

  setConnected: (connected, host, port) =>
    set((state) => ({
      connection: {
        ...state.connection,
        connected,
        serverHost: host || null,
        serverPort: port || null,
      },
    })),

  addChatMessage: (username, message) =>
    set((state) => ({
      chat: {
        messages: [
          ...state.chat.messages,
          { timestamp: new Date(), username, message },
        ],
      },
    })),

  updateUserList: (users) =>
    set(() => ({
      userList: { users },
    })),

  updatePlaylist: (items, currentIndex) =>
    set(() => ({
      playlist: { items, currentIndex },
    })),

  updatePlayerState: (playerState) =>
    set((state) => ({
      player: { ...state.player, ...playerState },
    })),
}));
