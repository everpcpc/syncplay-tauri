import { useState } from "react";
import { UserList } from "../users/UserList";
import { ChatPanel } from "../chat/ChatPanel";
import { PlayerStatus } from "../player/PlayerStatus";
import { PlaylistPanel } from "../playlist/PlaylistPanel";
import { ConnectionDialog } from "../connection/ConnectionDialog";

export function MainLayout() {
  const [showConnectionDialog, setShowConnectionDialog] = useState(false);
  const [showPlaylist, setShowPlaylist] = useState(true);

  return (
    <div className="flex flex-col h-screen bg-gray-900 text-white">
      {/* Header */}
      <header className="bg-gray-800 p-4 border-b border-gray-700">
        <div className="flex items-center justify-between">
          <h1 className="text-xl font-bold">Syncplay</h1>

          <div className="flex items-center gap-4">
            <PlayerStatus />

            <div className="flex gap-2">
              <button
                onClick={() => setShowPlaylist(!showPlaylist)}
                className="bg-gray-700 hover:bg-gray-600 text-white px-3 py-1 rounded text-sm"
              >
                {showPlaylist ? "Hide" : "Show"} Playlist
              </button>
              <button
                onClick={() => setShowConnectionDialog(true)}
                className="bg-blue-600 hover:bg-blue-700 text-white px-4 py-1 rounded text-sm"
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
        <aside className="w-64 bg-gray-800 border-r border-gray-700 p-4 overflow-auto">
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
    </div>
  );
}
