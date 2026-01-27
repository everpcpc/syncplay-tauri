import { useSyncplayStore } from "../../store";

export function UserList() {
  const users = useSyncplayStore((state) => state.users);
  const connection = useSyncplayStore((state) => state.connection);

  if (!connection.connected) {
    return (
      <div className="space-y-2">
        <h2 className="text-lg font-semibold mb-4">Users</h2>
        <p className="app-text-muted text-sm">Not connected</p>
      </div>
    );
  }

  return (
    <div className="space-y-2">
      <h2 className="text-lg font-semibold mb-4">Users ({users.length})</h2>

      {users.length === 0 ? (
        <p className="app-text-muted text-sm">No users in room</p>
      ) : (
        <div className="space-y-2">
          {users.map((user) => (
            <div key={user.username} className="app-panel-muted rounded-md p-3 text-sm">
              <div className="flex items-center justify-between">
                <span className="font-medium">{user.username}</span>
                {user.isController && (
                  <span className="text-xs bg-blue-600 px-2 py-0.5 rounded">Controller</span>
                )}
              </div>

              <div className="text-xs app-text-muted mt-1">Room: {user.room}</div>

              {user.file && (
                <div className="text-xs app-text-muted mt-1 truncate">File: {user.file}</div>
              )}

              <div className="flex items-center gap-2 mt-1">
                <span
                  className={`text-xs px-2 py-0.5 rounded ${
                    user.isReady ? "bg-green-600 text-white" : "app-chip-muted"
                  }`}
                >
                  {user.isReady ? "Ready" : "Not Ready"}
                </span>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
