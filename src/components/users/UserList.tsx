import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { LuCheck, LuCircle, LuPencilLine, LuUsers } from "react-icons/lu";
import { useSyncplayStore } from "../../store";
import { useNotificationStore } from "../../store/notifications";
import { RoomManagerDialog } from "./RoomManagerDialog";

export function UserList() {
  const users = useSyncplayStore((state) => state.users);
  const connection = useSyncplayStore((state) => state.connection);
  const config = useSyncplayStore((state) => state.config);
  const player = useSyncplayStore((state) => state.player);
  const addNotification = useNotificationStore((state) => state.addNotification);
  const [showRoomManager, setShowRoomManager] = useState(false);
  const lastPausedRef = useRef<boolean | null>(null);

  const currentUser = users.find((user) => user.username === config?.user.username);
  const isReady = currentUser?.isReady ?? false;

  const currentRoom = currentUser?.room ?? config?.user.default_room ?? "Room";

  useEffect(() => {
    if (!connection.connected) {
      lastPausedRef.current = player.paused;
      return;
    }
    const lastPaused = lastPausedRef.current;
    if (player.paused && lastPaused === false && isReady) {
      void invoke("set_ready", { isReady: false }).catch((error) => {
        const message =
          typeof error === "string"
            ? error
            : (error as { message?: string })?.message || "Unknown error";
        addNotification({
          type: "error",
          message: `Failed to update ready state: ${message}`,
        });
      });
    }
    lastPausedRef.current = player.paused;
  }, [player.paused, isReady, connection.connected, addNotification]);

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
      <div className="flex flex-col h-full gap-2">
        <div className="flex items-center justify-between gap-2">
          <div className="flex items-center gap-2">
            <LuUsers className="app-icon app-text-muted" />
            <span className="text-sm font-semibold">{currentRoom}</span>
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

  return (
    <div className="flex flex-col h-full gap-2">
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-2">
          <LuUsers className="app-icon app-text-muted" />
          <span className="text-sm font-semibold">{currentRoom}</span>
          <span className="text-xs app-text-muted">({users.length})</span>
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
        <div className="flex-1 overflow-auto">
          <p className="app-text-muted text-sm">No users in room</p>
        </div>
      ) : (
        <div className="space-y-2 flex-1 overflow-auto pr-1">
          {users.map((user) => (
            <div key={user.username} className="app-panel-muted rounded-md p-3 text-sm">
              <div className="flex items-center justify-between">
                <span className="font-medium">{user.username}</span>
                {user.isController && (
                  <span className="text-xs app-tag-accent px-2 py-0.5 rounded">Controller</span>
                )}
              </div>

              {user.file && (
                <div className="text-xs app-text-muted mt-1 truncate">File: {user.file}</div>
              )}

              <div className="flex items-center gap-2 mt-1">
                <span
                  className={`text-xs px-2 py-0.5 rounded ${
                    user.isReady ? "app-tag-success" : "app-tag-muted"
                  }`}
                >
                  {user.isReady ? "Ready" : "Not Ready"}
                </span>
              </div>
            </div>
          ))}
        </div>
      )}

      <RoomManagerDialog isOpen={showRoomManager} onClose={() => setShowRoomManager(false)} />
    </div>
  );
}
