import { useSyncplayStore } from "../../store";
import { invoke } from "@tauri-apps/api";

export function PlaylistPanel() {
  const playlist = useSyncplayStore((state) => state.playlist);
  const connection = useSyncplayStore((state) => state.connection);

  const handleAddFile = async () => {
    // TODO: Implement file picker dialog
    console.log("Add file clicked");
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
    <div className="flex flex-col h-full bg-gray-800 border-l border-gray-700">
      {/* Header */}
      <div className="p-4 border-b border-gray-700">
        <h2 className="text-lg font-semibold mb-2">Playlist</h2>
        <div className="flex gap-2">
          <button
            onClick={handleAddFile}
            disabled={!connection.connected}
            className="flex-1 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-600 disabled:cursor-not-allowed text-white px-3 py-1 rounded text-sm"
          >
            Add File
          </button>
          <button
            onClick={handleClear}
            disabled={!connection.connected || playlist.items.length === 0}
            className="bg-red-600 hover:bg-red-700 disabled:bg-gray-600 disabled:cursor-not-allowed text-white px-3 py-1 rounded text-sm"
          >
            Clear
          </button>
        </div>
      </div>

      {/* Playlist items */}
      <div className="flex-1 overflow-auto p-4">
        {playlist.items.length === 0 ? (
          <p className="text-gray-400 text-sm">No items in playlist</p>
        ) : (
          <div className="space-y-2">
            {playlist.items.map((item, index) => (
              <div
                key={index}
                className={`p-2 rounded text-sm ${
                  index === playlist.currentIndex
                    ? "bg-blue-600 text-white"
                    : "bg-gray-700 text-gray-200"
                }`}
              >
                <div className="flex items-center justify-between">
                  <span className="truncate flex-1">{item}</span>
                  <button
                    onClick={() => handleRemoveItem(index)}
                    disabled={!connection.connected}
                    className="ml-2 text-red-400 hover:text-red-300 disabled:text-gray-500"
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
      <div className="p-4 border-t border-gray-700">
        <div className="flex gap-2">
          <button
            onClick={handlePrevious}
            disabled={
              !connection.connected ||
              playlist.items.length === 0 ||
              playlist.currentIndex === null ||
              playlist.currentIndex === 0
            }
            className="flex-1 bg-gray-700 hover:bg-gray-600 disabled:bg-gray-800 disabled:cursor-not-allowed text-white px-3 py-2 rounded text-sm"
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
            className="flex-1 bg-gray-700 hover:bg-gray-600 disabled:bg-gray-800 disabled:cursor-not-allowed text-white px-3 py-2 rounded text-sm"
          >
            Next →
          </button>
        </div>
      </div>
    </div>
  );
}
