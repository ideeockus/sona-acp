import type { AgentStatus } from "./types";

export type AgentLook = "running" | "idle" | "stuck" | "disabled";

/** One word for the state, the same on every screen. */
export function look(status: AgentStatus | undefined): AgentLook {
  if (!status) return "idle";
  if (status.state === "disabled") return "disabled";
  if (status.failing.some((f) => f.stuck)) return "stuck";
  if (status.run) return "running";
  return "idle";
}
