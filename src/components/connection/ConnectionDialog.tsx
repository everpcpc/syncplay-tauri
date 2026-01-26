import { useState } from "react";
import { useSyncplayStore } from "../../store";
import { invoke } from "@tauri-apps/api";

interface ConnectionDialogProps {
  isOpen: boolean;
  onClose: () => void;
}

export function ConnectionDialog({ isOpen, onClose }: ConnectionDialogProps) {
  const connection = useSyncplayStore((state) => state.connection);
  const [formData, setFormData] = useState({
    host: "syncplay.pl",
    port: 8999,
    username: "",
    room: "default",
    password: "",
  });
  const [isConnecting, setIsConnecting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (!isOpen) return null;

  const handleConnect = async () => {
    if (!formData.username.trim()) {
      setError("Username is required");
      return;
    }

    setIsConnecting(true);
    setError(null);

    try {
      await invoke("connect_to_server", {
        host: formData.host,
        port: formData.port,
        username: formData.username,
        room: formData.room,
        password: formData.password || null,
      });
      onClose();
    } catch (err) {
      setError(err as string);
    } finally {
      setIsConnecting(false);
    }
  };

  const handleDisconnect = async () => {
    try {
      await invoke("disconnect_from_server");
      onClose();
    } catch (err) {
      setError(err as string);
    }
  };

  return (
    <div className="fixed inset-0 bg-black bg-opacity-50 flex items-center justify-center z-50">
      <div className="bg-gray-800 rounded-lg p-6 w-full max-w-md">
        <h2 className="text-xl font-bold mb-4">
          {connection.connected ? "Connected" : "Connect to Server"}
        </h2>

        {connection.connected ? (
          <div className="space-y-4">
            <div className="bg-gray-700 p-4 rounded">
              <p className="text-sm text-gray-300">
                Connected to: <span className="text-white font-medium">{connection.server}</span>
              </p>
            </div>

            <div className="flex gap-2">
              <button
                onClick={handleDisconnect}
                className="flex-1 bg-red-600 hover:bg-red-700 text-white px-4 py-2 rounded"
              >
                Disconnect
              </button>
              <button
                onClick={onClose}
                className="flex-1 bg-gray-700 hover:bg-gray-600 text-white px-4 py-2 rounded"
              >
                Close
              </button>
            </div>
          </div>
        ) : (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              handleConnect();
            }}
            className="space-y-4"
          >
            <div>
              <label className="block text-sm font-medium mb-1">Server</label>
              <input
                type="text"
                value={formData.host}
                onChange={(e) =>
                  setFormData({ ...formData, host: e.target.value })
                }
                className="w-full bg-gray-700 text-white px-3 py-2 rounded border border-gray-600 focus:outline-none focus:border-blue-500"
                placeholder="syncplay.pl"
              />
            </div>

            <div>
              <label className="block text-sm font-medium mb-1">Port</label>
              <input
                type="number"
                value={formData.port}
                onChange={(e) =>
                  setFormData({ ...formData, port: parseInt(e.target.value) })
                }
                className="w-full bg-gray-700 text-white px-3 py-2 rounded border border-gray-600 focus:outline-none focus:border-blue-500"
                placeholder="8999"
              />
            </div>

            <div>
              <label className="block text-sm font-medium mb-1">
                Username *
              </label>
              <input
                type="text"
                value={formData.username}
                onChange={(e) =>
                  setFormData({ ...formData, username: e.target.value })
                }
                className="w-full bg-gray-700 text-white px-3 py-2 rounded border border-gray-600 focus:outline-none focus:border-blue-500"
                placeholder="Your username"
                required
              />
            </div>

            <div>
              <label className="block text-sm font-medium mb-1">Room</label>
              <input
                type="text"
                value={formData.room}
                onChange={(e) =>
                  setFormData({ ...formData, room: e.target.value })
                }
                className="w-full bg-gray-700 text-white px-3 py-2 rounded border border-gray-600 focus:outline-none focus:border-blue-500"
                placeholder="default"
              />
            </div>

            <div>
              <label className="block text-sm font-medium mb-1">
                Password (optional)
              </label>
              <input
                type="password"
                value={formData.password}
                onChange={(e) =>
                  setFormData({ ...formData, password: e.target.value })
                }
                className="w-full bg-gray-700 text-white px-3 py-2 rounded border border-gray-600 focus:outline-none focus:border-blue-500"
                placeholder="Server password"
              />
            </div>

            {error && (
              <div className="bg-red-900 border border-red-700 text-red-200 px-4 py-2 rounded text-sm">
                {error}
              </div>
            )}

            <div className="flex gap-2">
              <button
                type="submit"
                disabled={isConnecting}
                className="flex-1 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-600 disabled:cursor-not-allowed text-white px-4 py-2 rounded"
              >
                {isConnecting ? "Connecting..." : "Connect"}
              </button>
              <button
                type="button"
                onClick={onClose}
                className="flex-1 bg-gray-700 hover:bg-gray-600 text-white px-4 py-2 rounded"
              >
                Cancel
              </button>
            </div>
          </form>
        )}
      </div>
    </div>
  );
}
