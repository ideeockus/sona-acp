import type { RunRecord } from "./types";

/** The API returns the newest 100 Runs; a sum over a full page may be short. */
export const RUNS_PAGE = 100;

export function startOfToday(): number {
  const d = new Date();
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

export interface Spend {
  runs: number;
  cost: number;
  failed: number;
  /** More Runs may lie beyond the page. */
  partial: boolean;
}

export function spendSince(runs: RunRecord[], since: number): Spend {
  const within = runs.filter((r) => Date.parse(r.started_at) >= since);
  const oldest = runs.at(-1);
  return {
    runs: within.length,
    cost: within.reduce((sum, r) => sum + r.usage.cost_micros, 0),
    failed: within.filter((r) => r.outcome === "error").length,
    partial: runs.length >= RUNS_PAGE && oldest !== undefined && Date.parse(oldest.started_at) >= since,
  };
}

export function took(run: RunRecord, now = Date.now()): string {
  const end = run.finished_at ? Date.parse(run.finished_at) : now;
  const seconds = Math.max(0, Math.round((end - Date.parse(run.started_at)) / 1000));
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return `${Math.round(seconds / 60)}m`;
  return `${Math.floor(seconds / 3600)}h ${Math.round((seconds % 3600) / 60)}m`;
}
