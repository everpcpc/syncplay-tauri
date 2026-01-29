import { useEffect, useRef, useState } from "react";
import { UserList } from "../users/UserList";
import { ChatPanel } from "../chat/ChatPanel";
import { PlayerStatus } from "../player/PlayerStatus";
import {
  LuColumns2,
  LuContrast,
  LuLock,
  LuLockOpen,
  LuClock,
  LuDroplet,
  LuDroplets,
  LuLink2,
  LuListMinus,
  LuListMusic,
  LuMoon,
  LuRows2,
  LuSettings,
  LuSun,
} from "react-icons/lu";
import { getCurrentWindow } from "@tauri-apps/api/window";
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
import { SyncplayConfig } from "../../types/config";

export function MainLayout() {
  const appWindow = isTauri() ? getCurrentWindow() : null;
  const [showConnectionDialog, setShowConnectionDialog] = useState(false);
  const [showSettingsDialog, setShowSettingsDialog] = useState(false);
  const [showPlaylist, setShowPlaylist] = useState(true);
  const [sideLayout, setSideLayout] = useState<"columns" | "rows">("rows");
  const [theme, setTheme] = useState<ThemePreference>("dark");
  const [transparencyMode, setTransparencyMode] = useState<TransparencyPreference>("off");
  const connection = useSyncplayStore((state) => state.connection);
  const tlsStatus = useSyncplayStore((state) => state.tlsStatus);
  const config = useSyncplayStore((state) => state.config);
  const setConfig = useSyncplayStore((state) => state.setConfig);
  const addNotification = useNotificationStore((state) => state.addNotification);
  const initializedRef = useRef(false);
  const showPlaylistRef = useRef<boolean | null>(null);

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
  }, [connection.connected, addNotification, setConfig]);

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
  }, [config]);

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

  const tlsLabel =
    tlsStatus === "enabled"
      ? "TLS enabled"
      : tlsStatus === "pending"
        ? "TLS pending"
        : tlsStatus === "unsupported"
          ? "TLS unsupported"
          : "TLS unknown";
  const tlsTagClass =
    tlsStatus === "enabled"
      ? "app-tag-success"
      : tlsStatus === "pending"
        ? "app-tag-accent"
        : "app-tag-muted";
  const tlsIcon =
    tlsStatus === "enabled" ? (
      <LuLock className="app-icon" />
    ) : tlsStatus === "pending" ? (
      <LuClock className="app-icon" />
    ) : (
      <LuLockOpen className="app-icon" />
    );

  return (
    <div className="app-shell">
      <NotificationContainer />
      <div className="drag-strip" id="titlebar" data-tauri-drag-region />

      <div className="app-layout">
        <section className="app-main-column">
          <main className="app-main-panel">
            <ChatPanel />
          </main>
        </section>

        <section className="app-side-column">
          <header
            className="app-header"
            id="toolbar-drag"
            data-tauri-drag-region
            onMouseDown={handleHeaderMouseDown}
          >
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
                  {connection.connected && (
                    <div
                      className={`flex items-center justify-center px-2 py-1 rounded text-xs ${tlsTagClass}`}
                      aria-label={tlsLabel}
                      title={tlsLabel}
                    >
                      {tlsIcon}
                    </div>
                  )}
                  <button
                    onClick={() => setShowConnectionDialog(true)}
                    className="btn-primary app-icon-button"
                    data-tauri-drag-region="false"
                    aria-label="Connect"
                  >
                    <LuLink2 className="app-icon" />
                  </button>
                  <button
                    onClick={() => setShowSettingsDialog(true)}
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
            style={{
              gridTemplateColumns:
                sideLayout === "columns"
                  ? showPlaylist
                    ? "minmax(0, 1fr) minmax(0, 1fr)"
                    : "minmax(0, 1fr)"
                  : "minmax(0, 1fr)",
              gridTemplateRows:
                sideLayout === "rows"
                  ? showPlaylist
                    ? "minmax(0, 1fr) minmax(0, 1fr)"
                    : "minmax(0, 1fr)"
                  : "minmax(0, 1fr)",
            }}
          >
            <aside className="app-side-panel app-sidebar p-5 overflow-visible">
              <UserList />
            </aside>

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
      <SettingsDialog isOpen={showSettingsDialog} onClose={() => setShowSettingsDialog(false)} />
    </div>
  );
}
