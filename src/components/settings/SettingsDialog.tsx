import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api";

interface ServerConfig {
  host: string;
  port: number;
  password: string | null;
}

interface UserPreferences {
  username: string;
  default_room: string;
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

interface SyncplayConfig {
  server: ServerConfig;
  user: UserPreferences;
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
  const [activeTab, setActiveTab] = useState<"server" | "user" | "advanced">("server");

  useEffect(() => {
    if (isOpen) {
      loadConfig();
    }
  }, [isOpen]);

  const loadConfig = async () => {
    setLoading(true);
    setError(null);
    try {
      const loadedConfig = await invoke<SyncplayConfig>("get_config");
      setConfig(loadedConfig);
    } catch (err) {
      setError(err as string);
    } finally {
      setLoading(false);
    }
  };

  const handleSave = async () => {
    if (!config) return;

    setLoading(true);
    setError(null);
    try {
      await invoke("update_config", { config });
      onClose();
    } catch (err) {
      setError(err as string);
    } finally {
      setLoading(false);
    }
  };

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 bg-black bg-opacity-50 flex items-center justify-center z-50">
      <div className="bg-gray-800 rounded-lg p-6 w-full max-w-2xl max-h-[80vh] overflow-auto">
        <h2 className="text-xl font-bold mb-4">Settings</h2>

        {loading && !config ? (
          <div className="text-center py-8">
            <p className="text-gray-400">Loading settings...</p>
          </div>
        ) : config ? (
          <>
            {/* Tabs */}
            <div className="flex gap-2 mb-4 border-b border-gray-700">
              <button
                onClick={() => setActiveTab("server")}
                className={`px-4 py-2 ${
                  activeTab === "server"
                    ? "border-b-2 border-blue-500 text-white"
                    : "text-gray-400"
                }`}
              >
                Server
              </button>
              <button
                onClick={() => setActiveTab("user")}
                className={`px-4 py-2 ${
                  activeTab === "user"
                    ? "border-b-2 border-blue-500 text-white"
                    : "text-gray-400"
                }`}
              >
                User
              </button>
              <button
                onClick={() => setActiveTab("advanced")}
                className={`px-4 py-2 ${
                  activeTab === "advanced"
                    ? "border-b-2 border-blue-500 text-white"
                    : "text-gray-400"
                }`}
              >
                Advanced
              </button>
            </div>

            {/* Server Tab */}
            {activeTab === "server" && (
              <div className="space-y-4">
                <div>
                  <label className="block text-sm font-medium mb-1">Default Server</label>
                  <input
                    type="text"
                    value={config.server.host}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        server: { ...config.server, host: e.target.value },
                      })
                    }
                    className="w-full bg-gray-700 text-white px-3 py-2 rounded border border-gray-600 focus:outline-none focus:border-blue-500"
                  />
                </div>

                <div>
                  <label className="block text-sm font-medium mb-1">Port</label>
                  <input
                    type="number"
                    value={config.server.port}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        server: { ...config.server, port: parseInt(e.target.value) },
                      })
                    }
                    className="w-full bg-gray-700 text-white px-3 py-2 rounded border border-gray-600 focus:outline-none focus:border-blue-500"
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
                    className="w-full bg-gray-700 text-white px-3 py-2 rounded border border-gray-600 focus:outline-none focus:border-blue-500"
                  />
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
                    className="w-full bg-gray-700 text-white px-3 py-2 rounded border border-gray-600 focus:outline-none focus:border-blue-500"
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
                    className="w-full bg-gray-700 text-white px-3 py-2 rounded border border-gray-600 focus:outline-none focus:border-blue-500"
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
                    className="w-full bg-gray-700 text-white px-3 py-2 rounded border border-gray-600 focus:outline-none focus:border-blue-500"
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
                    className="w-full bg-gray-700 text-white px-3 py-2 rounded border border-gray-600 focus:outline-none focus:border-blue-500"
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
                    className="w-full bg-gray-700 text-white px-3 py-2 rounded border border-gray-600 focus:outline-none focus:border-blue-500"
                  />
                </div>

                <div>
                  <label className="block text-sm font-medium mb-1">
                    Slowdown Rate (0-1)
                  </label>
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
                    className="w-full bg-gray-700 text-white px-3 py-2 rounded border border-gray-600 focus:outline-none focus:border-blue-500"
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
                    className="w-full bg-gray-700 text-white px-3 py-2 rounded border border-gray-600 focus:outline-none focus:border-blue-500"
                  />
                </div>
              </div>
            )}

            {error && (
              <div className="mt-4 bg-red-900 border border-red-700 text-red-200 px-4 py-2 rounded text-sm">
                {error}
              </div>
            )}

            <div className="flex gap-2 mt-6">
              <button
                onClick={handleSave}
                disabled={loading}
                className="flex-1 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-600 disabled:cursor-not-allowed text-white px-4 py-2 rounded"
              >
                {loading ? "Saving..." : "Save"}
              </button>
              <button
                onClick={onClose}
                className="flex-1 bg-gray-700 hover:bg-gray-600 text-white px-4 py-2 rounded"
              >
                Cancel
              </button>
            </div>
          </>
        ) : (
          <div className="text-center py-8">
            <p className="text-red-400">Failed to load settings</p>
          </div>
        )}
      </div>
    </div>
  );
}
