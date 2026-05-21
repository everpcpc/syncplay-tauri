import { LuPause, LuPlay } from "react-icons/lu";
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
  if (!filename) return "No file loaded";
  return filename.split(/[/\\]/).pop() || filename;
};

const formatSpeed = (speed: number | null) => {
  if (speed === null || speed === 1.0) return "";
  return `${speed.toFixed(2)}x`;
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

  const speedLabel = formatSpeed(player.speed);

  return (
    <div className="flex flex-wrap items-center gap-3 text-sm min-w-0">
      <button
        type="button"
        disabled
        className="btn-neutral app-icon-button disabled:opacity-100 disabled:cursor-default shrink-0"
        aria-label={player.paused ? "Paused" : "Playing"}
        title={player.paused ? "Paused" : "Playing"}
      >
        {player.paused ? (
          <LuPause className="app-icon app-text-warning" />
        ) : (
          <LuPlay className="app-icon" />
        )}
      </button>
      {player.position !== null && player.duration !== null && (
        <span className="font-mono text-xs shrink-0">
          {formatTime(player.position)}/{formatTime(player.duration)}
        </span>
      )}
      <span className="font-medium truncate max-w-[28vw]" title={player.filename ?? undefined}>
        {displayFilename(player.filename)}
      </span>
      {speedLabel && <span className="app-text-warning shrink-0">{speedLabel}</span>}
    </div>
  );
}
