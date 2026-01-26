import { useSyncplayStore } from "../../store";

export function PlayerStatus() {
  const player = useSyncplayStore((state) => state.player);
  const connection = useSyncplayStore((state) => state.connection);

  const formatTime = (seconds: number | null) => {
    if (seconds === null) return "--:--";
    const mins = Math.floor(seconds / 60);
    const secs = Math.floor(seconds % 60);
    return `${mins}:${secs.toString().padStart(2, "0")}`;
  };

  const formatSpeed = (speed: number | null) => {
    if (speed === null || speed === 1.0) return "";
    return `${speed.toFixed(2)}x`;
  };

  if (!connection.connected) {
    return (
      <div className="flex items-center gap-4 text-sm text-gray-400">
        <span>Not connected</span>
      </div>
    );
  }

  return (
    <div className="flex items-center gap-4 text-sm">
      {/* Filename */}
      <div className="flex items-center gap-2">
        <span className="text-gray-400">File:</span>
        <span className="text-white font-medium truncate max-w-xs">
          {player.filename || "No file loaded"}
        </span>
      </div>

      {/* Position / Duration */}
      {player.position !== null && player.duration !== null && (
        <div className="flex items-center gap-2">
          <span className="text-gray-400">Time:</span>
          <span className="text-white font-mono">
            {formatTime(player.position)} / {formatTime(player.duration)}
          </span>
        </div>
      )}

      {/* Playback state */}
      <div className="flex items-center gap-2">
        {player.paused ? (
          <span className="text-yellow-400">⏸ Paused</span>
        ) : (
          <span className="text-green-400">▶ Playing</span>
        )}
      </div>

      {/* Speed indicator */}
      {formatSpeed(player.speed) && (
        <div className="flex items-center gap-2">
          <span className="text-orange-400">{formatSpeed(player.speed)}</span>
        </div>
      )}

      {/* Server info */}
      {connection.server && (
        <div className="flex items-center gap-2 ml-auto">
          <span className="text-gray-400">Server:</span>
          <span className="text-green-400">{connection.server}</span>
        </div>
      )}
    </div>
  );
}
