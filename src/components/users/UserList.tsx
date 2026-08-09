import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { LuCheck, LuCircle, LuPencilLine, LuUsers } from "react-icons/lu";
import { useSyncplayStore } from "../../store";
import { useNotificationStore } from "../../store/notifications";
import { RoomManagerDialog } from "./RoomManagerDialog";

// Stable per-user avatar hue, derived from the username so every
// participant gets a recognizable color chip without a server avatar.
const avatarHueFor = (username: string) => {
  let hash = 0;
  for (let i = 0; i < username.length; i += 1) {
    hash = (hash * 31 + username.charCodeAt(i)) >>> 0;
  }
  return hash % 360;
};

export function UserList() {
  const users = useSyncplayStore((state) => state.users);
  const connection = useSyncplayStore((state) => state.connection);
  const config = useSyncplayStore((state) => state.config);
  const addNotification = useNotificationStore((state) => state.addNotification);
  const [showRoomManager, setShowRoomManager] = useState(false);

  const currentUsername = config?.user.username ?? null;
  const currentUser = users.find((user) => user.username === currentUsername);
  const isReady = currentUser?.isReady ?? false;

  const currentRoom = currentUser?.room ?? config?.user.default_room ?? "Room";

  const handleToggleReady = () => {
    if (!connection.connected) {
      addNotification({
        type: "warning",
        message: "Connect to a server first",
      });
      return;
    }
    void invoke("set_ready", { isReady: !isReady }).catch((error) => {
      const message =
        typeof error === "string"
          ? error
          : (error as { message?: string })?.message || "Unknown error";
      addNotification({
        type: "error",
        message: `Failed to update ready state: ${message}`,
      });
    });
  };

  if (!connection.connected) {
    return (
      <div className="flex flex-col h-full min-h-0 min-w-0 gap-2">
        <div className="flex items-center justify-between gap-2">
          <div className="flex items-center gap-2 min-w-0">
            <LuUsers className="app-icon app-text-muted" />
            <span className="text-sm font-semibold truncate">{currentRoom}</span>
          </div>
          <button
            onClick={() => setShowRoomManager(true)}
            className="btn-neutral app-icon-button"
            aria-label="Rooms"
          >
            <LuPencilLine className="app-icon" />
          </button>
        </div>
        <p className="app-text-muted text-sm">Not connected</p>
        <RoomManagerDialog isOpen={showRoomManager} onClose={() => setShowRoomManager(false)} />
      </div>
    );
  }

  const normalizeFileSize = (value: number | string | null | undefined) => {
    if (value === null || value === undefined) return null;
    if (typeof value === "string") return value;
    return value > 0 ? value : null;
  };

  const formatFileSize = (value: number | string | null | undefined) => {
    if (value === null || value === undefined) return "--";
    if (typeof value === "string") return value;
    if (value <= 0) return "--";
    const units = ["B", "KB", "MB", "GB", "TB"];
    let size = value;
    let index = 0;
    while (size >= 1024 && index < units.length - 1) {
      size /= 1024;
      index += 1;
    }
    const precision = size >= 100 ? 0 : size >= 10 ? 1 : 2;
    return `${size.toFixed(precision)} ${units[index]}`;
  };

  const formatDuration = (duration: number | null | undefined) => {
    if (duration === null || duration === undefined || duration <= 0) return "--";
    const totalSeconds = Math.floor(duration);
    const hours = Math.floor(totalSeconds / 3600);
    const minutes = Math.floor((totalSeconds % 3600) / 60);
    const seconds = totalSeconds % 60;
    const time = `${minutes.toString().padStart(2, "0")}:${seconds.toString().padStart(2, "0")}`;
    return hours > 0 ? `${hours}:${time}` : time;
  };

  const hasSameFileName = (a?: string | null, b?: string | null) =>
    a && b ? a.toLowerCase() === b.toLowerCase() : false;

  const hasSameFileSize = (a?: number | string | null, b?: number | string | null) => {
    const left = normalizeFileSize(a);
    const right = normalizeFileSize(b);
    if (left === null || right === null) return false;
    if (typeof left === "string" || typeof right === "string") {
      return left === right;
    }
    return Math.abs(left - right) < 1;
  };

  const hasSameDuration = (a?: number | null, b?: number | null) => {
    if (a === null || a === undefined || b === null || b === undefined) return false;
    return Math.abs(a - b) < 1;
  };

  const currentUserFile = currentUser?.file ?? null;
  const currentUserSize = currentUser?.fileSize;
  const currentUserDuration = currentUser?.fileDuration;
  const sortedUsers = currentUsername
    ? [
        ...users.filter((user) => user.username === currentUsername),
        ...users.filter((user) => user.username !== currentUsername),
      ]
    : users;

  return (
    <div className="flex flex-col h-full min-h-0 min-w-0 gap-2">
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-2 min-w-0">
          <LuUsers className="app-icon app-text-muted" />
          <span className="text-sm font-semibold truncate">{currentRoom}</span>
          <span className="text-xs app-text-muted shrink-0">({users.length})</span>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={handleToggleReady}
            className={`app-icon-button ${isReady ? "btn-primary" : "btn-neutral"}`}
            aria-label={isReady ? "Ready" : "Not ready"}
          >
            {isReady ? <LuCheck className="app-icon" /> : <LuCircle className="app-icon" />}
          </button>
          <button
            onClick={() => setShowRoomManager(true)}
            className="btn-neutral app-icon-button"
            aria-label="Rooms"
          >
            <LuPencilLine className="app-icon" />
          </button>
        </div>
      </div>

      {users.length === 0 ? (
        <div className="flex-1 min-h-0 overflow-auto">
          <p className="app-text-muted text-sm">No users in room</p>
        </div>
      ) : (
        <div className="space-y-2 flex-1 min-h-0 overflow-auto pr-1">
          {sortedUsers.map((user) => (
            <div key={user.username} className="app-panel-muted rounded-md p-3 text-sm">
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-2 min-w-0">
                  <span
                    className="app-avatar"
                    style={{ backgroundColor: `hsl(${avatarHueFor(user.username)} 62% 46%)` }}
                    aria-hidden="true"
                  >
                    {(user.username.trim()[0] ?? "?") as string}
                  </span>
                  <span className="font-medium truncate flex-1 min-w-0">{user.username}</span>
                  <span
                    className={`text-[10px] px-2 py-0 rounded-full ${
                      user.isReady ? "app-tag-success" : "app-tag-muted"
                    }`}
                  >
                    {user.isReady ? "Ready" : "Not Ready"}
                  </span>
                  {currentUsername === user.username && (
                    <span className="text-[10px] app-tag-accent px-2 py-0 rounded-full">You</span>
                  )}
                </div>
                {user.isController && (
                  <span className="text-xs app-tag-accent px-2 py-0.5 rounded">Controller</span>
                )}
              </div>

              {user.file && (
                <div className="mt-1 space-y-1">
                  <div
                    className={`text-xs truncate ${
                      user.room === currentRoom && !hasSameFileName(user.file, currentUserFile)
                        ? "app-text-warning"
                        : "app-text-muted"
                    }`}
                  >
                    File: {user.file}
                  </div>
                  <div className="flex flex-wrap items-center gap-2 text-xs">
                    <span
                      className={`${
                        user.room === currentRoom &&
                        !hasSameFileSize(user.fileSize, currentUserSize)
                          ? "app-text-warning"
                          : "app-text-muted"
                      }`}
                    >
                      Size: {formatFileSize(user.fileSize ?? null)}
                    </span>
                    <span className="app-text-muted">/</span>
                    <span
                      className={`${
                        user.room === currentRoom &&
                        !hasSameDuration(user.fileDuration ?? null, currentUserDuration ?? null)
                          ? "app-text-warning"
                          : "app-text-muted"
                      }`}
                    >
                      Duration: {formatDuration(user.fileDuration ?? null)}
                    </span>
                  </div>
                </div>
              )}
            </div>
          ))}
        </div>
      )}

      <RoomManagerDialog isOpen={showRoomManager} onClose={() => setShowRoomManager(false)} />
    </div>
  );
}
