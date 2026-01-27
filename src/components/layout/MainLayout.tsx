import { useEffect, useRef, useState } from "react";
import { UserList } from "../users/UserList";
import { ChatPanel } from "../chat/ChatPanel";
import { PlayerStatus } from "../player/PlayerStatus";
import { PlaylistPanel } from "../playlist/PlaylistPanel";
import { ConnectionDialog } from "../connection/ConnectionDialog";
import { SettingsDialog } from "../settings/SettingsDialog";
import { NotificationContainer } from "../notifications/NotificationContainer";
import { useSyncplayStore } from "../../store";
import { useNotificationStore } from "../../store/notifications";
import { invoke } from "@tauri-apps/api/core";
import { applyTheme } from "../../services/theme";

interface SyncplayConfig {
  server: {
    host: string;
    port: number;
    password: string | null;
  };
  user: {
    username: string;
    default_room: string;
    show_playlist: boolean;
    auto_connect: boolean;
    theme: string;
  };
}

export function MainLayout() {
  const [showConnectionDialog, setShowConnectionDialog] = useState(false);
  const [showSettingsDialog, setShowSettingsDialog] = useState(false);
  const [showPlaylist, setShowPlaylist] = useState(true);
  const connection = useSyncplayStore((state) => state.connection);
  const addNotification = useNotificationStore((state) => state.addNotification);
  const initializedRef = useRef(false);

  useEffect(() => {
    if (initializedRef.current) return;
    initializedRef.current = true;

    const initFromConfig = async () => {
      try {
        const config = await invoke<SyncplayConfig>("get_config");
        setShowPlaylist(config.user.show_playlist);
        applyTheme(config.user.theme);

        if (config.user.auto_connect && !connection.connected && config.user.username.trim()) {
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
  }, [connection.connected, addNotification]);

  return (
    <div className="flex flex-col h-screen app-shell">
      <NotificationContainer />
      {/* Header */}
      <header className="app-header px-6 py-4 backdrop-blur">
        <div className="flex items-center justify-between">
          <h1 className="text-xl font-bold">Syncplay</h1>

          <div className="flex items-center gap-4">
            <PlayerStatus />

            <div className="flex gap-2">
              <button
                onClick={() => setShowPlaylist(!showPlaylist)}
                className="btn-neutral px-3 py-1.5 rounded-md text-sm"
              >
                {showPlaylist ? "Hide" : "Show"} Playlist
              </button>
              <button
                onClick={() => setShowSettingsDialog(true)}
                className="btn-neutral px-3 py-1.5 rounded-md text-sm"
              >
                Settings
              </button>
              <button
                onClick={() => setShowConnectionDialog(true)}
                className="bg-blue-600 hover:bg-blue-500 text-white px-4 py-1.5 rounded-md text-sm"
              >
                Connect
              </button>
            </div>
          </div>
        </div>
      </header>

      {/* Main content */}
      <div className="flex flex-1 overflow-hidden">
        {/* Users sidebar */}
        <aside className="w-64 app-sidebar p-5 overflow-auto">
          <UserList />
        </aside>

        {/* Chat area */}
        <main className="flex-1 flex flex-col">
          <ChatPanel />
        </main>

        {/* Playlist sidebar */}
        {showPlaylist && (
          <aside className="w-80">
            <PlaylistPanel />
          </aside>
        )}
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
