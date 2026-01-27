import { useSyncplayStore } from "../../store";
import { useNotificationStore } from "../../store/notifications";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

interface SyncplayConfig {
  player: {
    media_directories: string[];
  };
}

export function PlaylistPanel() {
  const playlist = useSyncplayStore((state) => state.playlist);
  const connection = useSyncplayStore((state) => state.connection);
  const addNotification = useNotificationStore((state) => state.addNotification);

  const normalizePath = (path: string) =>
    path.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();

  const handleAddFile = async () => {
    if (!connection.connected) return;

    let config: SyncplayConfig | null = null;
    try {
      config = await invoke<SyncplayConfig>("get_config");
    } catch (error) {
      addNotification({
        type: "error",
        message: "Failed to load config for file picker",
      });
      return;
    }

    const mediaDirectories = config.player.media_directories.filter((dir) => dir.trim() !== "");
    if (mediaDirectories.length === 0) {
      addNotification({
        type: "warning",
        message: "Set media directories in Settings before adding files",
      });
      return;
    }

    let selected: string | string[] | null = null;
    try {
      selected = await open({
        multiple: false,
        directory: false,
        defaultPath: mediaDirectories[0],
      });
    } catch (error) {
      addNotification({
        type: "error",
        message: "Failed to open file picker",
      });
      return;
    }

    if (!selected || Array.isArray(selected)) {
      return;
    }

    const normalizedFile = normalizePath(selected);
    const normalizedDirs = mediaDirectories.map(normalizePath);
    const isInDirectory = normalizedDirs.some((dir) => normalizedFile.startsWith(`${dir}/`));
    if (!isInDirectory) {
      addNotification({
        type: "error",
        message: "Selected file is outside the media directories",
      });
      return;
    }

    const filename = selected.split(/[/\\\\]/).pop() || selected;
    try {
      await invoke("update_playlist", {
        action: "add",
        filename,
      });
    } catch (error) {
      addNotification({
        type: "error",
        message: "Failed to add file to playlist",
      });
    }
  };

  const handleRemoveItem = async (index: number) => {
    try {
      await invoke("update_playlist", {
        action: "remove",
        filename: index.toString(),
      });
    } catch (error) {
      console.error("Failed to remove item:", error);
    }
  };

  const handleNext = async () => {
    try {
      await invoke("update_playlist", {
        action: "next",
        filename: null,
      });
    } catch (error) {
      console.error("Failed to go to next:", error);
    }
  };

  const handlePrevious = async () => {
    try {
      await invoke("update_playlist", {
        action: "previous",
        filename: null,
      });
    } catch (error) {
      console.error("Failed to go to previous:", error);
    }
  };

  const handleClear = async () => {
    try {
      await invoke("update_playlist", {
        action: "clear",
        filename: null,
      });
    } catch (error) {
      console.error("Failed to clear playlist:", error);
    }
  };

  return (
    <div className="flex flex-col h-full app-sidebar-right">
      {/* Header */}
      <div className="p-4 border-b app-divider app-surface">
        <h2 className="text-lg font-semibold mb-2">Playlist</h2>
        <div className="flex gap-2">
          <button
            onClick={handleAddFile}
            disabled={!connection.connected}
            className="flex-1 bg-blue-600 hover:bg-blue-500 disabled:opacity-60 disabled:cursor-not-allowed text-white px-3 py-1 rounded-md text-sm"
          >
            Add File
          </button>
          <button
            onClick={handleClear}
            disabled={!connection.connected || playlist.items.length === 0}
            className="bg-red-600 hover:bg-red-500 disabled:opacity-60 disabled:cursor-not-allowed text-white px-3 py-1 rounded-md text-sm"
          >
            Clear
          </button>
        </div>
      </div>

      {/* Playlist items */}
      <div className="flex-1 overflow-auto p-4">
        {playlist.items.length === 0 ? (
          <p className="app-text-muted text-sm">No items in playlist</p>
        ) : (
          <div className="space-y-2">
            {playlist.items.map((item, index) => (
              <div
                key={index}
                className={`p-2 rounded-md text-sm ${
                  index === playlist.currentIndex ? "bg-blue-600 text-white" : "app-panel-muted"
                }`}
              >
                <div className="flex items-center justify-between">
                  <span className="truncate flex-1">{item}</span>
                  <button
                    onClick={() => handleRemoveItem(index)}
                    disabled={!connection.connected}
                    className="ml-2 text-red-500 hover:text-red-400 disabled:opacity-60"
                  >
                    ✕
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Navigation controls */}
      <div className="p-4 border-t app-divider app-surface">
        <div className="flex gap-2">
          <button
            onClick={handlePrevious}
            disabled={
              !connection.connected ||
              playlist.items.length === 0 ||
              playlist.currentIndex === null ||
              playlist.currentIndex === 0
            }
            className="flex-1 btn-neutral disabled:cursor-not-allowed px-3 py-2 rounded-md text-sm"
          >
            ← Previous
          </button>
          <button
            onClick={handleNext}
            disabled={
              !connection.connected ||
              playlist.items.length === 0 ||
              playlist.currentIndex === null ||
              playlist.currentIndex >= playlist.items.length - 1
            }
            className="flex-1 btn-neutral disabled:cursor-not-allowed px-3 py-2 rounded-md text-sm"
          >
            Next →
          </button>
        </div>
      </div>
    </div>
  );
}
