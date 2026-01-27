import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { applyTheme } from "../../services/theme";
import { open } from "@tauri-apps/plugin-dialog";

interface ServerConfig {
  host: string;
  port: number;
  password: string | null;
}

interface UserPreferences {
  username: string;
  default_room: string;
  room_list: string[];
  theme: string;
  seek_threshold_rewind: number;
  seek_threshold_fastforward: number;
  slowdown_threshold: number;
  slowdown_reset_threshold: number;
  slowdown_rate: number;
  show_osd: boolean;
  osd_duration: number;
  show_playlist: boolean;
  auto_connect: boolean;
}

interface PlayerConfig {
  player_path: string;
  mpv_socket_path: string;
  media_directories: string[];
}

interface DetectedPlayer {
  name: string;
  path: string;
  version: string | null;
}

interface SyncplayConfig {
  server: ServerConfig;
  user: UserPreferences;
  player: PlayerConfig;
  recent_servers: ServerConfig[];
}

interface SettingsDialogProps {
  isOpen: boolean;
  onClose: () => void;
}

export function SettingsDialog({ isOpen, onClose }: SettingsDialogProps) {
  const [config, setConfig] = useState<SyncplayConfig | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState<"server" | "user" | "player" | "advanced">("server");
  const [detectedPlayers, setDetectedPlayers] = useState<DetectedPlayer[]>([]);
  const [detectingPlayers, setDetectingPlayers] = useState(false);
  const [mediaDirectoryInput, setMediaDirectoryInput] = useState("");
  const [roomListInput, setRoomListInput] = useState("");
  const [serverAddress, setServerAddress] = useState("");
  const [serverAddressError, setServerAddressError] = useState<string | null>(null);
  const saveTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const skipAutoSaveRef = useRef(true);

  useEffect(() => {
    if (!isOpen) {
      if (saveTimeoutRef.current) {
        clearTimeout(saveTimeoutRef.current);
        saveTimeoutRef.current = null;
      }
      skipAutoSaveRef.current = true;
      return;
    }
    loadConfig();
    detectPlayers();
  }, [isOpen]);

  const loadConfig = async () => {
    setLoading(true);
    setError(null);
    try {
      const loadedConfig = await invoke<SyncplayConfig>("get_config");
      setConfig(loadedConfig);
      setServerAddress(`${loadedConfig.server.host}:${loadedConfig.server.port}`);
      setServerAddressError(null);
      skipAutoSaveRef.current = true;
    } catch (err) {
      setError(err as string);
    } finally {
      setLoading(false);
    }
  };

  const detectPlayers = async () => {
    setDetectingPlayers(true);
    try {
      const players = await invoke<DetectedPlayer[]>("detect_available_players");
      setDetectedPlayers(players);
    } catch (err) {
      console.error("Failed to detect players:", err);
    } finally {
      setDetectingPlayers(false);
    }
  };

  const parseAddress = (address: string): { host: string; port: number } | null => {
    const trimmed = address.trim();
    if (!trimmed) {
      return null;
    }
    const lastColon = trimmed.lastIndexOf(":");
    if (lastColon <= 0 || lastColon === trimmed.length - 1) {
      return null;
    }
    const host = trimmed.slice(0, lastColon).trim();
    const portValue = trimmed.slice(lastColon + 1).trim();
    const port = Number.parseInt(portValue, 10);
    if (!host || Number.isNaN(port) || port <= 0 || port > 65535) {
      return null;
    }
    return { host, port };
  };

  const handleAddressChange = (value: string) => {
    setServerAddress(value);
    const parsed = parseAddress(value);
    if (!parsed) {
      setServerAddressError(value.trim() ? "Address must be in host:port format" : null);
      return;
    }
    setServerAddressError(null);
    setConfig((prev) =>
      prev
        ? {
            ...prev,
            server: {
              ...prev.server,
              host: parsed.host,
              port: parsed.port,
            },
          }
        : prev
    );
  };

  const addMediaDirectoryValue = (value: string) => {
    if (!config) return;
    const trimmed = value.trim();
    if (!trimmed) return;
    if (config.player.media_directories.includes(trimmed)) {
      setMediaDirectoryInput("");
      return;
    }
    setConfig({
      ...config,
      player: {
        ...config.player,
        media_directories: [...config.player.media_directories, trimmed],
      },
    });
    setMediaDirectoryInput("");
  };

  const addMediaDirectory = () => {
    addMediaDirectoryValue(mediaDirectoryInput);
  };

  const addMediaDirectoryFromPicker = async () => {
    if (!config) return;
    setError(null);
    let selected: string | string[] | null = null;
    try {
      const fallbackPath =
        config.player.media_directories[config.player.media_directories.length - 1];
      selected = await open({
        directory: true,
        multiple: false,
        defaultPath: fallbackPath,
      });
    } catch (err) {
      setError("Failed to open directory picker");
      return;
    }

    if (!selected || Array.isArray(selected)) {
      return;
    }

    addMediaDirectoryValue(selected);
  };

  const addRoomEntry = () => {
    if (!config) return;
    const trimmed = roomListInput.trim();
    if (!trimmed) return;
    if (config.user.room_list.includes(trimmed)) {
      setRoomListInput("");
      return;
    }
    setConfig({
      ...config,
      user: {
        ...config.user,
        room_list: [...config.user.room_list, trimmed],
      },
    });
    setRoomListInput("");
  };

  const removeRoomEntry = (room: string) => {
    if (!config) return;
    setConfig({
      ...config,
      user: {
        ...config.user,
        room_list: config.user.room_list.filter((entry) => entry !== room),
      },
    });
  };

  const removeMediaDirectory = (dir: string) => {
    if (!config) return;
    setConfig({
      ...config,
      player: {
        ...config.player,
        media_directories: config.player.media_directories.filter((entry) => entry !== dir),
      },
    });
  };

  useEffect(() => {
    if (!isOpen || !config) return;
    if (skipAutoSaveRef.current) {
      skipAutoSaveRef.current = false;
      return;
    }

    if (saveTimeoutRef.current) {
      clearTimeout(saveTimeoutRef.current);
    }

    saveTimeoutRef.current = setTimeout(async () => {
      try {
        await invoke("update_config", { config });
        applyTheme(config.user.theme);
        setError(null);
      } catch (err) {
        setError(err as string);
      }
    }, 500);

    return () => {
      if (saveTimeoutRef.current) {
        clearTimeout(saveTimeoutRef.current);
        saveTimeoutRef.current = null;
      }
    };
  }, [config, isOpen]);

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 app-overlay flex items-center justify-center z-50">
      <div className="app-panel rounded-xl p-6 w-full max-w-2xl max-h-[80vh] overflow-auto shadow-xl">
        <div className="flex flex-wrap items-center justify-between gap-4 mb-4">
          <div>
            <h2 className="text-xl font-bold">Settings</h2>
            <p className="text-xs app-text-muted">Changes are saved automatically.</p>
          </div>
          <div className="flex items-center gap-3">
            {config && (
              <>
                <label className="text-sm app-text-muted" htmlFor="theme-select">
                  Theme
                </label>
                <select
                  id="theme-select"
                  value={config.user.theme}
                  onChange={(e) => {
                    const theme = e.target.value;
                    applyTheme(theme);
                    setConfig({
                      ...config,
                      user: { ...config.user, theme },
                    });
                  }}
                  className="app-input px-3 py-2 rounded text-sm focus:outline-none focus:border-blue-500"
                >
                  <option value="dark">Dark</option>
                  <option value="light">Light</option>
                </select>
              </>
            )}
            <button onClick={onClose} className="btn-neutral px-3 py-2 rounded-md text-sm">
              Close
            </button>
          </div>
        </div>

        {loading && !config ? (
          <div className="text-center py-8">
            <p className="app-text-muted">Loading settings...</p>
          </div>
        ) : config ? (
          <>
            {/* Tabs */}
            <div className="flex gap-2 mb-4 border-b app-divider">
              <button
                onClick={() => setActiveTab("server")}
                className={`px-4 py-2 ${
                  activeTab === "server"
                    ? "border-b-2 border-blue-500 text-blue-500"
                    : "app-text-muted"
                }`}
              >
                Server
              </button>
              <button
                onClick={() => setActiveTab("user")}
                className={`px-4 py-2 ${
                  activeTab === "user"
                    ? "border-b-2 border-blue-500 text-blue-500"
                    : "app-text-muted"
                }`}
              >
                User
              </button>
              <button
                onClick={() => setActiveTab("player")}
                className={`px-4 py-2 ${
                  activeTab === "player"
                    ? "border-b-2 border-blue-500 text-blue-500"
                    : "app-text-muted"
                }`}
              >
                Player
              </button>
              <button
                onClick={() => setActiveTab("advanced")}
                className={`px-4 py-2 ${
                  activeTab === "advanced"
                    ? "border-b-2 border-blue-500 text-blue-500"
                    : "app-text-muted"
                }`}
              >
                Advanced
              </button>
            </div>

            {/* Server Tab */}
            {activeTab === "server" && (
              <div className="space-y-4">
                <div>
                  <label className="block text-sm font-medium mb-1">Server Address</label>
                  <input
                    type="text"
                    value={serverAddress}
                    onChange={(e) => handleAddressChange(e.target.value)}
                    className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                    placeholder="syncplay.pl:8999"
                  />
                  {serverAddressError ? (
                    <p className="text-xs text-red-600 mt-1">{serverAddressError}</p>
                  ) : (
                    <p className="text-xs app-text-muted mt-1">Format: host:port</p>
                  )}
                </div>

                <div>
                  <label className="block text-sm font-medium mb-1">Default Room</label>
                  <input
                    type="text"
                    value={config.user.default_room}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: { ...config.user, default_room: e.target.value },
                      })
                    }
                    className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                  />
                </div>

                <div>
                  <label className="block text-sm font-medium mb-1">Password (optional)</label>
                  <input
                    type="password"
                    value={config.server.password || ""}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        server: {
                          ...config.server,
                          password: e.target.value || null,
                        },
                      })
                    }
                    className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                  />
                </div>

                <div>
                  <label className="block text-sm font-medium mb-1">Room List</label>
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
                      className="bg-blue-600 hover:bg-blue-700 text-white px-3 py-2 rounded text-sm"
                    >
                      Add
                    </button>
                  </div>
                  {config.user.room_list.length === 0 ? (
                    <p className="text-xs app-text-muted mt-2">No rooms saved.</p>
                  ) : (
                    <div className="mt-2 space-y-2">
                      {config.user.room_list.map((room) => (
                        <div
                          key={room}
                          className="flex items-center justify-between app-panel-muted px-3 py-2 rounded"
                        >
                          <span className="text-sm truncate">{room}</span>
                          <button
                            type="button"
                            onClick={() => removeRoomEntry(room)}
                            className="text-xs text-red-600 hover:text-red-500"
                          >
                            Remove
                          </button>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              </div>
            )}

            {/* User Tab */}
            {activeTab === "user" && (
              <div className="space-y-4">
                <div>
                  <label className="block text-sm font-medium mb-1">Username</label>
                  <input
                    type="text"
                    value={config.user.username}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: { ...config.user, username: e.target.value },
                      })
                    }
                    className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                  />
                </div>

                <div className="flex items-center gap-2">
                  <input
                    type="checkbox"
                    checked={config.user.show_osd}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: { ...config.user, show_osd: e.target.checked },
                      })
                    }
                    className="w-4 h-4"
                  />
                  <label className="text-sm">Show OSD messages</label>
                </div>

                <div className="flex items-center gap-2">
                  <input
                    type="checkbox"
                    checked={config.user.show_playlist}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: { ...config.user, show_playlist: e.target.checked },
                      })
                    }
                    className="w-4 h-4"
                  />
                  <label className="text-sm">Show playlist by default</label>
                </div>

                <div className="flex items-center gap-2">
                  <input
                    type="checkbox"
                    checked={config.user.auto_connect}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: { ...config.user, auto_connect: e.target.checked },
                      })
                    }
                    className="w-4 h-4"
                  />
                  <label className="text-sm">Auto-connect on startup</label>
                </div>
              </div>
            )}

            {/* Player Tab */}
            {activeTab === "player" && (
              <div className="space-y-4">
                <div>
                  <label className="block text-sm font-medium mb-1">Media Player</label>
                  {detectingPlayers ? (
                    <p className="text-sm app-text-muted">Detecting players...</p>
                  ) : detectedPlayers.length > 0 ? (
                    <select
                      value={config.player.player_path}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          player: { ...config.player, player_path: e.target.value },
                        })
                      }
                      className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                    >
                      <option value="">Select a player...</option>
                      {detectedPlayers.map((player, index) => (
                        <option key={index} value={player.path}>
                          {player.name} {player.version ? `(${player.version})` : ""} -{" "}
                          {player.path}
                        </option>
                      ))}
                      <option value="custom">Custom path...</option>
                    </select>
                  ) : (
                    <p className="text-sm app-text-muted mb-2">
                      No players detected. Enter path manually.
                    </p>
                  )}
                </div>

                {(config.player.player_path === "custom" ||
                  detectedPlayers.length === 0 ||
                  !detectedPlayers.some((p) => p.path === config.player.player_path)) && (
                  <div>
                    <label className="block text-sm font-medium mb-1">Player Path (Manual)</label>
                    <input
                      type="text"
                      value={
                        config.player.player_path === "custom" ? "" : config.player.player_path
                      }
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          player: { ...config.player, player_path: e.target.value },
                        })
                      }
                      className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                      placeholder="/usr/local/bin/mpv"
                    />
                    <p className="text-xs app-text-muted mt-1">
                      Full path to media player executable
                    </p>
                  </div>
                )}

                <div>
                  <label className="block text-sm font-medium mb-1">Media Directories</label>
                  <div className="flex gap-2">
                    <input
                      type="text"
                      value={mediaDirectoryInput}
                      onChange={(e) => setMediaDirectoryInput(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") {
                          e.preventDefault();
                          addMediaDirectory();
                        }
                      }}
                      className="flex-1 app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                      placeholder="/path/to/media"
                    />
                    <button
                      type="button"
                      onClick={addMediaDirectory}
                      className="bg-blue-600 hover:bg-blue-700 text-white px-3 py-2 rounded text-sm"
                    >
                      Add
                    </button>
                    <button
                      type="button"
                      onClick={addMediaDirectoryFromPicker}
                      className="btn-neutral px-3 py-2 rounded text-sm"
                    >
                      Browse
                    </button>
                  </div>
                  {config.player.media_directories.length === 0 ? (
                    <p className="text-xs app-text-muted mt-2">No media directories added.</p>
                  ) : (
                    <div className="mt-2 space-y-2">
                      {config.player.media_directories.map((dir) => (
                        <div
                          key={dir}
                          className="flex items-center justify-between app-panel-muted px-3 py-2 rounded"
                        >
                          <span className="text-sm truncate">{dir}</span>
                          <button
                            type="button"
                            onClick={() => removeMediaDirectory(dir)}
                            className="text-xs text-red-600 hover:text-red-500"
                          >
                            Remove
                          </button>
                        </div>
                      ))}
                    </div>
                  )}
                  <p className="text-xs app-text-muted mt-2">
                    Files are matched locally against these directories.
                  </p>
                </div>
              </div>
            )}

            {/* Advanced Tab */}
            {activeTab === "advanced" && (
              <div className="space-y-4">
                <div>
                  <label className="block text-sm font-medium mb-1">
                    Seek Threshold Rewind (seconds)
                  </label>
                  <input
                    type="number"
                    step="0.1"
                    value={config.user.seek_threshold_rewind}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: {
                          ...config.user,
                          seek_threshold_rewind: parseFloat(e.target.value),
                        },
                      })
                    }
                    className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                  />
                </div>

                <div>
                  <label className="block text-sm font-medium mb-1">
                    Seek Threshold Fastforward (seconds)
                  </label>
                  <input
                    type="number"
                    step="0.1"
                    value={config.user.seek_threshold_fastforward}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: {
                          ...config.user,
                          seek_threshold_fastforward: parseFloat(e.target.value),
                        },
                      })
                    }
                    className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                  />
                </div>

                <div>
                  <label className="block text-sm font-medium mb-1">
                    Slowdown Threshold (seconds)
                  </label>
                  <input
                    type="number"
                    step="0.1"
                    value={config.user.slowdown_threshold}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: {
                          ...config.user,
                          slowdown_threshold: parseFloat(e.target.value),
                        },
                      })
                    }
                    className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                  />
                </div>

                <div>
                  <label className="block text-sm font-medium mb-1">Slowdown Rate (0-1)</label>
                  <input
                    type="number"
                    step="0.01"
                    min="0"
                    max="1"
                    value={config.user.slowdown_rate}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: {
                          ...config.user,
                          slowdown_rate: parseFloat(e.target.value),
                        },
                      })
                    }
                    className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                  />
                </div>

                <div>
                  <label className="block text-sm font-medium mb-1">
                    OSD Duration (milliseconds)
                  </label>
                  <input
                    type="number"
                    value={config.user.osd_duration}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: {
                          ...config.user,
                          osd_duration: parseInt(e.target.value),
                        },
                      })
                    }
                    className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                  />
                </div>
              </div>
            )}

            {error && (
              <div className="mt-4 bg-red-500/10 border border-red-500/50 text-red-600 px-4 py-2 rounded-md text-sm">
                {error}
              </div>
            )}
          </>
        ) : (
          <div className="text-center py-8">
            <p className="text-red-600">Failed to load settings</p>
          </div>
        )}
      </div>
    </div>
  );
}
