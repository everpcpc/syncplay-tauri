import { useEffect, useRef, useState } from "react";
import { useSyncplayStore } from "../../store";
import { useNotificationStore } from "../../store/notifications";
import { invoke } from "@tauri-apps/api/core";
import { PublicServer, SyncplayConfig } from "../../types/config";

interface ConnectionDialogProps {
  isOpen: boolean;
  onClose: () => void;
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
          aria-label="Toggle options"
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
  const [formData, setFormData] = useState({
    address: "syncplay.pl:8999",
    username: "",
    room: "default",
    password: "",
  });
  const [isConnecting, setIsConnecting] = useState(false);
  const [error, setError] = useState<string | null>(null);

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
      } catch (err) {
        setError("Failed to load saved config");
      }
    };
    loadConfig();
  }, [isOpen]);

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
