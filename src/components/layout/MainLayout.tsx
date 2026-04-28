import { useCallback, useEffect, useRef, useState } from "react";
import { UserList } from "../users/UserList";
import { ChatPanel } from "../chat/ChatPanel";
import { PlayerStatus } from "../player/PlayerStatus";
import {
  LuColumns2,
  LuContrast,
  LuLock,
  LuDroplet,
  LuDroplets,
  LuLink2,
  LuListMinus,
  LuListMusic,
  LuMoon,
  LuRows2,
  LuSettings,
  LuSun,
  LuZap,
} from "react-icons/lu";
import { getVersion } from "@tauri-apps/api/app";
import { PhysicalSize } from "@tauri-apps/api/dpi";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { type DownloadEvent, type Update } from "@tauri-apps/plugin-updater";
import { useWindowDrag } from "../../hooks/useWindowDrag";
import { PlaylistPanel } from "../playlist/PlaylistPanel";
import { ConnectionDialog } from "../connection/ConnectionDialog";
import { SettingsDialog } from "../settings/SettingsDialog";
import { NotificationContainer } from "../notifications/NotificationContainer";
import { useSyncplayStore } from "../../store";
import { useNotificationStore } from "../../store/notifications";
import { invoke, isTauri } from "@tauri-apps/api/core";
import {
  applyTheme,
  applyTransparency,
  normalizeTheme,
  ThemePreference,
  TransparencyPreference,
  normalizeTransparency,
} from "../../services/theme";
import {
  checkForUpdates,
  downloadAndInstallUpdate,
  formatBytes,
  formatUpdateError,
  nextUpdateProgress,
  shouldAutoCheckUpdates,
  type UpdateProgress,
} from "../../services/updater";
import { SyncplayConfig, UserPreferences } from "../../types/config";

type UiLayoutPreferencePatch = Partial<
  Pick<
    UserPreferences,
    "side_column_width" | "side_panel_primary_size" | "window_width" | "window_height"
  >
>;

export function MainLayout() {
  const appWindow = isTauri() ? getCurrentWindow() : null;
  const layoutRef = useRef<HTMLDivElement | null>(null);
  const sidePanelsRef = useRef<HTMLDivElement | null>(null);
  const [showConnectionDialog, setShowConnectionDialog] = useState(false);
  const [showSettingsDialog, setShowSettingsDialog] = useState(false);
  const [showPlaylist, setShowPlaylist] = useState(true);
  const [sideLayout, setSideLayout] = useState<"columns" | "rows">("rows");
  const [theme, setTheme] = useState<ThemePreference>("dark");
  const [transparencyMode, setTransparencyMode] = useState<TransparencyPreference>("off");
  const [layoutSize, setLayoutSize] = useState({ width: 0, height: 0 });
  const [sidePanelsSize, setSidePanelsSize] = useState({ width: 0, height: 0 });
  const [sideWidth, setSideWidth] = useState<number | null>(null);
  const [sidePanelSize, setSidePanelSize] = useState<number | null>(null);
  const [appVersion, setAppVersion] = useState<string | null>(null);
  const [updateVersion, setUpdateVersion] = useState<string | null>(null);
  const [isInstallingUpdate, setIsInstallingUpdate] = useState(false);
  const [updateProgress, setUpdateProgress] = useState<UpdateProgress | null>(null);
  const [settingsInitialTab, setSettingsInitialTab] = useState<
    "sync" | "ready" | "privacy" | "chat" | "osd" | "misc" | undefined
  >(undefined);
  const connection = useSyncplayStore((state) => state.connection);
  const tlsStatus = useSyncplayStore((state) => state.tlsStatus);
  const rttMs = useSyncplayStore((state) => state.rttMs);
  const config = useSyncplayStore((state) => state.config);
  const setConfig = useSyncplayStore((state) => state.setConfig);
  const addNotification = useNotificationStore((state) => state.addNotification);
  const initializedRef = useRef(false);
  const autoUpdateCheckedRef = useRef(false);
  const availableUpdateRef = useRef<Update | null>(null);
  const updateProgressRef = useRef<UpdateProgress>({ downloaded: 0 });
  const showPlaylistRef = useRef<boolean | null>(null);
  const pendingUiLayoutPatchRef = useRef<UiLayoutPreferencePatch | null>(null);
  const uiLayoutPersistTimerRef = useRef<number | null>(null);
  const uiLayoutPersistingRef = useRef(false);
  const uiLayoutLoadedRef = useRef(false);
  const [windowSizeTrackingReady, setWindowSizeTrackingReady] = useState(false);
  const RESIZER_SIZE = 12;
  const GAP_SIZE = 0;
  const MAIN_MIN_WIDTH = 360;
  const SIDE_MIN_WIDTH = 320;
  const SIDE_PANEL_MIN = 200;
  const resolveSidePanelMin = (total: number) =>
    Math.min(SIDE_PANEL_MIN, Math.max(0, Math.floor((total - GAP_SIZE) / 2)));

  const replaceAvailableUpdate = useCallback((nextUpdate: Update | null) => {
    const previousUpdate = availableUpdateRef.current;
    availableUpdateRef.current = nextUpdate;
    setUpdateVersion(nextUpdate?.version ?? null);
    if (previousUpdate && previousUpdate !== nextUpdate) {
      void previousUpdate.close().catch((error) => {
        console.warn("Failed to close updater resource", error);
      });
    }
  }, []);

  useEffect(() => {
    let active = true;
    const loadVersion = async () => {
      try {
        const version = await getVersion();
        if (active) {
          setAppVersion(version);
        }
      } catch (error) {
        console.warn("Failed to load app version", error);
        if (active) {
          setAppVersion(null);
        }
      }
    };
    void loadVersion();
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    if (initializedRef.current) return;
    initializedRef.current = true;

    const initFromConfig = async () => {
      try {
        const config = await invoke<SyncplayConfig>("get_config");
        setConfig(config);
        setShowPlaylist(config.user.show_playlist);
        const normalizedTheme = normalizeTheme(config.user.theme);
        setTheme(normalizedTheme);
        applyTheme(normalizedTheme);
        const normalizedTransparency = normalizeTransparency(config.user.transparency_mode);
        setTransparencyMode(normalizedTransparency);
        applyTransparency(normalizedTransparency);

        if (
          !autoUpdateCheckedRef.current &&
          shouldAutoCheckUpdates(config.user.check_for_updates_automatically)
        ) {
          autoUpdateCheckedRef.current = true;
          const updateResult = await checkForUpdates();
          if (updateResult.status === "available") {
            replaceAvailableUpdate(updateResult.update);
            addNotification({
              type: "info",
              message: `Update ${updateResult.update.version} available. Click Update in the header to install.`,
            });
          } else {
            replaceAvailableUpdate(null);
            if (updateResult.status === "error") {
              console.warn("Auto update check failed", updateResult.message);
            }
          }
        }

        if (config.user.force_gui_prompt) {
          setShowConnectionDialog(true);
        } else if (
          config.user.auto_connect &&
          !connection.connected &&
          config.user.username.trim()
        ) {
          try {
            await invoke("connect_to_server", {
              host: config.server.host,
              port: config.server.port,
              username: config.user.username,
              room: config.user.default_room,
              password: config.server.password || null,
            });
          } catch (error) {
            addNotification({
              type: "error",
              message: "Auto-connect failed",
            });
          }
        }
      } catch (error) {
        addNotification({
          type: "warning",
          message: "Failed to load config for auto-connect",
        });
      }
    };

    initFromConfig();
  }, [connection.connected, addNotification, replaceAvailableUpdate, setConfig]);

  useEffect(() => {
    return () => {
      if (availableUpdateRef.current) {
        void availableUpdateRef.current.close().catch((error) => {
          console.warn("Failed to close updater resource", error);
        });
        availableUpdateRef.current = null;
      }
    };
  }, []);

  const flushUiLayoutPatch = useCallback(async () => {
    if (uiLayoutPersistingRef.current) {
      return;
    }

    const patch = pendingUiLayoutPatchRef.current;
    if (!patch) {
      return;
    }

    pendingUiLayoutPatchRef.current = null;
    uiLayoutPersistingRef.current = true;

    try {
      const currentConfig = await invoke<SyncplayConfig>("get_config");
      const nextConfig: SyncplayConfig = {
        ...currentConfig,
        user: { ...currentConfig.user, ...patch },
      };
      await invoke("update_config", { config: nextConfig });
      setConfig(nextConfig);
    } catch (error) {
      console.warn("Failed to persist UI layout preferences", error);
    } finally {
      uiLayoutPersistingRef.current = false;
      if (pendingUiLayoutPatchRef.current) {
        uiLayoutPersistTimerRef.current = window.setTimeout(() => {
          uiLayoutPersistTimerRef.current = null;
          void flushUiLayoutPatch();
        }, 200);
      }
    }
  }, [setConfig]);

  const queueUiLayoutPatch = useCallback(
    (patch: UiLayoutPreferencePatch, debounceMs = 300) => {
      pendingUiLayoutPatchRef.current = {
        ...(pendingUiLayoutPatchRef.current ?? {}),
        ...patch,
      };

      if (uiLayoutPersistTimerRef.current !== null) {
        window.clearTimeout(uiLayoutPersistTimerRef.current);
      }

      uiLayoutPersistTimerRef.current = window.setTimeout(() => {
        uiLayoutPersistTimerRef.current = null;
        void flushUiLayoutPatch();
      }, debounceMs);
    },
    [flushUiLayoutPatch]
  );

  useEffect(() => {
    return () => {
      if (uiLayoutPersistTimerRef.current !== null) {
        window.clearTimeout(uiLayoutPersistTimerRef.current);
        uiLayoutPersistTimerRef.current = null;
      }
      if (pendingUiLayoutPatchRef.current) {
        void flushUiLayoutPatch();
      }
    };
  }, [flushUiLayoutPatch]);

  useEffect(() => {
    if (!config) return;
    if (showPlaylistRef.current !== config.user.show_playlist) {
      showPlaylistRef.current = config.user.show_playlist;
      setShowPlaylist(config.user.show_playlist);
    }
    if (config.user.side_panel_layout) {
      setSideLayout(config.user.side_panel_layout);
    }
    const normalizedTheme = normalizeTheme(config.user.theme);
    setTheme(normalizedTheme);
    applyTheme(normalizedTheme);
    const normalizedTransparency = normalizeTransparency(config.user.transparency_mode);
    setTransparencyMode(normalizedTransparency);
    applyTransparency(normalizedTransparency);
    if (!uiLayoutLoadedRef.current) {
      uiLayoutLoadedRef.current = true;
      if (typeof config.user.side_column_width === "number" && config.user.side_column_width > 0) {
        setSideWidth(config.user.side_column_width);
      }
      if (
        typeof config.user.side_panel_primary_size === "number" &&
        config.user.side_panel_primary_size > 0
      ) {
        setSidePanelSize(config.user.side_panel_primary_size);
      }
    }
  }, [config]);

  useEffect(() => {
    if (!appWindow || !config || windowSizeTrackingReady) return;

    const restoreWindowSize = async () => {
      try {
        const width = config.user.window_width;
        const height = config.user.window_height;
        if (typeof width === "number" && typeof height === "number" && width > 0 && height > 0) {
          await appWindow.setSize(new PhysicalSize(width, height));
        }
      } catch (error) {
        console.warn("Failed to restore window size", error);
      } finally {
        setWindowSizeTrackingReady(true);
      }
    };

    void restoreWindowSize();
  }, [appWindow, config, windowSizeTrackingReady]);

  useEffect(() => {
    if (!appWindow || !windowSizeTrackingReady) return;

    let mounted = true;
    let unlisten: (() => void) | null = null;

    const setupResizeListener = async () => {
      try {
        unlisten = await appWindow.onResized(({ payload: size }) => {
          if (!mounted) return;
          const width = Math.round(size.width);
          const height = Math.round(size.height);
          queueUiLayoutPatch(
            {
              window_width: width,
              window_height: height,
            },
            450
          );
        });
      } catch (error) {
        console.warn("Failed to listen for window resize", error);
      }
    };

    void setupResizeListener();

    return () => {
      mounted = false;
      if (unlisten) {
        unlisten();
      }
    };
  }, [appWindow, queueUiLayoutPatch, windowSizeTrackingReady]);

  useEffect(() => {
    if (!layoutRef.current) return;
    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (!entry) return;
      const { width, height } = entry.contentRect;
      setLayoutSize({ width, height });
    });
    observer.observe(layoutRef.current);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (!sidePanelsRef.current) return;
    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (!entry) return;
      const { width, height } = entry.contentRect;
      setSidePanelsSize({ width, height });
    });
    observer.observe(sidePanelsRef.current);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (!layoutSize.width) return;
    setSideWidth((previous) => {
      const min = SIDE_MIN_WIDTH;
      const max = Math.max(min, layoutSize.width - MAIN_MIN_WIDTH - GAP_SIZE);
      const fallback = Math.round(Math.min(560, Math.max(min, layoutSize.width * 0.36)));
      const next = previous ?? fallback;
      return Math.min(Math.max(next, min), max);
    });
  }, [layoutSize.width]);

  useEffect(() => {
    if (!showPlaylist) return;
    const total = sideLayout === "rows" ? sidePanelsSize.height : sidePanelsSize.width;
    if (!total) return;
    setSidePanelSize((previous) => {
      const min = resolveSidePanelMin(total);
      const max = Math.max(min, total - min - GAP_SIZE);
      const fallback = Math.round(total / 2);
      const next = previous ?? fallback;
      return Math.min(Math.max(next, min), max);
    });
  }, [sidePanelsSize.height, sidePanelsSize.width, sideLayout, showPlaylist]);

  const handleToggleTheme = async () => {
    const previousTheme = theme;
    const nextTheme = theme === "light" ? "dark" : "light";
    setTheme(nextTheme);
    applyTheme(nextTheme);

    try {
      const config = await invoke<SyncplayConfig>("get_config");
      await invoke("update_config", {
        config: {
          ...config,
          user: { ...config.user, theme: nextTheme },
        },
      });
    } catch (error) {
      setTheme(previousTheme);
      applyTheme(previousTheme);
      addNotification({
        type: "error",
        message: "Failed to save theme",
      });
    }
  };

  const handleToggleTransparency = async () => {
    const nextMode =
      transparencyMode === "off" ? "low" : transparencyMode === "low" ? "high" : "off";
    setTransparencyMode(nextMode);
    applyTransparency(nextMode);

    try {
      const config = await invoke<SyncplayConfig>("get_config");
      await invoke("update_config", {
        config: {
          ...config,
          user: { ...config.user, transparency_mode: nextMode },
        },
      });
    } catch (error) {
      setTransparencyMode(transparencyMode);
      applyTransparency(transparencyMode);
      addNotification({
        type: "error",
        message: "Failed to save transparency",
      });
    }
  };

  const handleTogglePlaylist = () => {
    setShowPlaylist((prev) => {
      const next = !prev;
      void (async () => {
        try {
          const config = await invoke<SyncplayConfig>("get_config");
          await invoke("update_config", {
            config: {
              ...config,
              user: { ...config.user, show_playlist: next },
            },
          });
        } catch (error) {
          setShowPlaylist(prev);
          addNotification({
            type: "error",
            message: "Failed to save playlist visibility",
          });
        }
      })();
      return next;
    });
  };

  const handleHeaderMouseDown = (event: React.MouseEvent) => {
    if (event.button !== 0) return;
    const target = event.target as HTMLElement;
    if (target.closest('[data-tauri-drag-region="false"]')) return;
    if (!appWindow) return;
    void appWindow.startDragging();
  };
  useWindowDrag("titlebar");
  useWindowDrag("toolbar-drag");

  const showTls = connection.connected && tlsStatus === "enabled";
  const formatRtt = (value: number | null) => {
    if (value === null || Number.isNaN(value)) return "";
    const rounded = Math.max(0, Math.round(value));
    return `${rounded}ms`;
  };
  const rttLabel = formatRtt(rttMs);
  const clampValue = (value: number, min: number, max: number) =>
    Math.min(Math.max(value, min), max);
  const layoutMainWidth =
    sideWidth && layoutSize.width
      ? Math.max(MAIN_MIN_WIDTH, layoutSize.width - sideWidth - GAP_SIZE)
      : null;
  const sidePanelPrimarySize = showPlaylist && sidePanelSize !== null ? sidePanelSize : null;
  const sidePanelTotal = sideLayout === "rows" ? sidePanelsSize.height : sidePanelsSize.width;
  const sidePanelMin = resolveSidePanelMin(sidePanelTotal);
  const sidePanelSecondarySize =
    sidePanelPrimarySize !== null && sidePanelTotal
      ? Math.max(sidePanelMin, sidePanelTotal - sidePanelPrimarySize - GAP_SIZE)
      : null;
  const sidePanelFallback =
    showPlaylist && sideLayout === "columns"
      ? `minmax(0, 1fr) minmax(0, 1fr)`
      : showPlaylist && sideLayout === "rows"
        ? `minmax(0, 1fr) minmax(0, 1fr)`
        : "minmax(0, 1fr)";
  const mainResizerStyle =
    layoutMainWidth !== null
      ? {
          left: `${layoutMainWidth + GAP_SIZE / 2}px`,
          top: 0,
          height: "100%",
          width: `${RESIZER_SIZE}px`,
          transform: "translateX(-50%)",
        }
      : undefined;
  const sideResizerStyle =
    showPlaylist && sidePanelPrimarySize !== null
      ? sideLayout === "rows"
        ? {
            top: `${sidePanelPrimarySize + GAP_SIZE / 2}px`,
            left: 0,
            width: "100%",
            height: `${RESIZER_SIZE}px`,
            transform: "translateY(-50%)",
          }
        : {
            left: `${sidePanelPrimarySize + GAP_SIZE / 2}px`,
            top: 0,
            width: `${RESIZER_SIZE}px`,
            height: "100%",
            transform: "translateX(-50%)",
          }
      : undefined;

  const handleMainResizeStart = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!layoutRef.current || sideWidth === null) return;
    event.preventDefault();
    const startX = event.clientX;
    const startSideWidth = sideWidth;
    let latestWidth = startSideWidth;
    const rect = layoutRef.current.getBoundingClientRect();
    const min = SIDE_MIN_WIDTH;
    const max = Math.max(min, rect.width - MAIN_MIN_WIDTH - GAP_SIZE);

    const handlePointerMove = (moveEvent: PointerEvent) => {
      const delta = moveEvent.clientX - startX;
      const nextWidth = clampValue(startSideWidth - delta, min, max);
      latestWidth = nextWidth;
      setSideWidth(nextWidth);
    };

    const handlePointerUp = () => {
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", handlePointerUp);
      queueUiLayoutPatch(
        {
          side_column_width: Math.round(latestWidth),
        },
        120
      );
    };

    window.addEventListener("pointermove", handlePointerMove);
    window.addEventListener("pointerup", handlePointerUp);
  };

  const handleSideResizeStart = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!sidePanelsRef.current || sidePanelPrimarySize === null) return;
    event.preventDefault();
    const isRows = sideLayout === "rows";
    const startOffset = isRows ? event.clientY : event.clientX;
    const startSize = sidePanelPrimarySize;
    let latestSize = startSize;
    const rect = sidePanelsRef.current.getBoundingClientRect();
    const total = isRows ? rect.height : rect.width;
    const min = resolveSidePanelMin(total);
    const max = Math.max(min, total - min - GAP_SIZE);

    const handlePointerMove = (moveEvent: PointerEvent) => {
      const currentOffset = isRows ? moveEvent.clientY : moveEvent.clientX;
      const delta = currentOffset - startOffset;
      const nextSize = clampValue(startSize + delta, min, max);
      latestSize = nextSize;
      setSidePanelSize(nextSize);
    };

    const handlePointerUp = () => {
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", handlePointerUp);
      queueUiLayoutPatch(
        {
          side_panel_primary_size: Math.round(latestSize),
        },
        120
      );
    };

    window.addEventListener("pointermove", handlePointerMove);
    window.addEventListener("pointerup", handlePointerUp);
  };

  const handleOpenSettings = (
    initialTab?: "sync" | "ready" | "privacy" | "chat" | "osd" | "misc"
  ) => {
    setSettingsInitialTab(initialTab);
    setShowSettingsDialog(true);
  };

  const handleInstallHeaderUpdate = async () => {
    const availableUpdate = availableUpdateRef.current;
    if (!availableUpdate) {
      handleOpenSettings("misc");
      return;
    }

    setIsInstallingUpdate(true);
    updateProgressRef.current = { downloaded: 0 };
    setUpdateProgress({ downloaded: 0 });

    try {
      await downloadAndInstallUpdate(availableUpdate, (event: DownloadEvent) => {
        updateProgressRef.current = nextUpdateProgress(updateProgressRef.current, event);
        setUpdateProgress({ ...updateProgressRef.current });
      });
      replaceAvailableUpdate(null);
    } catch (error) {
      addNotification({
        type: "error",
        message: `Update failed: ${formatUpdateError(error)}`,
      });
      setIsInstallingUpdate(false);
      setUpdateProgress(null);
    }
  };

  const handleSettingsUpdateAvailable = (version: string | null) => {
    const availableUpdate = availableUpdateRef.current;
    if (!version) {
      replaceAvailableUpdate(null);
      return;
    }
    if (availableUpdate?.version === version) {
      setUpdateVersion(version);
      return;
    }
    replaceAvailableUpdate(null);
    setUpdateVersion(version);
  };

  const handleCloseSettings = () => {
    setShowSettingsDialog(false);
    setSettingsInitialTab(undefined);
  };

  return (
    <div className="app-shell">
      <NotificationContainer />
      <div className="drag-strip" id="titlebar" data-tauri-drag-region />

      <div
        className="app-layout"
        ref={layoutRef}
        style={{
          gridTemplateColumns:
            layoutMainWidth && sideWidth ? `${layoutMainWidth}px ${sideWidth}px` : undefined,
        }}
      >
        <section className="app-main-column">
          <main className="app-main-panel">
            <ChatPanel />
          </main>
        </section>

        <div
          className="app-resizer app-resizer-vertical app-resizer-overlay"
          role="separator"
          aria-orientation="vertical"
          onPointerDown={handleMainResizeStart}
          style={mainResizerStyle}
        />

        <section className="app-side-column">
          <header
            className="app-header relative"
            id="toolbar-drag"
            data-tauri-drag-region
            onMouseDown={handleHeaderMouseDown}
          >
            {(appVersion || updateVersion) && (
              <div
                className="absolute top-2 flex items-center gap-2 app-header-actions"
                data-tauri-drag-region="false"
                style={{
                  right: "calc(16px + var(--tauri-frame-controls-width, 0px))",
                }}
              >
                {appVersion && (
                  <div
                    className="text-xs app-tag-muted px-2.5 py-1 rounded-full"
                    aria-label={`Version ${appVersion}`}
                    title={`Version ${appVersion}`}
                  >
                    v{appVersion}
                  </div>
                )}
                {updateVersion && (
                  <button
                    onClick={handleInstallHeaderUpdate}
                    disabled={isInstallingUpdate}
                    className="btn-primary px-2.5 py-1 rounded-full text-xs"
                    aria-label={`Update available: ${updateVersion}`}
                    title={
                      isInstallingUpdate && updateProgress
                        ? `Installing ${updateVersion}: ${formatBytes(updateProgress.downloaded)}${
                            updateProgress.total ? ` / ${formatBytes(updateProgress.total)}` : ""
                          }`
                        : `Install update ${updateVersion}`
                    }
                  >
                    {isInstallingUpdate ? "Installing..." : "Update"}
                  </button>
                )}
              </div>
            )}
            <div className="app-header-row">
              <PlayerStatus />
              <div className="app-header-actions w-full" data-tauri-drag-region="false">
                <div className="flex items-center gap-2">
                  <button
                    onClick={handleTogglePlaylist}
                    className="btn-neutral app-icon-button"
                    data-tauri-drag-region="false"
                    aria-label={showPlaylist ? "Playlist shown" : "Playlist hidden"}
                  >
                    {showPlaylist ? (
                      <LuListMusic className="app-icon" />
                    ) : (
                      <LuListMinus className="app-icon" />
                    )}
                  </button>
                  <button
                    onClick={() =>
                      setSideLayout((prev) => {
                        const next = prev === "columns" ? "rows" : "columns";
                        void (async () => {
                          try {
                            const config = await invoke<SyncplayConfig>("get_config");
                            await invoke("update_config", {
                              config: {
                                ...config,
                                user: { ...config.user, side_panel_layout: next },
                              },
                            });
                          } catch (error) {
                            setSideLayout(prev);
                            addNotification({
                              type: "error",
                              message: "Failed to save layout",
                            });
                          }
                        })();
                        return next;
                      })
                    }
                    className="btn-neutral app-icon-button"
                    data-tauri-drag-region="false"
                    aria-label={sideLayout === "columns" ? "Layout split" : "Layout stacked"}
                  >
                    {sideLayout === "rows" ? (
                      <LuRows2 className="app-icon" />
                    ) : (
                      <LuColumns2 className="app-icon" />
                    )}
                  </button>
                  <button
                    onClick={handleToggleTheme}
                    className="btn-neutral app-icon-button"
                    data-tauri-drag-region="false"
                    aria-label={theme === "light" ? "Theme light" : "Theme dark"}
                  >
                    {theme === "light" ? (
                      <LuSun className="app-icon" />
                    ) : (
                      <LuMoon className="app-icon" />
                    )}
                  </button>
                  <button
                    onClick={handleToggleTransparency}
                    className="btn-neutral app-icon-button"
                    data-tauri-drag-region="false"
                    aria-label={
                      transparencyMode === "off"
                        ? "Transparency off"
                        : transparencyMode === "low"
                          ? "Transparency low"
                          : "Transparency high"
                    }
                  >
                    {transparencyMode === "off" ? (
                      <LuContrast className="app-icon" />
                    ) : transparencyMode === "low" ? (
                      <LuDroplet className="app-icon" />
                    ) : (
                      <LuDroplets className="app-icon" />
                    )}
                  </button>
                </div>
                <div className="flex items-center gap-2 ml-auto">
                  {connection.connected && rttLabel && (
                    <div
                      className="flex items-center gap-2 app-panel-muted px-2.5 py-1 rounded-full text-xs"
                      aria-label={`RTT ${rttLabel}`}
                      title={`RTT ${rttLabel}`}
                    >
                      <LuZap className="app-icon" />
                      <span className="font-mono">{rttLabel}</span>
                    </div>
                  )}
                  {showTls && (
                    <div
                      className="flex items-center justify-center px-2 py-1 rounded text-xs app-panel-muted app-tooltip"
                      aria-label="TLS enabled"
                    >
                      <LuLock className="app-icon" />
                    </div>
                  )}
                  <button
                    onClick={() => setShowConnectionDialog(true)}
                    className={`app-icon-button btn-neutral ${
                      connection.connected ? "app-tag-accent" : ""
                    }`}
                    data-tauri-drag-region="false"
                    aria-label="Connect"
                  >
                    <LuLink2 className="app-icon" />
                  </button>
                  <button
                    onClick={() => handleOpenSettings()}
                    className="btn-neutral app-icon-button app-tooltip-right"
                    data-tauri-drag-region="false"
                    aria-label="Settings"
                  >
                    <LuSettings className="app-icon" />
                  </button>
                </div>
              </div>
            </div>
          </header>

          <div
            className="app-side-panels"
            ref={sidePanelsRef}
            data-layout={sideLayout}
            style={{
              gridTemplateColumns:
                sideLayout === "columns"
                  ? showPlaylist && sidePanelPrimarySize !== null && sidePanelSecondarySize !== null
                    ? `${sidePanelPrimarySize}px ${sidePanelSecondarySize}px`
                    : sidePanelFallback
                  : "minmax(0, 1fr)",
              gridTemplateRows:
                sideLayout === "rows"
                  ? showPlaylist && sidePanelPrimarySize !== null && sidePanelSecondarySize !== null
                    ? `${sidePanelPrimarySize}px ${sidePanelSecondarySize}px`
                    : sidePanelFallback
                  : "minmax(0, 1fr)",
            }}
          >
            <aside className="app-side-panel app-sidebar p-5 overflow-visible">
              <UserList />
            </aside>

            {showPlaylist && (
              <div
                className={`app-resizer app-resizer-overlay ${
                  sideLayout === "rows" ? "app-resizer-horizontal" : "app-resizer-vertical"
                }`}
                role="separator"
                aria-orientation={sideLayout === "rows" ? "horizontal" : "vertical"}
                onPointerDown={handleSideResizeStart}
                style={sideResizerStyle}
              />
            )}

            {showPlaylist && (
              <aside className="app-side-panel app-sidebar-right overflow-visible">
                <PlaylistPanel />
              </aside>
            )}
          </div>
        </section>
      </div>

      {/* Connection dialog */}
      <ConnectionDialog
        isOpen={showConnectionDialog}
        onClose={() => setShowConnectionDialog(false)}
      />

      {/* Settings dialog */}
      <SettingsDialog
        isOpen={showSettingsDialog}
        onClose={handleCloseSettings}
        initialTab={settingsInitialTab}
        appVersion={appVersion}
        onUpdateAvailable={handleSettingsUpdateAvailable}
      />
    </div>
  );
}
