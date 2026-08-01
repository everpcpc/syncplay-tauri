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

const SYNC_THRESHOLD_SECONDS = 1;

const formatSyncOffset = (offsetSeconds: number) => {
  const absolute = Math.abs(offsetSeconds);
  const label = absolute >= 10 ? absolute.toFixed(0) : absolute.toFixed(1);
  return offsetSeconds < 0 ? `behind ${label}s` : `ahead ${label}s`;
};

export function PlayerStatus() {
  const connection = useSyncplayStore((state) => state.connection);
  const player = useSyncplayStore((state) => state.player);
  const syncOffsetSeconds = useSyncplayStore((state) => state.syncOffsetSeconds);

  if (!connection.connected) {
    return (
      <div className="flex items-center gap-4 text-sm app-text-muted">
        <span>Not connected</span>
      </div>
    );
  }

  const speedLabel = formatSpeed(player.speed);
  const playbackStateLabel = player.paused ? "Paused" : "Playing";
  const showSyncOffset = player.filename !== null && syncOffsetSeconds !== null;
  const inSync =
    showSyncOffset && Math.abs(syncOffsetSeconds) < SYNC_THRESHOLD_SECONDS;

  return (
    <div className="flex flex-nowrap items-center gap-3 text-sm min-w-0 overflow-hidden">
      {/* Playback happens in the external player, so this is a read-only
          status indicator rather than a clickable button. */}
      <div
        className="app-tooltip flex items-center justify-center w-8 h-8 shrink-0"
        role="status"
        aria-label={`${playbackStateLabel} (controlled in the external player)`}
      >
        {player.paused ? (
          <LuPause className="app-icon app-text-warning" />
        ) : (
          <LuPlay className="app-icon app-text-accent" />
        )}
      </div>
      {player.position !== null && player.duration !== null && (
        <span className="font-mono text-xs shrink-0">
          {formatTime(player.position)}/{formatTime(player.duration)}
        </span>
      )}
      <span
        className="font-medium truncate min-w-0 max-w-[28vw]"
        title={player.filename ?? undefined}
      >
        {displayFilename(player.filename)}
      </span>
      {showSyncOffset && (
        <span
          className={`app-tooltip-text text-[10px] px-2 py-0 rounded-full shrink-0 ${
            inSync ? "app-tag-success" : "app-tag-muted app-text-warning"
          }`}
          aria-label={
            inSync
              ? "In sync with the room"
              : `Your playback is ${formatSyncOffset(syncOffsetSeconds)} relative to the room`
          }
        >
          {inSync ? "in sync" : formatSyncOffset(syncOffsetSeconds)}
        </span>
      )}
      {speedLabel && <span className="app-text-warning shrink-0">{speedLabel}</span>}
    </div>
  );
}
