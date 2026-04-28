import { isTauri } from "@tauri-apps/api/core";
import { check, type DownloadEvent, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

export interface UpdateProgress {
  downloaded: number;
  total?: number;
}

export type UpdateCheckResult =
  | { status: "unsupported" }
  | { status: "up-to-date" }
  | { status: "available"; update: Update }
  | { status: "error"; message: string };

export const formatUpdateError = (error: unknown) => {
  if (error instanceof Error) {
    return error.message;
  }
  if (typeof error === "string") {
    return error;
  }
  try {
    return JSON.stringify(error);
  } catch (stringifyError) {
    console.warn("Failed to stringify update error", stringifyError);
    return "Unknown update error";
  }
};

export const shouldAutoCheckUpdates = (value: boolean | null | undefined) => value !== false;

export const formatBytes = (value: number) => {
  if (!Number.isFinite(value) || value <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  let size = value;
  let index = 0;
  while (size >= 1024 && index < units.length - 1) {
    size /= 1024;
    index += 1;
  }
  return `${size.toFixed(size >= 10 || index === 0 ? 0 : 1)} ${units[index]}`;
};

export const nextUpdateProgress = (
  current: UpdateProgress,
  event: DownloadEvent
): UpdateProgress => {
  if (event.event === "Started") {
    return {
      downloaded: 0,
      total: event.data.contentLength,
    };
  }
  if (event.event === "Progress") {
    return {
      ...current,
      downloaded: current.downloaded + event.data.chunkLength,
    };
  }
  if (event.event === "Finished") {
    return {
      ...current,
      total: current.total ?? current.downloaded,
    };
  }
  return current;
};

export const checkForUpdates = async (): Promise<UpdateCheckResult> => {
  if (!isTauri()) {
    return { status: "unsupported" };
  }
  try {
    const update = await check();
    if (!update) {
      return { status: "up-to-date" };
    }
    return { status: "available", update };
  } catch (error) {
    return { status: "error", message: formatUpdateError(error) };
  }
};

export const downloadAndInstallUpdate = async (
  update: Update,
  onEvent?: (event: DownloadEvent) => void
) => {
  await update.downloadAndInstall(onEvent);
  await relaunch();
};
