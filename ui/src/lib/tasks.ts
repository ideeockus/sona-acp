import type { TaskStatus } from "./types";

export const STATUSES: TaskStatus[] = ["needs_human", "open", "in_progress", "done", "cancelled"];

export function statusLabel(status: TaskStatus): string {
  return status.replace("_", " ");
}
