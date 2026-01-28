import { useSyncplayStore } from "../../store";
import {
  LuChevronLeft,
  LuChevronRight,
  LuFolder,
  LuListMusic,
  LuPlus,
  LuRepeat,
  LuRepeat1,
  LuShield,
  LuTrash2,
  LuUsers,
  LuX,
} from "react-icons/lu";
import { useNotificationStore } from "../../store/notifications";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { SyncplayConfig } from "../../types/config";
import { useState } from "react";
import { MediaDirectoriesDialog } from "./MediaDirectoriesDialog";
import { TrustedDomainsDialog } from "./TrustedDomainsDialog";

export function PlaylistPanel() {
  const playlist = useSyncplayStore((state) => state.playlist);
  const connection = useSyncplayStore((state) => state.connection);
  const config = useSyncplayStore((state) => state.config);
  const setConfig = useSyncplayStore((state) => state.setConfig);
  const addNotification = useNotificationStore((state) => state.addNotification);
  const [showMediaDirectories, setShowMediaDirectories] = useState(false);
  const [showTrustedDomains, setShowTrustedDomains] = useState(false);

  const normalizePath = (path: string) =>
    path.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();

  const updateUserSetting = async <K extends keyof SyncplayConfig["user"]>(
    key: K,
    value: SyncplayConfig["user"][K]
  ) => {
    try {
      const baseConfig = config ?? (await invoke<SyncplayConfig>("get_config"));
      const nextConfig: SyncplayConfig = {
        ...baseConfig,
        user: {
          ...baseConfig.user,
          [key]: value,
        },
      };
      await invoke("update_config", { config: nextConfig });
      setConfig(nextConfig);
    } catch (error) {
      addNotification({
        type: "error",
        message: "Failed to update playlist settings",
      });
    }
  };

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
      <div className="p-4 border-b app-divider app-surface rounded-t-2xl">
        <div className="flex flex-col gap-2">
          <div className="flex items-center justify-between gap-2">
            <LuListMusic className="app-icon app-text-muted" />
            <div className="flex items-center gap-2 flex-1">
              <button
                onClick={handleAddFile}
                disabled={!connection.connected}
                className="btn-primary app-icon-button disabled:opacity-60 disabled:cursor-not-allowed"
                aria-label="Add"
              >
                <LuPlus className="app-icon" />
              </button>
              <button
                onClick={handleClear}
                disabled={!connection.connected || playlist.items.length === 0}
                className="btn-danger app-icon-button disabled:opacity-60 disabled:cursor-not-allowed"
                aria-label="Clear"
              >
                <LuTrash2 className="app-icon" />
              </button>
            </div>
            <div className="flex items-center gap-2">
              <button
                onClick={() => setShowTrustedDomains(true)}
                className="btn-neutral app-icon-button"
                aria-label="Trusted domains"
              >
                <LuShield className="app-icon" />
              </button>
              <button
                onClick={() => setShowMediaDirectories(true)}
                className="btn-neutral app-icon-button app-tooltip-right"
                aria-label="Media directories"
              >
                <LuFolder className="app-icon" />
              </button>
            </div>
          </div>
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
                    className="ml-2 app-text-danger hover:opacity-80 disabled:opacity-60 app-tooltip"
                    aria-label="Remove"
                  >
                    <LuX className="app-icon" />
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Navigation controls */}
      <div className="p-4 border-t app-divider app-surface rounded-b-2xl">
        <div className="flex items-center justify-between gap-4">
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
              aria-label="Previous"
            >
              <LuChevronLeft className="app-icon" />
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
              aria-label="Next"
            >
              <LuChevronRight className="app-icon" />
            </button>
          </div>
          <div className="flex items-center gap-2">
            <button
              onClick={() =>
                updateUserSetting("shared_playlist_enabled", !config?.user.shared_playlist_enabled)
              }
              className={`btn-neutral app-icon-button ${
                config?.user.shared_playlist_enabled ? "app-tag-accent" : ""
              }`}
              aria-label={
                config?.user.shared_playlist_enabled
                  ? "Shared playlists on"
                  : "Shared playlists off"
              }
            >
              <LuUsers className="app-icon" />
            </button>
            <button
              onClick={() =>
                updateUserSetting("loop_at_end_of_playlist", !config?.user.loop_at_end_of_playlist)
              }
              className={`btn-neutral app-icon-button ${
                config?.user.loop_at_end_of_playlist ? "app-tag-accent" : ""
              }`}
              aria-label={
                config?.user.loop_at_end_of_playlist ? "Loop playlist on" : "Loop playlist off"
              }
            >
              <LuRepeat className="app-icon" />
            </button>
            <button
              onClick={() =>
                updateUserSetting("loop_single_files", !config?.user.loop_single_files)
              }
              className={`btn-neutral app-icon-button ${
                config?.user.loop_single_files ? "app-tag-accent" : ""
              } app-tooltip-right`}
              aria-label={config?.user.loop_single_files ? "Loop file on" : "Loop file off"}
            >
              <LuRepeat1 className="app-icon" />
            </button>
          </div>
        </div>
      </div>

      <MediaDirectoriesDialog
        isOpen={showMediaDirectories}
        onClose={() => setShowMediaDirectories(false)}
      />
      <TrustedDomainsDialog
        isOpen={showTrustedDomains}
        onClose={() => setShowTrustedDomains(false)}
      />
    </div>
  );
}
