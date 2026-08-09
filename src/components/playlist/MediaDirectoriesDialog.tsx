import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { LuChevronDown, LuChevronUp } from "react-icons/lu";
import { SyncplayConfig } from "../../types/config";
import { useNotificationStore } from "../../store/notifications";
import { AppDialogPortal } from "../common/AppDialogPortal";

interface MediaDirectoriesDialogProps {
  isOpen: boolean;
  onClose: () => void;
}

export function MediaDirectoriesDialog({ isOpen, onClose }: MediaDirectoriesDialogProps) {
  const [config, setConfig] = useState<SyncplayConfig | null>(null);
  const [loading, setLoading] = useState(false);
  const [mediaDirectoryInput, setMediaDirectoryInput] = useState("");
  const [mediaIndexTimeoutInput, setMediaIndexTimeoutInput] = useState("20");
  const addNotification = useNotificationStore((state) => state.addNotification);

  useEffect(() => {
    if (!isOpen) {
      setConfig(null);
      setMediaDirectoryInput("");
      setMediaIndexTimeoutInput("20");
      return;
    }
    const loadConfig = async () => {
      setLoading(true);
      try {
        const loaded = await invoke<SyncplayConfig>("get_config");
        setConfig(loaded);
        setMediaIndexTimeoutInput(String(loaded.player.media_index_timeout_seconds));
      } catch (error) {
        addNotification({
          type: "error",
          message: "Failed to load media directory settings",
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
        message: "Failed to update media directories",
      });
    }
  };

  const saveMediaIndexTimeout = async () => {
    if (!config) return;
    const timeoutSeconds = Number(mediaIndexTimeoutInput);
    if (!Number.isInteger(timeoutSeconds) || timeoutSeconds < 1) {
      addNotification({
        type: "warning",
        message: "Media scan timeout must be a positive whole number",
      });
      setMediaIndexTimeoutInput(String(config.player.media_index_timeout_seconds));
      return;
    }
    if (timeoutSeconds === config.player.media_index_timeout_seconds) {
      return;
    }

    const nextConfig: SyncplayConfig = {
      ...config,
      player: {
        ...config.player,
        media_index_timeout_seconds: timeoutSeconds,
      },
    };
    await saveConfig(nextConfig);
  };

  const addMediaDirectory = async () => {
    if (!config) return;
    const trimmed = mediaDirectoryInput.trim();
    if (!trimmed) return;
    if (config.player.media_directories.includes(trimmed)) {
      addNotification({
        type: "warning",
        message: "Media directory already exists",
      });
      return;
    }
    const nextConfig: SyncplayConfig = {
      ...config,
      player: {
        ...config.player,
        media_directories: [...config.player.media_directories, trimmed],
      },
    };
    await saveConfig(nextConfig);
    setMediaDirectoryInput("");
  };

  const addMediaDirectoryFromPicker = async () => {
    if (!config) return;
    let selected: string | string[] | null = null;
    try {
      selected = await open({
        multiple: false,
        directory: true,
      });
    } catch (error) {
      addNotification({
        type: "error",
        message: "Failed to open directory picker",
      });
      return;
    }
    if (!selected || Array.isArray(selected)) {
      return;
    }
    if (config.player.media_directories.includes(selected)) {
      addNotification({
        type: "warning",
        message: "Media directory already exists",
      });
      return;
    }
    const nextConfig: SyncplayConfig = {
      ...config,
      player: {
        ...config.player,
        media_directories: [...config.player.media_directories, selected],
      },
    };
    await saveConfig(nextConfig);
  };

  const removeMediaDirectory = async (dir: string) => {
    if (!config) return;
    const nextConfig: SyncplayConfig = {
      ...config,
      player: {
        ...config.player,
        media_directories: config.player.media_directories.filter((entry) => entry !== dir),
      },
    };
    await saveConfig(nextConfig);
  };

  const moveMediaDirectory = async (index: number, offset: -1 | 1) => {
    if (!config) return;
    const targetIndex = index + offset;
    const directories = config.player.media_directories;
    if (targetIndex < 0 || targetIndex >= directories.length) return;

    const nextDirectories = [...directories];
    [nextDirectories[index], nextDirectories[targetIndex]] = [
      nextDirectories[targetIndex],
      nextDirectories[index],
    ];

    const nextConfig: SyncplayConfig = {
      ...config,
      player: {
        ...config.player,
        media_directories: nextDirectories,
      },
    };
    await saveConfig(nextConfig);
  };

  if (!isOpen) return null;

  return (
    <AppDialogPortal className="fixed inset-0 app-overlay app-dialog-overlay flex items-center justify-center">
      <div className="app-panel app-panel-glass rounded-xl p-6 w-full max-w-2xl max-h-[80vh] overflow-auto app-dialog-panel">
        <div className="flex flex-wrap items-center justify-between gap-4 mb-4">
          <div>
            <h2 className="text-xl font-bold">Media Directories</h2>
            <p className="text-xs app-text-muted">Manage directories used for playlist matching.</p>
          </div>
          <button onClick={onClose} className="btn-neutral px-3 py-2 rounded-md text-sm">
            Close
          </button>
        </div>

        {loading && !config ? (
          <div className="text-center py-8">
            <p className="app-text-muted">Loading directories...</p>
          </div>
        ) : config ? (
          <div className="space-y-4">
            <div>
              <label className="text-sm font-medium mb-2 block">Scan timeout</label>
              <div className="flex flex-col sm:flex-row sm:items-center gap-2">
                <input
                  type="number"
                  min={1}
                  step={1}
                  value={mediaIndexTimeoutInput}
                  onChange={(e) => setMediaIndexTimeoutInput(e.target.value)}
                  onBlur={saveMediaIndexTimeout}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      e.currentTarget.blur();
                    }
                  }}
                  className="w-full sm:w-32 app-input px-3 py-2 rounded focus:outline-none"
                />
                <span className="text-xs app-text-muted">
                  Seconds allowed for media directory scanning.
                </span>
              </div>
            </div>

            <div>
              <label className="text-sm font-medium mb-2 block">Add directory</label>
              <div className="flex flex-col sm:flex-row gap-2">
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
                  className="flex-1 app-input px-3 py-2 rounded focus:outline-none"
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
            </div>

            <div>
              <label className="text-sm font-medium mb-2 block">Current directories</label>
              {config.player.media_directories.length === 0 ? (
                <p className="text-xs app-text-muted">No media directories added.</p>
              ) : (
                <div className="space-y-2">
                  {config.player.media_directories.map((dir, index) => (
                    <div
                      key={dir}
                      className="flex items-center justify-between gap-2 app-panel-muted px-3 py-2 rounded-lg"
                    >
                      <span className="text-sm truncate min-w-0 flex-1">{dir}</span>
                      <div className="flex items-center gap-1 shrink-0">
                        <button
                          type="button"
                          onClick={() => moveMediaDirectory(index, -1)}
                          disabled={index === 0}
                          className="btn-neutral app-icon-button disabled:opacity-40 disabled:cursor-not-allowed"
                          aria-label="Move up"
                        >
                          <LuChevronUp className="app-icon" />
                        </button>
                        <button
                          type="button"
                          onClick={() => moveMediaDirectory(index, 1)}
                          disabled={index === config.player.media_directories.length - 1}
                          className="btn-neutral app-icon-button disabled:opacity-40 disabled:cursor-not-allowed"
                          aria-label="Move down"
                        >
                          <LuChevronDown className="app-icon" />
                        </button>
                        <button
                          type="button"
                          onClick={() => removeMediaDirectory(dir)}
                          className="text-xs app-text-danger hover:opacity-80"
                        >
                          Remove
                        </button>
                      </div>
                    </div>
                  ))}
                </div>
              )}
              <p className="text-xs app-text-muted mt-2">
                Files are matched locally against these directories.
              </p>
            </div>
          </div>
        ) : null}
      </div>
    </AppDialogPortal>
  );
}
