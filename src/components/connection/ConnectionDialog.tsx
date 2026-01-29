import { useEffect, useRef, useState } from "react";
import { LuSettings } from "react-icons/lu";
import { useSyncplayStore } from "../../store";
import { useNotificationStore } from "../../store/notifications";
import { invoke } from "@tauri-apps/api/core";
import { PublicServer, SyncplayConfig } from "../../types/config";

interface ConnectionDialogProps {
  isOpen: boolean;
  onClose: () => void;
}

interface DetectedPlayer {
  name: string;
  path: string;
  version: string | null;
}

interface PlayerDetectionCache {
  players: DetectedPlayer[];
  updated_at: number | null;
}

interface ComboBoxProps {
  label: string;
  value: string;
  options: Array<{ label: string; value: string }>;
  onChange: (value: string) => void;
  onSelect?: (value: string) => void;
  placeholder?: string;
  helperText?: string;
}

function ComboBox({
  label,
  value,
  options,
  onChange,
  onSelect,
  placeholder,
  helperText,
}: ComboBoxProps) {
  const [isOpen, setIsOpen] = useState(false);
  const wrapperRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (wrapperRef.current && !wrapperRef.current.contains(event.target as Node)) {
        setIsOpen(false);
      }
    };

    document.addEventListener("mousedown", handleClickOutside);
    return () => {
      document.removeEventListener("mousedown", handleClickOutside);
    };
  }, []);

  const query = value.trim().toLowerCase();
  const normalizedOptions = options
    .map((option) => ({
      label: option.label.trim(),
      value: option.value.trim(),
    }))
    .filter((option) => option.label && option.value);
  const uniqueOptions = Array.from(
    new Map(normalizedOptions.map((option) => [option.value, option])).values()
  );
  const filteredOptions = uniqueOptions.filter((option) =>
    query
      ? option.label.toLowerCase().includes(query) || option.value.toLowerCase().includes(query)
      : true
  );

  const handleSelect = (option: { label: string; value: string }) => {
    if (onSelect) {
      onSelect(option.value);
    } else {
      onChange(option.value);
    }
    setIsOpen(false);
  };

  return (
    <div className="space-y-1">
      <label className="block text-sm font-medium">{label}</label>
      <div ref={wrapperRef} className="relative">
        <input
          type="text"
          value={value}
          onChange={(event) => {
            onChange(event.target.value);
            setIsOpen(true);
          }}
          onFocus={() => setIsOpen(true)}
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              setIsOpen(false);
            }
          }}
          className="w-full app-input px-3 py-2 rounded-md focus:outline-none focus:border-blue-500 pr-10"
          placeholder={placeholder}
        />
        <button
          type="button"
          onMouseDown={(event) => {
            event.preventDefault();
            setIsOpen((prev) => !prev);
          }}
          className="absolute right-2 top-1/2 -translate-y-1/2 app-input-addon rounded px-1.5 py-1 text-xs"
          aria-label="Options"
        >
          v
        </button>
        {isOpen && (
          <div className="absolute z-20 mt-2 w-full app-dropdown rounded-lg p-2">
            {filteredOptions.length === 0 ? (
              <div className="px-2 py-1.5 text-xs app-text-muted">No matches</div>
            ) : (
              <div className="max-h-48 overflow-auto space-y-1">
                {filteredOptions.map((option) => (
                  <button
                    type="button"
                    key={option.value}
                    onMouseDown={(event) => {
                      event.preventDefault();
                      handleSelect(option);
                    }}
                    className="w-full text-left px-3 py-2 text-sm app-dropdown-item"
                    aria-selected={option.value === value}
                  >
                    <div className="text-sm">{option.label}</div>
                    {option.label !== option.value && (
                      <div className="text-xs app-text-muted">{option.value}</div>
                    )}
                  </button>
                ))}
              </div>
            )}
          </div>
        )}
      </div>
      {helperText && <p className="text-xs app-text-muted">{helperText}</p>}
    </div>
  );
}

export function ConnectionDialog({ isOpen, onClose }: ConnectionDialogProps) {
  const connection = useSyncplayStore((state) => state.connection);
  const addNotification = useNotificationStore((state) => state.addNotification);
  const [config, setConfig] = useState<SyncplayConfig | null>(null);
  const [activeTab, setActiveTab] = useState<"connection" | "player">("connection");
  const [formData, setFormData] = useState({
    address: "syncplay.pl:8999",
    username: "",
    room: "default",
    password: "",
  });
  const [isConnecting, setIsConnecting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showOptions, setShowOptions] = useState(false);
  const [detectedPlayers, setDetectedPlayers] = useState<DetectedPlayer[]>([]);
  const [detectingPlayers, setDetectingPlayers] = useState(false);
  const [playersUpdatedAt, setPlayersUpdatedAt] = useState<number | null>(null);
  const [playersError, setPlayersError] = useState<string | null>(null);
  const [playerArgsInput, setPlayerArgsInput] = useState("");

  const serverOptions = buildServerOptions(config?.recent_servers ?? [], config?.public_servers);
  const roomOptions = config?.user.room_list ?? [];

  useEffect(() => {
    if (!isOpen) return;
    const loadConfig = async () => {
      try {
        const loaded = await invoke<SyncplayConfig>("get_config");
        setConfig(loaded);
        setFormData({
          address: `${loaded.server.host}:${loaded.server.port}`,
          username: loaded.user.username,
          room: loaded.user.default_room,
          password: loaded.server.password || "",
        });
        const args = loaded.player.player_arguments || [];
        setPlayerArgsInput(args.join(" "));
      } catch (err) {
        setError("Failed to load saved config");
      }
    };
    loadConfig();
  }, [isOpen]);

  useEffect(() => {
    if (!config) return;
    const args = config.player.player_arguments || [];
    setPlayerArgsInput(args.join(" "));
  }, [config?.player.player_arguments]);

  useEffect(() => {
    if (!isOpen || activeTab !== "player") return;
    void (async () => {
      await loadPlayerCache();
      await refreshPlayers();
    })();
  }, [isOpen, activeTab]);

  if (!isOpen) return null;

  const handleAddressSelect = (value: string) => {
    const entry = config?.recent_servers.find(
      (server) => `${server.host}:${server.port}` === value
    );
    setFormData((prev) => ({
      ...prev,
      address: value,
      password: entry?.password || prev.password,
    }));
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

  const buildUpdatedConfig = (
    base: SyncplayConfig,
    host: string,
    port: number
  ): SyncplayConfig => ({
    ...base,
    server: {
      ...base.server,
      host,
      port,
      password: formData.password || null,
    },
    user: {
      ...base.user,
      username: formData.username,
      default_room: formData.room,
    },
  });

  const updateConfig = async (nextConfig: SyncplayConfig) => {
    setConfig(nextConfig);
    try {
      await invoke("update_config", { config: nextConfig });
    } catch (err) {
      setError("Failed to update connection settings");
    }
  };

  const updateUserConfig = async (updates: Partial<SyncplayConfig["user"]>) => {
    if (!config) return;
    const nextConfig = {
      ...config,
      user: {
        ...config.user,
        ...updates,
      },
    };
    await updateConfig(nextConfig);
  };

  const updatePlayerArguments = (value: string) => {
    if (!config) return;
    const args = value
      .split(" ")
      .map((entry) => entry.trim())
      .filter((entry) => entry.length > 0);
    const nextConfig = {
      ...config,
      player: {
        ...config.player,
        player_arguments: args,
      },
    };
    void updateConfig(nextConfig);
  };

  const loadPlayerCache = async () => {
    try {
      const cache = await invoke<PlayerDetectionCache>("get_cached_players");
      setDetectedPlayers(cache.players);
      setPlayersUpdatedAt(cache.updated_at);
      setPlayersError(null);
    } catch (err) {
      console.error("Failed to load cached players:", err);
      setPlayersError("Failed to load cached players.");
    }
  };

  const refreshPlayers = async () => {
    if (detectingPlayers) return;
    setDetectingPlayers(true);
    setPlayersError(null);
    try {
      const cache = await invoke<PlayerDetectionCache>("refresh_player_detection");
      setDetectedPlayers(cache.players);
      setPlayersUpdatedAt(cache.updated_at);
    } catch (err) {
      console.error("Failed to refresh players:", err);
      const message =
        typeof err === "string" ? err : (err as { message?: string })?.message || "Unknown error";
      setPlayersError(`Failed to refresh players: ${message}`);
    } finally {
      setDetectingPlayers(false);
    }
  };

  const addRoomToList = (rooms: string[], room: string) => {
    const trimmed = room.trim();
    if (!trimmed) return rooms;
    const next = rooms.filter((entry) => entry !== trimmed);
    next.unshift(trimmed);
    return next;
  };

  const addServerToList = (
    servers: SyncplayConfig["recent_servers"],
    host: string,
    port: number,
    password: string | null
  ) => {
    const next = servers.filter((entry) => entry.host !== host || entry.port !== port);
    next.unshift({ host, port, password });
    return next.slice(0, 10);
  };

  function buildServerOptions(
    recent: SyncplayConfig["recent_servers"],
    publicServers?: PublicServer[]
  ) {
    const recentOptions = recent.map((server) => ({
      label: `${server.host}:${server.port}`,
      value: `${server.host}:${server.port}`,
    }));
    const publicOptions =
      publicServers?.map((server) => ({
        label: server.name,
        value: server.address,
      })) ?? [];
    return [...publicOptions, ...recentOptions];
  }

  const handleConnect = async (saveConfig: boolean) => {
    if (!formData.username.trim()) {
      setError("Username is required");
      return;
    }

    const address = parseAddress(formData.address);
    if (!address) {
      setError("Address must be in host:port format");
      return;
    }

    setIsConnecting(true);
    setError(null);

    try {
      if (saveConfig) {
        let currentConfig = config;
        if (!currentConfig) {
          currentConfig = await invoke<SyncplayConfig>("get_config");
          setConfig(currentConfig);
        }
        const updatedBase = buildUpdatedConfig(currentConfig, address.host, address.port);
        const updated = {
          ...updatedBase,
          recent_servers: addServerToList(
            updatedBase.recent_servers,
            address.host,
            address.port,
            updatedBase.server.password
          ),
          user: {
            ...updatedBase.user,
            room_list: addRoomToList(updatedBase.user.room_list, formData.room),
          },
        };
        await invoke("update_config", { config: updated });
        setConfig(updated);
      }

      await invoke("connect_to_server", {
        host: address.host,
        port: address.port,
        username: formData.username,
        room: formData.room,
        password: formData.password || null,
      });
      addNotification({
        type: "success",
        message: `Connected to ${address.host}:${address.port}`,
      });
      onClose();
    } catch (err) {
      setError(err as string);
      addNotification({
        type: "error",
        message: `Connection failed: ${err}`,
      });
    } finally {
      setIsConnecting(false);
    }
  };

  const handleDisconnect = async () => {
    try {
      await invoke("disconnect_from_server");
      addNotification({
        type: "info",
        message: "Disconnected from server",
      });
      onClose();
    } catch (err) {
      setError(err as string);
    }
  };

  return (
    <div
      className="fixed inset-0 app-overlay flex items-center justify-center z-50"
      data-tauri-drag-region="false"
    >
      <div className="app-panel app-panel-glass rounded-xl p-6 w-full max-w-2xl shadow-xl">
        <h2 className="text-xl font-bold mb-4">
          {connection.connected ? "Connected" : "Connect to Server"}
        </h2>

        {!connection.connected && (
          <div className="flex flex-wrap gap-2 mb-4 border-b app-divider">
            {[
              { id: "connection", label: "Connection" },
              { id: "player", label: "Player" },
            ].map((tab) => (
              <button
                key={tab.id}
                type="button"
                onClick={() => setActiveTab(tab.id as "connection" | "player")}
                className={`px-4 py-2 app-tab ${activeTab === tab.id ? "app-tab-active" : ""}`}
              >
                {tab.label}
              </button>
            ))}
          </div>
        )}

        {connection.connected ? (
          <div className="space-y-4">
            <div className="app-panel-muted p-4 rounded-lg">
              <p className="text-sm app-text-muted">
                Connected to: <span className="font-medium">{connection.server}</span>
              </p>
            </div>

            <div className="flex gap-2">
              <button onClick={handleDisconnect} className="flex-1 btn-danger px-4 py-2 rounded">
                Disconnect
              </button>
              <button onClick={onClose} className="flex-1 btn-neutral px-4 py-2 rounded-md">
                Close
              </button>
            </div>
          </div>
        ) : (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              handleConnect(false);
            }}
            className="space-y-4"
          >
            {activeTab === "connection" ? (
              <>
                <ComboBox
                  label="Address (host:port)"
                  value={formData.address}
                  options={serverOptions}
                  onChange={(value) => setFormData({ ...formData, address: value })}
                  onSelect={handleAddressSelect}
                  placeholder="syncplay.pl:8999"
                />

                <div>
                  <label className="block text-sm font-medium mb-1">Username *</label>
                  <input
                    type="text"
                    value={formData.username}
                    onChange={(e) => setFormData({ ...formData, username: e.target.value })}
                    className="w-full app-input px-3 py-2 rounded-md focus:outline-none focus:border-blue-500"
                    placeholder="Your username"
                    required
                  />
                </div>

                <ComboBox
                  label="Room"
                  value={formData.room}
                  options={roomOptions.map((room) => ({ label: room, value: room }))}
                  onChange={(value) => setFormData({ ...formData, room: value })}
                  placeholder="default"
                />

                <div>
                  <label className="block text-sm font-medium mb-1">Password (optional)</label>
                  <input
                    type="password"
                    value={formData.password}
                    onChange={(e) => setFormData({ ...formData, password: e.target.value })}
                    className="w-full app-input px-3 py-2 rounded-md focus:outline-none focus:border-blue-500"
                    placeholder="Server password"
                  />
                </div>

                <div className="flex items-center justify-between">
                  <span className="text-sm font-medium">Connection Options</span>
                  <button
                    type="button"
                    className="btn-neutral app-icon-button"
                    onClick={() => setShowOptions((prev) => !prev)}
                    aria-label="Toggle connection options"
                  >
                    <LuSettings className="app-icon" />
                  </button>
                </div>

                {showOptions && config && (
                  <div className="flex flex-col gap-2">
                    <label className="flex items-center gap-2 text-sm">
                      <input
                        type="checkbox"
                        checked={config.user.autosave_joins_to_list}
                        onChange={(e) =>
                          updateUserConfig({ autosave_joins_to_list: e.target.checked })
                        }
                        className="w-4 h-4"
                      />
                      Auto-save joined rooms
                    </label>
                    <label className="flex items-center gap-2 text-sm">
                      <input
                        type="checkbox"
                        checked={config.user.auto_connect}
                        onChange={(e) => updateUserConfig({ auto_connect: e.target.checked })}
                        className="w-4 h-4"
                      />
                      Auto-connect on startup
                    </label>
                    <label className="flex items-center gap-2 text-sm">
                      <input
                        type="checkbox"
                        checked={config.user.force_gui_prompt}
                        onChange={(e) => updateUserConfig({ force_gui_prompt: e.target.checked })}
                        className="w-4 h-4"
                      />
                      Always show connect dialog on startup
                    </label>
                  </div>
                )}
              </>
            ) : (
              <div className="space-y-4">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <label className="block text-sm font-medium">Media Player</label>
                  <div className="flex items-center gap-2 text-xs app-text-muted">
                    {playersUpdatedAt && (
                      <span>
                        Last checked{" "}
                        {new Date(playersUpdatedAt).toLocaleTimeString(undefined, {
                          hour: "2-digit",
                          minute: "2-digit",
                        })}
                      </span>
                    )}
                    <button
                      type="button"
                      onClick={refreshPlayers}
                      className="btn-neutral px-2 py-1 rounded-md text-xs"
                      disabled={detectingPlayers}
                    >
                      {detectingPlayers ? "Refreshing..." : "Refresh Players"}
                    </button>
                  </div>
                </div>

                {(detectingPlayers || playersError) && (
                  <p className="text-xs app-text-muted">
                    {detectingPlayers ? "Refreshing player list..." : playersError}
                  </p>
                )}

                {config && (
                  <>
                    <div>
                      {detectedPlayers.length > 0 ? (
                        <select
                          value={config.player.player_path}
                          onChange={(e) =>
                            updateConfig({
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
                        <label className="block text-sm font-medium mb-1">
                          Player Path (Manual)
                        </label>
                        <input
                          type="text"
                          value={
                            config.player.player_path === "custom" ? "" : config.player.player_path
                          }
                          onChange={(e) =>
                            updateConfig({
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
                  </>
                )}
              </div>
            )}

            {error && <div className="app-alert app-alert-danger px-4 py-2 text-sm">{error}</div>}

            <div className="flex gap-2">
              <button
                type="submit"
                disabled={isConnecting}
                className="flex-1 btn-primary disabled:opacity-60 disabled:cursor-not-allowed px-4 py-2 rounded-md"
              >
                {isConnecting ? "Connecting..." : "Connect"}
              </button>
              <button
                type="button"
                onClick={() => handleConnect(true)}
                disabled={isConnecting}
                className="flex-1 btn-secondary disabled:opacity-60 disabled:cursor-not-allowed px-4 py-2 rounded-md"
              >
                {isConnecting ? "Connecting..." : "Connect & Save"}
              </button>
              <button
                type="button"
                onClick={onClose}
                className="flex-1 btn-neutral px-4 py-2 rounded-md"
              >
                Cancel
              </button>
            </div>
          </form>
        )}
      </div>
    </div>
  );
}
