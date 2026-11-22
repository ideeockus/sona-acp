import type { Access } from "./types";

/** The groups an agent is granted, in this order everywhere (D99). */
export const ACCESS: { key: Access; label: string; tools: string; note: string }[] = [
  { key: "read_files", label: "Read files", tools: "read, ls, grep, find", note: "In its workdir and skills, inside its container." },
  { key: "write_files", label: "Write files", tools: "write, edit", note: "In its workdir only." },
  { key: "shell", label: "Shell", tools: "bash", note: "Runs anything inside its container. Gateway does not check the commands." },
  { key: "tasks", label: "Tasks", tools: "task_create, task_get, task_update", note: "Create tasks and work on those it takes part in." },
  { key: "send_messages", label: "Send messages", tools: "send_message", note: "To the agents chosen below." },
];

export function accessLabel(key: Access): string {
  return ACCESS.find((a) => a.key === key)?.label ?? key;
}
