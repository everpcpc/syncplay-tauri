import { invoke } from "@tauri-apps/api/tauri";

export interface ConnectionParams {
  host: string;
  port: number;
  username: string;
  room: string;
  password?: string;
}

export const tauriApi = {
  // Connection commands
  async connectToServer(params: ConnectionParams): Promise<void> {
    return invoke("connect_to_server", { ...params });
  },

  async disconnectFromServer(): Promise<void> {
    return invoke("disconnect_from_server");
  },

  async getConnectionStatus(): Promise<boolean> {
    return invoke("get_connection_status");
  },

  // Chat commands
  async sendChatMessage(message: string): Promise<void> {
    return invoke("send_chat_message", { message });
  },

  // Room commands
  async changeRoom(room: string): Promise<void> {
    return invoke("change_room", { room });
  },

  async setReady(isReady: boolean): Promise<void> {
    return invoke("set_ready", { isReady });
  },

  // Playlist commands
  async updatePlaylist(action: string, filename?: string): Promise<void> {
    return invoke("update_playlist", { action, filename });
  },
};
