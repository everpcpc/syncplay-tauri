import { useSyncplayStore } from "../../store";
import {
  LuChevronLeft,
  LuChevronRight,
  LuFilm,
  LuFolder,
  LuListMusic,
  LuPlay,
  LuPlus,
  LuRefreshCw,
  LuRepeat,
  LuRepeat1,
  LuShield,
  LuTrash2,
  LuUsers,
} from "react-icons/lu";
import { useNotificationStore } from "../../store/notifications";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { SyncplayConfig } from "../../types/config";
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type DragEvent } from "react";
import { createPortal } from "react-dom";
import { MediaDirectoriesDialog } from "./MediaDirectoriesDialog";
import { TrustedDomainsDialog } from "./TrustedDomainsDialog";

const LAST_ADD_FILE_DIRECTORY_KEY = "syncplay.lastAddFileDirectory";
const PLAYLIST_REORDER_ANIMATION_MS = 180;

interface PlaylistItemStatus {
  filename: string;
  path: string | null;
  available: boolean;
}

interface PlaylistReorderAnimation {
  expectedItems: string[];
  startRects: Map<number, DOMRect>;
}

function reorderItems<T>(items: readonly T[], fromIndex: number, toIndex: number): T[] | null {
  if (fromIndex < 0 || fromIndex >= items.length) return null;
  if (toIndex < 0 || toIndex > items.length || fromIndex === toIndex) return null;
  const nextItems = [...items];
  const [moved] = nextItems.splice(fromIndex, 1);
  const insertIndex = Math.min(fromIndex < toIndex ? toIndex - 1 : toIndex, nextItems.length);
  nextItems.splice(insertIndex, 0, moved);
  return nextItems.every((item, index) => item === items[index]) ? null : nextItems;
}

export function PlaylistPanel() {
  const playlist = useSyncplayStore((state) => state.playlist);
  const connection = useSyncplayStore((state) => state.connection);
  const player = useSyncplayStore((state) => state.player);
  const config = useSyncplayStore((state) => state.config);
  const setConfig = useSyncplayStore((state) => state.setConfig);
  const mediaIndexVersion = useSyncplayStore((state) => state.mediaIndexVersion);
  const mediaIndexRefreshing = useSyncplayStore((state) => state.mediaIndexRefreshing);
  const addNotification = useNotificationStore((state) => state.addNotification);
  const [showMediaDirectories, setShowMediaDirectories] = useState(false);
  const [showTrustedDomains, setShowTrustedDomains] = useState(false);
  const [availability, setAvailability] = useState<PlaylistItemStatus[]>([]);
  const availabilityRef = useRef<PlaylistItemStatus[]>([]);
  const playlistContainerRef = useRef<HTMLDivElement | null>(null);
  const dragIndexRef = useRef<number | null>(null);
  const dragOverIndexRef = useRef<number | null>(null);
  const dragStartRef = useRef<{
    index: number;
    startX: number;
    startY: number;
    offsetX: number;
    offsetY: number;
    width: number;
    height: number;
    text: string;
    active: boolean;
  } | null>(null);
  const dragGhostRafRef = useRef<number | null>(null);
  const reorderAnimationRef = useRef<PlaylistReorderAnimation | null>(null);
  const [draggingIndex, setDraggingIndex] = useState<number | null>(null);
  const [dragTargetIndex, setDragTargetIndex] = useState<number | null>(null);
  const [dragGhost, setDragGhost] = useState<{
    text: string;
    width: number;
    height: number;
    x: number;
    y: number;
  } | null>(null);

  useEffect(() => {
    availabilityRef.current = availability;
  }, [availability]);

  const refreshAvailability = useCallback(
    async (items: string[], mode: "all" | "missing", cancelledRef: { cancelled: boolean }) => {
      if (items.length === 0) {
        setAvailability([]);
        return;
      }
      const existing = availabilityRef.current;
      const shouldRefreshAll = mode === "all" || existing.length !== items.length;
      const targetIndexes = shouldRefreshAll
        ? items.map((_, index) => index)
        : existing
            .map((info, index) => (info?.path ? null : index))
            .filter((index): index is number => index !== null);
      if (targetIndexes.length === 0) return;
      const targetItems = targetIndexes.map((index) => items[index]);
      let result: PlaylistItemStatus[] = [];
      try {
        result = await invoke<PlaylistItemStatus[]>("check_playlist_items", {
          items: targetItems,
        });
      } catch (error) {
        result = targetItems.map((item) => ({
          filename: item,
          path: null,
          available: false,
        }));
      }
      if (cancelledRef.cancelled) return;
      setAvailability((prev) => {
        const base =
          shouldRefreshAll && result.length === items.length
            ? result
            : prev.length === items.length
              ? [...prev]
              : items.map((item) => ({
                  filename: item,
                  path: null,
                  available: false,
                }));
        targetIndexes.forEach((targetIndex, idx) => {
          base[targetIndex] = result[idx];
        });
        return base;
      });
    },
    []
  );

  useEffect(() => {
    const cancelledRef = { cancelled: false };
    void refreshAvailability(playlist.items, "all", cancelledRef);
    return () => {
      cancelledRef.cancelled = true;
    };
  }, [playlist.items, config?.player.media_directories, refreshAvailability]);

  useEffect(() => {
    const cancelledRef = { cancelled: false };
    const items = playlist.items;
    const existing = availabilityRef.current;
    const mode = existing.length === 0 || existing.length !== items.length ? "all" : "missing";
    void refreshAvailability(items, mode, cancelledRef);
    return () => {
      cancelledRef.cancelled = true;
    };
  }, [mediaIndexVersion, playlist.items, refreshAvailability]);

  const renderDragGhost = () => {
    if (!dragGhost || typeof document === "undefined") return null;
    return createPortal(
      <div
        className="playlist-drag-ghost pointer-events-none fixed"
        style={{
          left: dragGhost.x,
          top: dragGhost.y,
          width: dragGhost.width,
          height: dragGhost.height,
        }}
      >
        <div className="h-full w-full rounded-md app-panel-glass text-sm px-2 py-2">
          {dragGhost.text}
        </div>
      </div>,
      document.body
    );
  };

  const normalizePath = useCallback(
    (path: string) => path.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase(),
    []
  );

  const isPathInsideDirectory = useCallback(
    (path: string, directory: string) => {
      const normalizedPath = normalizePath(path);
      const normalizedDirectory = normalizePath(directory);
      return (
        normalizedPath === normalizedDirectory ||
        normalizedPath.startsWith(`${normalizedDirectory}/`)
      );
    },
    [normalizePath]
  );

  const getParentDirectory = (path: string) => {
    const normalized = path.replace(/\\/g, "/");
    const index = normalized.lastIndexOf("/");
    return index > 0 ? path.slice(0, index) : null;
  };

  const getLastAddFileDirectory = useCallback(
    (mediaDirectories: string[]) => {
      const lastDirectory = window.localStorage.getItem(LAST_ADD_FILE_DIRECTORY_KEY)?.trim();
      if (
        lastDirectory &&
        mediaDirectories.some((dir) => isPathInsideDirectory(lastDirectory, dir))
      ) {
        return lastDirectory;
      }
      return mediaDirectories[0];
    },
    [isPathInsideDirectory]
  );

  const rememberAddFileDirectory = (path: string) => {
    const parentDirectory = getParentDirectory(path);
    if (!parentDirectory) return;
    window.localStorage.setItem(LAST_ADD_FILE_DIRECTORY_KEY, parentDirectory);
  };

  const normalizeFilename = (value: string | null | undefined) => {
    if (!value) return "";
    const base = value.split(/[/\\\\]/).pop() || value;
    return base.trim().toLowerCase();
  };

  const formatLastScan = (timestamp: number) => {
    if (!timestamp) return "Never";
    const date = new Date(timestamp);
    if (Number.isNaN(date.getTime())) return "Never";
    return date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  };

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
        defaultPath: getLastAddFileDirectory(mediaDirectories),
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
    const isInDirectory = mediaDirectories.some((dir) =>
      isPathInsideDirectory(normalizedFile, dir)
    );
    if (!isInDirectory) {
      addNotification({
        type: "error",
        message: "Selected file is outside the media directories",
      });
      return;
    }

    try {
      await invoke("update_playlist", {
        action: "add",
        filename: selected,
      });
      rememberAddFileDirectory(selected);
    } catch (error) {
      addNotification({
        type: "error",
        message: "Failed to add file to playlist",
      });
    }
  };

  const handleDropPaths = useCallback(
    async (paths: string[]) => {
      if (!connection.connected) return;

      const resolvedPaths = paths.map((path) => path.trim()).filter((path) => path !== "");
      if (resolvedPaths.length === 0) return;

      const rejected: string[] = [];

      for (const path of resolvedPaths) {
        try {
          await invoke("update_playlist", {
            action: "add",
            filename: path,
          });
        } catch (error) {
          rejected.push(path);
        }
      }

      if (rejected.length > 0) {
        addNotification({
          type: "warning",
          message: `Skipped ${rejected.length} file(s) that could not be added`,
        });
      }
    },
    [addNotification, connection.connected]
  );

  const handleDropFiles = async (event: DragEvent<HTMLDivElement>) => {
    event.preventDefault();
    event.stopPropagation();
    if (isTauri()) return;

    const files = Array.from(event.dataTransfer.files);
    const paths = files
      .map((file) => (file as { path?: string }).path ?? file.name)
      .filter((path): path is string => Boolean(path));
    if (paths.length === 0) return;

    await handleDropPaths(paths);
  };

  const handleReorderItems = useCallback(
    async (fromIndex: number, toIndex: number, animation: PlaylistReorderAnimation | null) => {
      const clearAnimation = () => {
        if (reorderAnimationRef.current === animation) {
          reorderAnimationRef.current = null;
        }
      };
      if (!connection.connected) {
        clearAnimation();
        return;
      }
      const nextItems = reorderItems(playlist.items, fromIndex, toIndex);
      if (!nextItems) {
        clearAnimation();
        return;
      }

      try {
        await invoke("update_playlist", {
          action: "reorder",
          items: nextItems,
        });
      } catch (error) {
        clearAnimation();
        addNotification({
          type: "error",
          message: "Failed to reorder playlist",
        });
      }
    },
    [addNotification, connection.connected, playlist.items]
  );

  const getDropIndex = useCallback((clientY: number) => {
    const container = playlistContainerRef.current;
    if (!container) return null;
    const items = Array.from(container.querySelectorAll<HTMLElement>("[data-playlist-item]"));
    if (items.length === 0) return null;
    for (const item of items) {
      const rect = item.getBoundingClientRect();
      const midpoint = rect.top + rect.height / 2;
      const index = Number(item.dataset.index);
      if (Number.isNaN(index)) continue;
      if (clientY < midpoint) {
        return index;
      }
    }
    return items.length;
  }, []);

  const capturePlaylistItemRects = useCallback(
    (fromIndex: number, toIndex: number): PlaylistReorderAnimation | null => {
      const expectedItems = reorderItems(playlist.items, fromIndex, toIndex);
      const reorderedIndexes = reorderItems(
        playlist.items.map((_, index) => index),
        fromIndex,
        toIndex
      );
      const container = playlistContainerRef.current;
      if (!expectedItems || !reorderedIndexes || !container) return null;
      const currentRects = new Map<number, DOMRect>();
      container.querySelectorAll<HTMLElement>("[data-playlist-item]").forEach((item) => {
        const index = Number(item.dataset.index);
        if (!Number.isNaN(index)) currentRects.set(index, item.getBoundingClientRect());
      });
      const startRects = new Map<number, DOMRect>();
      reorderedIndexes.forEach((previousIndex, nextIndex) => {
        const rect = currentRects.get(previousIndex);
        if (rect) startRects.set(nextIndex, rect);
      });
      if (startRects.size === 0) return null;
      const animation = { expectedItems, startRects };
      reorderAnimationRef.current = animation;
      return animation;
    },
    [playlist.items]
  );

  useLayoutEffect(() => {
    const animation = reorderAnimationRef.current;
    if (!animation) return;
    const matchesExpectedOrder =
      animation.expectedItems.length === playlist.items.length &&
      animation.expectedItems.every((item, index) => item === playlist.items[index]);
    if (!matchesExpectedOrder) return;
    reorderAnimationRef.current = null;
    const container = playlistContainerRef.current;
    if (!container || window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    container.querySelectorAll<HTMLElement>("[data-playlist-item]").forEach((item) => {
      const index = Number(item.dataset.index);
      const startRect = Number.isNaN(index) ? null : animation.startRects.get(index);
      if (!startRect) return;
      const endRect = item.getBoundingClientRect();
      const deltaX = startRect.left - endRect.left;
      const deltaY = startRect.top - endRect.top;
      if (Math.abs(deltaX) < 1 && Math.abs(deltaY) < 1) return;
      item.animate(
        [{ transform: `translate(${deltaX}px, ${deltaY}px)` }, { transform: "translate(0, 0)" }],
        {
          duration: PLAYLIST_REORDER_ANIMATION_MS,
          easing: "cubic-bezier(0.22, 1, 0.36, 1)",
        }
      );
    });
  }, [playlist.items]);

  const scheduleDragGhostUpdate = useCallback((x: number, y: number) => {
    if (dragGhostRafRef.current !== null) return;
    dragGhostRafRef.current = window.requestAnimationFrame(() => {
      dragGhostRafRef.current = null;
      const dragState = dragStartRef.current;
      if (!dragState?.active) return;
      setDragGhost((prev) =>
        prev
          ? { ...prev, x, y }
          : {
              text: dragState.text,
              width: dragState.width,
              height: dragState.height,
              x,
              y,
            }
      );
    });
  }, []);

  const handlePointerMove = useCallback(
    (event: PointerEvent) => {
      const dragState = dragStartRef.current;
      if (!dragState) return;
      if (!dragState.active) {
        const deltaX = Math.abs(event.clientX - dragState.startX);
        const deltaY = Math.abs(event.clientY - dragState.startY);
        if (deltaX < 4 && deltaY < 4) {
          return;
        }
        dragState.active = true;
        setDraggingIndex(dragState.index);
      }
      const dropIndex = getDropIndex(event.clientY);
      if (dropIndex !== null && dragOverIndexRef.current !== dropIndex) {
        dragOverIndexRef.current = dropIndex;
        setDragTargetIndex(dropIndex);
      }
      const nextX = event.clientX - dragState.offsetX;
      const nextY = event.clientY - dragState.offsetY;
      scheduleDragGhostUpdate(nextX, nextY);
    },
    [getDropIndex, scheduleDragGhostUpdate]
  );

  const handlePointerUp = useCallback(() => {
    window.removeEventListener("pointermove", handlePointerMove);
    window.removeEventListener("pointerup", handlePointerUp);
    const dragState = dragStartRef.current;
    dragStartRef.current = null;
    setDraggingIndex(null);
    setDragTargetIndex(null);
    setDragGhost(null);
    if (dragGhostRafRef.current !== null) {
      window.cancelAnimationFrame(dragGhostRafRef.current);
      dragGhostRafRef.current = null;
    }
    if (!dragState?.active) {
      dragIndexRef.current = null;
      dragOverIndexRef.current = null;
      return;
    }
    const fromIndex = dragState.index;
    const toIndex = dragOverIndexRef.current ?? fromIndex;
    const animation = capturePlaylistItemRects(fromIndex, toIndex);
    dragIndexRef.current = null;
    dragOverIndexRef.current = null;
    void handleReorderItems(fromIndex, toIndex, animation);
  }, [capturePlaylistItemRects, handlePointerMove, handleReorderItems]);

  useEffect(() => {
    if (!isTauri()) return;

    let unlisten: (() => void) | null = null;

    const setup = async () => {
      try {
        const webview = getCurrentWebview();
        unlisten = await webview.onDragDropEvent((event) => {
          const payload = event.payload;
          if (payload.type !== "drop" || payload.paths.length === 0) return;
          void handleDropPaths(payload.paths);
        });
      } catch {}
    };

    void setup();
    return () => {
      if (unlisten) {
        unlisten();
      }
    };
  }, [handleDropPaths]);

  useEffect(() => {
    return () => {
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", handlePointerUp);
      if (dragGhostRafRef.current !== null) {
        window.cancelAnimationFrame(dragGhostRafRef.current);
        dragGhostRafRef.current = null;
      }
    };
  }, [handlePointerMove, handlePointerUp]);

  const handleScanMediaDirectory = async () => {
    if (mediaIndexRefreshing) return;
    try {
      await invoke("refresh_media_index");
    } catch (error) {
      addNotification({
        type: "error",
        message: "Failed to scan media directory",
      });
    }
  };

  const scanLabel = mediaIndexRefreshing ? "Scanning media directory" : "Scan media directory";
  const scanTooltip = `${scanLabel} (Last scan: ${formatLastScan(mediaIndexVersion)})`;

  const dropIndicatorIndex =
    draggingIndex !== null &&
    dragTargetIndex !== null &&
    dragTargetIndex !== draggingIndex &&
    dragTargetIndex !== draggingIndex + 1
      ? dragTargetIndex
      : null;
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

  const handlePlayItem = async (index: number) => {
    if (!connection.connected) return;
    try {
      await invoke("update_playlist", {
        action: "select",
        filename: index.toString(),
      });
    } catch (error) {
      console.error("Failed to select playlist item:", error);
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
    <div className="flex flex-col h-full min-h-0 min-w-0">
      {renderDragGhost()}

      {/* Header */}
      <div className="p-4 border-b app-divider app-surface">
        <div className="flex flex-col gap-2">
          <div className="flex items-center justify-between gap-2">
            <div className="flex items-center gap-2 shrink-0">
              <LuListMusic className="app-icon app-text-muted" />
              <span className="text-sm font-semibold">Playlist</span>
              <span className="text-xs app-text-muted app-tnum">({playlist.items.length})</span>
            </div>
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
                onClick={handleScanMediaDirectory}
                disabled={mediaIndexRefreshing}
                className="btn-neutral app-icon-button disabled:opacity-60 disabled:cursor-not-allowed app-tooltip-right"
                aria-label={scanTooltip}
              >
                <LuRefreshCw className="app-icon" />
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
      <div
        ref={playlistContainerRef}
        className="flex-1 min-h-0 overflow-auto p-4"
        onDragOver={(event) => {
          event.preventDefault();
        }}
        onDrop={(event) => {
          void handleDropFiles(event);
        }}
      >
        {playlist.items.length === 0 ? (
          <div className="flex h-full flex-col items-center justify-center gap-1.5 text-center px-4">
            <LuListMusic className="app-icon app-text-muted" />
            <p className="app-text-muted text-sm">No items in playlist</p>
            <p className="app-text-muted text-xs">
              Drag files here or click + to add. Double-click an item to play it.
            </p>
          </div>
        ) : (
          <div
            className={`playlist-list space-y-2 ${
              dropIndicatorIndex === playlist.items.length ? "playlist-list-drop-at-end" : ""
            }`}
          >
            {playlist.items.map((item, index) =>
              (() => {
                const itemStatus = availability[index];
                const available = itemStatus?.available ?? true;
                const resolvedPath = itemStatus?.path ?? null;
                const isCurrent =
                  normalizeFilename(player.filename) !== "" &&
                  normalizeFilename(player.filename) === normalizeFilename(item);
                const tooltipText = resolvedPath ?? "";
                return (
                  <div
                    key={index}
                    onDoubleClick={() => {
                      if (available && !isCurrent) {
                        void handlePlayItem(index);
                      }
                    }}
                    className={`playlist-item p-2.5 rounded-lg text-sm select-none transition-transform transition-opacity duration-150 ${
                      isCurrent ? "app-item-playing" : "app-panel-muted group"
                    } ${
                      connection.connected && playlist.items.length > 1
                        ? "playlist-item-sortable"
                        : ""
                    } ${draggingIndex === index ? "playlist-item-dragging" : ""} ${
                      dropIndicatorIndex === index ? "playlist-item-drop-before" : ""
                    }`}
                    data-playlist-item
                    data-index={index}
                    onPointerDown={(event) => {
                      if (!connection.connected || playlist.items.length < 2) return;
                      if (event.button !== 0) return;
                      const target = event.target as HTMLElement | null;
                      if (target?.closest("button")) return;
                      event.preventDefault();
                      const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
                      dragIndexRef.current = index;
                      dragOverIndexRef.current = index;
                      dragStartRef.current = {
                        index,
                        startX: event.clientX,
                        startY: event.clientY,
                        offsetX: event.clientX - rect.left,
                        offsetY: event.clientY - rect.top,
                        width: rect.width,
                        height: rect.height,
                        text: item,
                        active: false,
                      };
                      window.addEventListener("pointermove", handlePointerMove);
                      window.addEventListener("pointerup", handlePointerUp);
                    }}
                  >
                    <div className="relative flex items-center gap-2">
                      <button
                        onClick={(event) => {
                          event.stopPropagation();
                          void handlePlayItem(index);
                        }}
                        disabled={!connection.connected || !available || isCurrent}
                        aria-label="Play"
                        className={`btn-neutral app-icon-button playlist-overlay-button app-text-muted hover:app-text-accent ${
                          isCurrent
                            ? "invisible pointer-events-none"
                            : "invisible group-hover:visible hover:visible focus-visible:visible pointer-events-none group-hover:pointer-events-auto"
                        } disabled:opacity-40 app-tooltip-side-right !absolute left-0 top-1/2 z-10`}
                      >
                        <LuPlay className="app-icon" />
                      </button>
                      <LuFilm
                        className={`app-icon shrink-0 ${
                          available
                            ? isCurrent
                              ? "app-text-accent"
                              : "app-text-muted"
                            : "app-text-muted opacity-50"
                        }`}
                        aria-hidden="true"
                      />
                      <span
                        className={`app-tooltip-text truncate flex-1 ${
                          available
                            ? isCurrent
                              ? "app-text-accent font-semibold"
                              : ""
                            : "app-text-muted"
                        }`}
                        aria-label={tooltipText || "Unresolved path"}
                      >
                        {item}
                      </span>
                      {isCurrent && (
                        <span className="text-[10px] px-2 leading-4 rounded-full app-tag-accent">
                          Playing
                        </span>
                      )}
                      {!available && (
                        <span className="text-[10px] px-2 py-1 rounded-full app-chip-muted app-text-danger">
                          Unavailable
                        </span>
                      )}
                      <button
                        onClick={(event) => {
                          event.stopPropagation();
                          void handleRemoveItem(index);
                        }}
                        disabled={!connection.connected}
                        aria-label="Remove"
                        className={`btn-neutral app-icon-button playlist-overlay-button app-text-danger hover:opacity-80 disabled:opacity-60 ${
                          isCurrent
                            ? "invisible pointer-events-none"
                            : "invisible group-hover:visible hover:visible focus-visible:visible pointer-events-none group-hover:pointer-events-auto"
                        } app-tooltip-side-left !absolute right-0 top-1/2 z-10`}
                      >
                        <LuTrash2 className="app-icon" />
                      </button>
                    </div>
                  </div>
                );
              })()
            )}
          </div>
        )}
      </div>

      {/* Navigation controls */}
      <div className="p-4 border-t app-divider app-surface">
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
