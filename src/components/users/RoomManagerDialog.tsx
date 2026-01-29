import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { SyncplayConfig } from "../../types/config";
import { useNotificationStore } from "../../store/notifications";

interface RoomManagerDialogProps {
  isOpen: boolean;
  onClose: () => void;
}

export function RoomManagerDialog({ isOpen, onClose }: RoomManagerDialogProps) {
  const [config, setConfig] = useState<SyncplayConfig | null>(null);
  const [loading, setLoading] = useState(false);
  const [connecting, setConnecting] = useState(false);
  const [roomNameInput, setRoomNameInput] = useState("");
  const [roomListInput, setRoomListInput] = useState("");
  const addNotification = useNotificationStore((state) => state.addNotification);

  useEffect(() => {
    if (!isOpen) {
      setConfig(null);
      setRoomNameInput("");
      setRoomListInput("");
      return;
    }

    const loadConfig = async () => {
      setLoading(true);
      try {
        const loaded = await invoke<SyncplayConfig>("get_config");
        setConfig(loaded);
        setRoomNameInput(loaded.user.default_room);
      } catch (error) {
        addNotification({
          type: "error",
          message: "Failed to load room settings",
        });
      } finally {
        setLoading(false);
      }
    };

    loadConfig();
  }, [isOpen, addNotification]);

  const saveConfig = async (nextConfig: SyncplayConfig) => {
    try {
      await invoke("update_config", { config: nextConfig });
      setConfig(nextConfig);
    } catch (error) {
      addNotification({
        type: "error",
        message: "Failed to update room settings",
      });
    }
  };

  const connectToRoom = async () => {
    const trimmed = roomNameInput.trim();
    if (!trimmed) return;
    setConnecting(true);
    try {
      await invoke("change_room", { room: trimmed });
    } catch (error) {
      addNotification({
        type: "error",
        message: "Failed to change room",
      });
    } finally {
      setConnecting(false);
    }
  };

  const addRoomEntry = async () => {
    if (!config) return;
    const trimmed = roomListInput.trim();
    if (!trimmed) return;
    if (config.user.room_list.includes(trimmed)) {
      addNotification({
        type: "warning",
        message: "Room already exists",
      });
      setRoomListInput("");
      return;
    }
    const nextConfig: SyncplayConfig = {
      ...config,
      user: {
        ...config.user,
        room_list: [...config.user.room_list, trimmed],
      },
    };
    await saveConfig(nextConfig);
    setRoomListInput("");
  };

  const removeRoomEntry = async (room: string) => {
    if (!config) return;
    const nextConfig: SyncplayConfig = {
      ...config,
      user: {
        ...config.user,
        room_list: config.user.room_list.filter((entry) => entry !== room),
      },
    };
    await saveConfig(nextConfig);
  };

  const updateDefaultRoom = async (value: string) => {
    if (!config) return;
    const nextConfig: SyncplayConfig = {
      ...config,
      user: {
        ...config.user,
        default_room: value,
      },
    };
    await saveConfig(nextConfig);
  };

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 app-overlay flex items-center justify-center z-50">
      <div className="app-panel app-panel-glass rounded-xl p-6 w-full max-w-2xl max-h-[80vh] overflow-auto shadow-xl">
        <div className="flex flex-wrap items-center justify-between gap-4 mb-4">
          <div>
            <h2 className="text-xl font-bold">Rooms</h2>
            <p className="text-xs app-text-muted">Manage room connections and defaults.</p>
          </div>
          <button onClick={onClose} className="btn-neutral px-3 py-2 rounded-md text-sm">
            Close
          </button>
        </div>

        {loading && !config ? (
          <div className="text-center py-8">
            <p className="app-text-muted">Loading rooms...</p>
          </div>
        ) : config ? (
          <div className="space-y-4">
            <div>
              <label className="block text-sm font-medium mb-1">Connect to Room</label>
              <div className="flex gap-2">
                <input
                  type="text"
                  value={roomNameInput}
                  onChange={(e) => setRoomNameInput(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      void connectToRoom();
                    }
                  }}
                  className="flex-1 app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                  placeholder="Room name"
                />
                <button
                  type="button"
                  onClick={() => void connectToRoom()}
                  className="btn-primary px-3 py-2 rounded text-sm"
                  disabled={connecting}
                >
                  {connecting ? "Connecting..." : "Connect"}
                </button>
              </div>
            </div>

            <div>
              <label className="block text-sm font-medium mb-1">Saved Rooms</label>
              <div className="flex gap-2">
                <input
                  type="text"
                  value={roomListInput}
                  onChange={(e) => setRoomListInput(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      addRoomEntry();
                    }
                  }}
                  className="flex-1 app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                  placeholder="Add a room"
                />
                <button
                  type="button"
                  onClick={addRoomEntry}
                  className="btn-primary px-3 py-2 rounded text-sm"
                >
                  Add
                </button>
              </div>
              {config.user.room_list.length === 0 ? (
                <p className="text-xs app-text-muted mt-2">No rooms saved.</p>
              ) : (
                <div className="mt-2 space-y-2">
                  {config.user.room_list.map((room) => {
                    const isDefault = room === config.user.default_room;
                    return (
                      <div
                        key={room}
                        className="flex items-center justify-between app-panel-muted px-3 py-2 rounded"
                      >
                        <div className="flex items-center gap-2 min-w-0">
                          <span className="text-sm truncate">{room}</span>
                          {isDefault && (
                            <span className="text-xs app-tag-accent px-2 py-0.5 rounded">
                              Default
                            </span>
                          )}
                        </div>
                        <div className="flex items-center gap-3 shrink-0">
                          {!isDefault && (
                            <button
                              type="button"
                              onClick={() => updateDefaultRoom(room)}
                              className="text-xs app-text-muted hover:opacity-80"
                            >
                              Make default
                            </button>
                          )}
                          <button
                            type="button"
                            onClick={() => removeRoomEntry(room)}
                            className="text-xs app-text-danger hover:opacity-80"
                          >
                            Remove
                          </button>
                        </div>
                      </div>
                    );
                  })}
                </div>
              )}
            </div>
          </div>
        ) : null}
      </div>
    </div>
  );
}
