import { useSyncplayStore } from "../../store";
import { FiChevronLeft, FiChevronRight, FiPlus, FiTrash2, FiX } from "react-icons/fi";
import { useNotificationStore } from "../../store/notifications";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { SyncplayConfig } from "../../types/config";

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
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="p-4 border-b app-divider app-surface">
        <h2 className="text-lg font-semibold mb-2">Playlist</h2>
        <div className="flex gap-2">
          <button
            onClick={handleAddFile}
            disabled={!connection.connected}
            className="btn-primary app-icon-button disabled:opacity-60 disabled:cursor-not-allowed"
            title="Add file"
            aria-label="Add file"
          >
            <FiPlus className="app-icon" />
          </button>
          <button
            onClick={handleClear}
            disabled={!connection.connected || playlist.items.length === 0}
            className="btn-danger app-icon-button disabled:opacity-60 disabled:cursor-not-allowed"
            title="Clear playlist"
            aria-label="Clear playlist"
          >
            <FiTrash2 className="app-icon" />
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
                  index === playlist.currentIndex ? "app-item-active" : "app-panel-muted"
                }`}
              >
                <div className="flex items-center justify-between">
                  <span className="truncate flex-1">{item}</span>
                  <button
                    onClick={() => handleRemoveItem(index)}
                    disabled={!connection.connected}
                    className="ml-2 app-text-danger hover:opacity-80 disabled:opacity-60"
                    title="Remove item"
                    aria-label="Remove item"
                  >
                    <FiX className="app-icon" />
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
            className="btn-neutral app-icon-button disabled:cursor-not-allowed"
            title="Previous"
            aria-label="Previous"
          >
            <FiChevronLeft className="app-icon" />
          </button>
          <button
            onClick={handleNext}
            disabled={
              !connection.connected ||
              playlist.items.length === 0 ||
              playlist.currentIndex === null ||
              playlist.currentIndex >= playlist.items.length - 1
            }
            className="btn-neutral app-icon-button disabled:cursor-not-allowed"
            title="Next"
            aria-label="Next"
          >
            <FiChevronRight className="app-icon" />
          </button>
        </div>
      </div>
    </div>
  );
}
