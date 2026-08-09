export interface SyncplayUser {
  username: string;
  room: string;
  file: string | null;
  fileSize?: number | string | null;
  fileDuration?: number | null;
  isReady: boolean;
  isController: boolean;
}

export interface UserListEventPayload {
  users: SyncplayUser[];
  rooms?: string[];
}

export interface ServerRoomFeatures {
  managedRooms: boolean;
  persistentRooms: boolean;
}
