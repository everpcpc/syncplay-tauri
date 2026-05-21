import { useSyncplayStore } from "../../store";

const formatTime = (seconds: number | null) => {
  if (seconds === null || !Number.isFinite(seconds) || seconds < 0) return "--:--";
  const totalSeconds = Math.floor(seconds);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const secs = totalSeconds % 60;
  const base = `${minutes.toString().padStart(2, "0")}:${secs.toString().padStart(2, "0")}`;
  return hours > 0 ? `${hours}:${base}` : base;
};

const displayFilename = (filename: string | null) => {
  if (!filename) return "No file";
  return filename.split(/[/\\]/).pop() || filename;
};

export function PlayerStatus() {
  const connection = useSyncplayStore((state) => state.connection);
  const player = useSyncplayStore((state) => state.player);

  if (!connection.connected) {
    return (
      <div className="flex items-center gap-4 text-sm app-text-muted">
        <span>Not connected</span>
      </div>
    );
  }

  const speedLabel = player.speed && player.speed !== 1 ? `${player.speed.toFixed(2)}x` : null;

  return (
    <div className="flex items-center gap-3 min-w-0 text-sm">
      <span className="app-text-accent shrink-0">{player.paused ? "Paused" : "Playing"}</span>
      <span className="truncate max-w-[28vw]" title={player.filename ?? undefined}>
        {displayFilename(player.filename)}
      </span>
      <span className="app-text-muted font-mono shrink-0">
        {formatTime(player.position)} / {formatTime(player.duration)}
      </span>
      {speedLabel && (
        <span className="app-tag-muted px-2 py-0.5 rounded shrink-0">{speedLabel}</span>
      )}
      {connection.server && (
        <span className="app-text-muted truncate max-w-[18vw]" title={connection.server}>
          {connection.server}
        </span>
      )}
    </div>
  );
}
