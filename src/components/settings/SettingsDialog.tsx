import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getAppliedTheme } from "../../services/theme";
import { open } from "@tauri-apps/plugin-dialog";
import {
  ChatInputPosition,
  ChatOutputMode,
  PrivacyMode,
  SyncplayConfig,
  UnpauseAction,
} from "../../types/config";

interface DetectedPlayer {
  name: string;
  path: string;
  version: string | null;
}

interface SettingsDialogProps {
  isOpen: boolean;
  onClose: () => void;
}

type SettingsTab = "connection" | "player" | "sync" | "ready" | "privacy" | "chat" | "osd" | "misc";

const privacyOptions: Array<{ label: string; value: PrivacyMode }> = [
  { label: "Send raw", value: "send_raw" },
  { label: "Send hashed", value: "send_hashed" },
  { label: "Do not send", value: "do_not_send" },
];

const unpauseOptions: Array<{ label: string; value: UnpauseAction }> = [
  { label: "If already ready", value: "if_already_ready" },
  { label: "If others ready", value: "if_others_ready" },
  { label: "If min users ready", value: "if_min_users_ready" },
  { label: "Always", value: "always" },
];

const chatInputPositions: Array<{ label: string; value: ChatInputPosition }> = [
  { label: "Top", value: "top" },
  { label: "Middle", value: "middle" },
  { label: "Bottom", value: "bottom" },
];

const chatOutputModes: Array<{ label: string; value: ChatOutputMode }> = [
  { label: "Chatroom", value: "chatroom" },
  { label: "Scrolling", value: "scrolling" },
];

export function SettingsDialog({ isOpen, onClose }: SettingsDialogProps) {
  const [config, setConfig] = useState<SyncplayConfig | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState<SettingsTab>("connection");
  const [detectedPlayers, setDetectedPlayers] = useState<DetectedPlayer[]>([]);
  const [detectingPlayers, setDetectingPlayers] = useState(false);
  const [mediaDirectoryInput, setMediaDirectoryInput] = useState("");
  const [roomListInput, setRoomListInput] = useState("");
  const [trustedDomainInput, setTrustedDomainInput] = useState("");
  const [serverAddress, setServerAddress] = useState("");
  const [serverAddressError, setServerAddressError] = useState<string | null>(null);
  const [playerArgsInput, setPlayerArgsInput] = useState("");
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

  useEffect(() => {
    if (!config) return;
    const args = config.player.player_arguments || [];
    setPlayerArgsInput(args.join(" "));
  }, [config?.player.player_arguments]);

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

  const addTrustedDomain = () => {
    if (!config) return;
    const trimmed = trustedDomainInput.trim();
    if (!trimmed) return;
    if (config.user.trusted_domains.includes(trimmed)) {
      setTrustedDomainInput("");
      return;
    }
    setConfig({
      ...config,
      user: {
        ...config.user,
        trusted_domains: [...config.user.trusted_domains, trimmed],
      },
    });
    setTrustedDomainInput("");
  };

  const removeTrustedDomain = (domain: string) => {
    if (!config) return;
    setConfig({
      ...config,
      user: {
        ...config.user,
        trusted_domains: config.user.trusted_domains.filter((entry) => entry !== domain),
      },
    });
  };

  const updatePlayerArguments = (value: string) => {
    if (!config) return;
    const args = value
      .split(" ")
      .map((entry) => entry.trim())
      .filter((entry) => entry.length > 0);
    setConfig({
      ...config,
      player: {
        ...config.player,
        player_arguments: args,
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
        await invoke("update_config", {
          config: {
            ...config,
            user: { ...config.user, theme: getAppliedTheme() },
          },
        });
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
    <div
      className="fixed inset-0 app-overlay flex items-center justify-center z-50"
      data-tauri-drag-region="false"
    >
      <div
        className="app-panel app-panel-glass rounded-xl p-6 w-full max-w-4xl max-h-[85vh] overflow-auto shadow-xl"
        data-tauri-drag-region="false"
      >
        <div className="flex flex-wrap items-center justify-between gap-4 mb-4">
          <div>
            <h2 className="text-xl font-bold">Settings</h2>
            <p className="text-xs app-text-muted">Changes are saved automatically.</p>
          </div>
          <div className="flex items-center gap-3">
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
            <div className="flex flex-wrap gap-2 mb-4 border-b app-divider">
              {[
                { id: "connection", label: "Connection" },
                { id: "player", label: "Player" },
                { id: "sync", label: "Sync" },
                { id: "ready", label: "Readiness" },
                { id: "privacy", label: "Privacy" },
                { id: "chat", label: "Chat" },
                { id: "osd", label: "OSD" },
                { id: "misc", label: "Misc" },
              ].map((tab) => (
                <button
                  key={tab.id}
                  onClick={() => setActiveTab(tab.id as SettingsTab)}
                  className={`px-4 py-2 app-tab ${activeTab === tab.id ? "app-tab-active" : ""}`}
                >
                  {tab.label}
                </button>
              ))}
            </div>

            {activeTab === "connection" && (
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
                    <p className="text-xs app-text-danger mt-1">{serverAddressError}</p>
                  ) : (
                    <p className="text-xs app-text-muted mt-1">Format: host:port</p>
                  )}
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
                      className="btn-primary px-3 py-2 rounded text-sm"
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
                            className="text-xs app-text-danger hover:opacity-80"
                          >
                            Remove
                          </button>
                        </div>
                      ))}
                    </div>
                  )}
                </div>

                <div className="flex flex-col gap-2">
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.autosave_joins_to_list}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, autosave_joins_to_list: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Auto-save joined rooms
                  </label>
                  <label className="flex items-center gap-2 text-sm">
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
                    Auto-connect on startup
                  </label>
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.force_gui_prompt}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, force_gui_prompt: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Always show connect dialog on startup
                  </label>
                </div>
              </div>
            )}

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
                  <label className="block text-sm font-medium mb-1">Player Arguments</label>
                  <input
                    type="text"
                    value={playerArgsInput}
                    onChange={(e) => {
                      setPlayerArgsInput(e.target.value);
                      updatePlayerArguments(e.target.value);
                    }}
                    className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                    placeholder="--fullscreen --no-border"
                  />
                  <p className="text-xs app-text-muted mt-1">
                    Arguments applied when launching the player
                  </p>
                </div>

                <div>
                  <label className="block text-sm font-medium mb-1">MPV Socket Path</label>
                  <input
                    type="text"
                    value={config.player.mpv_socket_path}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        player: { ...config.player, mpv_socket_path: e.target.value },
                      })
                    }
                    className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                  />
                </div>

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
                      className="btn-primary px-3 py-2 rounded text-sm"
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
                            className="text-xs app-text-danger hover:opacity-80"
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

                <div className="flex flex-col gap-2">
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.shared_playlist_enabled}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, shared_playlist_enabled: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Enable shared playlists
                  </label>
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.loop_at_end_of_playlist}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, loop_at_end_of_playlist: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Loop at end of playlist
                  </label>
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.loop_single_files}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, loop_single_files: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Loop single files
                  </label>
                </div>
              </div>
            )}

            {activeTab === "sync" && (
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
                  <label className="block text-sm font-medium mb-1">
                    Slowdown Reset Threshold (seconds)
                  </label>
                  <input
                    type="number"
                    step="0.1"
                    value={config.user.slowdown_reset_threshold}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: {
                          ...config.user,
                          slowdown_reset_threshold: parseFloat(e.target.value),
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

                <div className="flex flex-col gap-2">
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.slow_on_desync}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, slow_on_desync: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Slow down on desync
                  </label>
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.rewind_on_desync}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, rewind_on_desync: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Rewind on desync
                  </label>
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.fastforward_on_desync}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, fastforward_on_desync: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Fast-forward on desync
                  </label>
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.dont_slow_down_with_me}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, dont_slow_down_with_me: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Do not slow down with me
                  </label>
                </div>
              </div>
            )}

            {activeTab === "ready" && (
              <div className="space-y-4">
                <div className="flex flex-col gap-2">
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.ready_at_start}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, ready_at_start: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Ready at startup
                  </label>
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.pause_on_leave}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, pause_on_leave: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Pause when someone leaves the room
                  </label>
                </div>

                <div>
                  <label className="block text-sm font-medium mb-1">Unpause behavior</label>
                  <select
                    value={config.user.unpause_action}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: { ...config.user, unpause_action: e.target.value as UnpauseAction },
                      })
                    }
                    className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                  >
                    {unpauseOptions.map((option) => (
                      <option key={option.value} value={option.value}>
                        {option.label}
                      </option>
                    ))}
                  </select>
                </div>

                <div className="flex flex-col gap-2">
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.autoplay_enabled}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, autoplay_enabled: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Enable auto-play when all ready
                  </label>
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.autoplay_require_same_filenames}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: {
                            ...config.user,
                            autoplay_require_same_filenames: e.target.checked,
                          },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Require same filenames for auto-play
                  </label>
                </div>

                <div>
                  <label className="block text-sm font-medium mb-1">Auto-play minimum users</label>
                  <input
                    type="number"
                    value={config.user.autoplay_min_users}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: {
                          ...config.user,
                          autoplay_min_users: parseInt(e.target.value, 10),
                        },
                      })
                    }
                    className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                  />
                  <p className="text-xs app-text-muted mt-1">Use -1 to disable minimum.</p>
                </div>
              </div>
            )}

            {activeTab === "privacy" && (
              <div className="space-y-4">
                <div>
                  <label className="block text-sm font-medium mb-1">Filename privacy</label>
                  <select
                    value={config.user.filename_privacy_mode}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: {
                          ...config.user,
                          filename_privacy_mode: e.target.value as PrivacyMode,
                        },
                      })
                    }
                    className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                  >
                    {privacyOptions.map((option) => (
                      <option key={option.value} value={option.value}>
                        {option.label}
                      </option>
                    ))}
                  </select>
                </div>

                <div>
                  <label className="block text-sm font-medium mb-1">Filesize privacy</label>
                  <select
                    value={config.user.filesize_privacy_mode}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: {
                          ...config.user,
                          filesize_privacy_mode: e.target.value as PrivacyMode,
                        },
                      })
                    }
                    className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                  >
                    {privacyOptions.map((option) => (
                      <option key={option.value} value={option.value}>
                        {option.label}
                      </option>
                    ))}
                  </select>
                </div>

                <label className="flex items-center gap-2 text-sm">
                  <input
                    type="checkbox"
                    checked={config.user.only_switch_to_trusted_domains}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: {
                          ...config.user,
                          only_switch_to_trusted_domains: e.target.checked,
                        },
                      })
                    }
                    className="w-4 h-4"
                  />
                  Only switch to trusted domains
                </label>

                <div>
                  <label className="block text-sm font-medium mb-1">Trusted domains</label>
                  <div className="flex gap-2">
                    <input
                      type="text"
                      value={trustedDomainInput}
                      onChange={(e) => setTrustedDomainInput(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") {
                          e.preventDefault();
                          addTrustedDomain();
                        }
                      }}
                      className="flex-1 app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                      placeholder="youtube.com"
                    />
                    <button
                      type="button"
                      onClick={addTrustedDomain}
                      className="btn-primary px-3 py-2 rounded text-sm"
                    >
                      Add
                    </button>
                  </div>
                  {config.user.trusted_domains.length === 0 ? (
                    <p className="text-xs app-text-muted mt-2">No trusted domains added.</p>
                  ) : (
                    <div className="mt-2 space-y-2">
                      {config.user.trusted_domains.map((domain) => (
                        <div
                          key={domain}
                          className="flex items-center justify-between app-panel-muted px-3 py-2 rounded"
                        >
                          <span className="text-sm truncate">{domain}</span>
                          <button
                            type="button"
                            onClick={() => removeTrustedDomain(domain)}
                            className="text-xs app-text-danger hover:opacity-80"
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

            {activeTab === "chat" && (
              <div className="space-y-4">
                <div className="flex flex-col gap-2">
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.chat_input_enabled}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, chat_input_enabled: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Enable chat input
                  </label>
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.chat_direct_input}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, chat_direct_input: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Direct input mode
                  </label>
                </div>

                <div>
                  <label className="block text-sm font-medium mb-1">Chat input position</label>
                  <select
                    value={config.user.chat_input_position}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: {
                          ...config.user,
                          chat_input_position: e.target.value as ChatInputPosition,
                        },
                      })
                    }
                    className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                  >
                    {chatInputPositions.map((option) => (
                      <option key={option.value} value={option.value}>
                        {option.label}
                      </option>
                    ))}
                  </select>
                </div>

                <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                  <div>
                    <label className="block text-sm font-medium mb-1">Input font family</label>
                    <input
                      type="text"
                      value={config.user.chat_input_font_family}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, chat_input_font_family: e.target.value },
                        })
                      }
                      className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                    />
                  </div>
                  <div>
                    <label className="block text-sm font-medium mb-1">Input font size</label>
                    <input
                      type="number"
                      value={config.user.chat_input_relative_font_size}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: {
                            ...config.user,
                            chat_input_relative_font_size: parseInt(e.target.value, 10),
                          },
                        })
                      }
                      className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                    />
                  </div>
                  <div>
                    <label className="block text-sm font-medium mb-1">Input font weight</label>
                    <input
                      type="number"
                      value={config.user.chat_input_font_weight}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: {
                            ...config.user,
                            chat_input_font_weight: parseInt(e.target.value, 10),
                          },
                        })
                      }
                      className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                    />
                  </div>
                  <div>
                    <label className="block text-sm font-medium mb-1">Input font color</label>
                    <input
                      type="text"
                      value={config.user.chat_input_font_color}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, chat_input_font_color: e.target.value },
                        })
                      }
                      className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                    />
                  </div>
                </div>

                <label className="flex items-center gap-2 text-sm">
                  <input
                    type="checkbox"
                    checked={config.user.chat_input_font_underline}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: { ...config.user, chat_input_font_underline: e.target.checked },
                      })
                    }
                    className="w-4 h-4"
                  />
                  Underline chat input
                </label>

                <div className="flex flex-col gap-2">
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.chat_output_enabled}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, chat_output_enabled: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Enable chat output
                  </label>
                </div>

                <div>
                  <label className="block text-sm font-medium mb-1">Chat output mode</label>
                  <select
                    value={config.user.chat_output_mode}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: {
                          ...config.user,
                          chat_output_mode: e.target.value as ChatOutputMode,
                        },
                      })
                    }
                    className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                  >
                    {chatOutputModes.map((option) => (
                      <option key={option.value} value={option.value}>
                        {option.label}
                      </option>
                    ))}
                  </select>
                </div>

                <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                  <div>
                    <label className="block text-sm font-medium mb-1">Output font family</label>
                    <input
                      type="text"
                      value={config.user.chat_output_font_family}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, chat_output_font_family: e.target.value },
                        })
                      }
                      className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                    />
                  </div>
                  <div>
                    <label className="block text-sm font-medium mb-1">Output font size</label>
                    <input
                      type="number"
                      value={config.user.chat_output_relative_font_size}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: {
                            ...config.user,
                            chat_output_relative_font_size: parseInt(e.target.value, 10),
                          },
                        })
                      }
                      className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                    />
                  </div>
                  <div>
                    <label className="block text-sm font-medium mb-1">Output font weight</label>
                    <input
                      type="number"
                      value={config.user.chat_output_font_weight}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: {
                            ...config.user,
                            chat_output_font_weight: parseInt(e.target.value, 10),
                          },
                        })
                      }
                      className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                    />
                  </div>
                  <div>
                    <label className="block text-sm font-medium mb-1">Chat max lines</label>
                    <input
                      type="number"
                      value={config.user.chat_max_lines}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, chat_max_lines: parseInt(e.target.value, 10) },
                        })
                      }
                      className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                    />
                  </div>
                </div>

                <label className="flex items-center gap-2 text-sm">
                  <input
                    type="checkbox"
                    checked={config.user.chat_output_font_underline}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: { ...config.user, chat_output_font_underline: e.target.checked },
                      })
                    }
                    className="w-4 h-4"
                  />
                  Underline chat output
                </label>

                <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
                  <div>
                    <label className="block text-sm font-medium mb-1">Top margin</label>
                    <input
                      type="number"
                      value={config.user.chat_top_margin}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, chat_top_margin: parseInt(e.target.value, 10) },
                        })
                      }
                      className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                    />
                  </div>
                  <div>
                    <label className="block text-sm font-medium mb-1">Left margin</label>
                    <input
                      type="number"
                      value={config.user.chat_left_margin}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, chat_left_margin: parseInt(e.target.value, 10) },
                        })
                      }
                      className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                    />
                  </div>
                  <div>
                    <label className="block text-sm font-medium mb-1">Bottom margin</label>
                    <input
                      type="number"
                      value={config.user.chat_bottom_margin}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: {
                            ...config.user,
                            chat_bottom_margin: parseInt(e.target.value, 10),
                          },
                        })
                      }
                      className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                    />
                  </div>
                </div>

                <div className="flex flex-col gap-2">
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.chat_move_osd}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, chat_move_osd: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Move OSD for chat
                  </label>
                </div>

                <div>
                  <label className="block text-sm font-medium mb-1">OSD margin</label>
                  <input
                    type="number"
                    value={config.user.chat_osd_margin}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: { ...config.user, chat_osd_margin: parseInt(e.target.value, 10) },
                      })
                    }
                    className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                  />
                </div>

                <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
                  <div>
                    <label className="block text-sm font-medium mb-1">Notification timeout</label>
                    <input
                      type="number"
                      value={config.user.notification_timeout}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: {
                            ...config.user,
                            notification_timeout: parseInt(e.target.value, 10),
                          },
                        })
                      }
                      className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                    />
                  </div>
                  <div>
                    <label className="block text-sm font-medium mb-1">Alert timeout</label>
                    <input
                      type="number"
                      value={config.user.alert_timeout}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, alert_timeout: parseInt(e.target.value, 10) },
                        })
                      }
                      className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                    />
                  </div>
                  <div>
                    <label className="block text-sm font-medium mb-1">Chat timeout</label>
                    <input
                      type="number"
                      value={config.user.chat_timeout}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, chat_timeout: parseInt(e.target.value, 10) },
                        })
                      }
                      className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                    />
                  </div>
                </div>
              </div>
            )}

            {activeTab === "osd" && (
              <div className="space-y-4">
                <div>
                  <label className="block text-sm font-medium mb-1">OSD Duration (ms)</label>
                  <input
                    type="number"
                    value={config.user.osd_duration}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: { ...config.user, osd_duration: parseInt(e.target.value, 10) },
                      })
                    }
                    className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                  />
                </div>

                <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
                  <label className="flex items-center gap-2 text-sm">
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
                    Show OSD
                  </label>
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.show_osd_warnings}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, show_osd_warnings: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Show OSD warnings
                  </label>
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.show_slowdown_osd}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, show_slowdown_osd: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Show slowdown OSD
                  </label>
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.show_same_room_osd}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, show_same_room_osd: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Show same room OSD
                  </label>
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.show_different_room_osd}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, show_different_room_osd: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Show different room OSD
                  </label>
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.show_non_controller_osd}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, show_non_controller_osd: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Show non-controller OSD
                  </label>
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.user.show_duration_notification}
                      onChange={(e) =>
                        setConfig({
                          ...config,
                          user: { ...config.user, show_duration_notification: e.target.checked },
                        })
                      }
                      className="w-4 h-4"
                    />
                    Show duration notification
                  </label>
                </div>
              </div>
            )}

            {activeTab === "misc" && (
              <div className="space-y-4">
                <label className="flex items-center gap-2 text-sm">
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
                  Show playlist by default
                </label>

                <label className="flex items-center gap-2 text-sm">
                  <input
                    type="checkbox"
                    checked={config.user.reduce_transparency}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: { ...config.user, reduce_transparency: e.target.checked },
                      })
                    }
                    className="w-4 h-4"
                  />
                  Reduce transparency
                </label>

                <label className="flex items-center gap-2 text-sm">
                  <input
                    type="checkbox"
                    checked={config.user.show_contact_info}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: { ...config.user, show_contact_info: e.target.checked },
                      })
                    }
                    className="w-4 h-4"
                  />
                  Show contact info
                </label>

                <label className="flex items-center gap-2 text-sm">
                  <input
                    type="checkbox"
                    checked={config.user.debug}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        user: { ...config.user, debug: e.target.checked },
                      })
                    }
                    className="w-4 h-4"
                  />
                  Enable debug logging
                </label>

                <div>
                  <label className="block text-sm font-medium mb-1">
                    Check for updates automatically
                  </label>
                  <select
                    value={
                      config.user.check_for_updates_automatically === null
                        ? "auto"
                        : config.user.check_for_updates_automatically
                          ? "enabled"
                          : "disabled"
                    }
                    onChange={(e) => {
                      const value = e.target.value;
                      const resolved = value === "auto" ? null : value === "enabled" ? true : false;
                      setConfig({
                        ...config,
                        user: { ...config.user, check_for_updates_automatically: resolved },
                      });
                    }}
                    className="w-full app-input px-3 py-2 rounded focus:outline-none focus:border-blue-500"
                  >
                    <option value="auto">Use default</option>
                    <option value="enabled">Enabled</option>
                    <option value="disabled">Disabled</option>
                  </select>
                </div>
              </div>
            )}

            {error && (
              <div className="mt-4 app-alert app-alert-danger px-4 py-2 text-sm">{error}</div>
            )}
          </>
        ) : (
          <div className="text-center py-8">
            <p className="app-text-danger">Failed to load settings</p>
          </div>
        )}
      </div>
    </div>
  );
}
