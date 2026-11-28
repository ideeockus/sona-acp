// The API's JSON, written to match the Rust DTOs in crates/common.

export type Alias = string;
/** RFC 3339 in UTC (D73). */
export type Time = string;
/** `task-<id>` or `chain-<id>` (D78). */
export type SessionKey = string;

export interface ApiErrorBody {
  kind: "unauthenticated" | "forbidden" | "not_found" | "invalid_input" | "conflict" | "internal";
  detail?: string;
}

export interface SessionInfo {
  alias: Alias;
  expires_at: Time | null;
}

export type Access = "read_files" | "write_files" | "shell" | "tasks" | "send_messages";
export type AgentState = "active" | "disabled";

/** A model Pi has a key for (D104); prices are dollars per million tokens. */
export interface ModelInfo {
  provider: string;
  id: string;
  name: string;
  context_window: number;
  price: { input: number; output: number };
}

export interface AgentCard {
  alias: Alias;
  title: string;
  model: { provider: string; id: string };
  access: Access[];
  allow_message_to: Alias[];
  prompt: string;
  hue: number | null;
}

export interface Agent extends AgentCard {
  version: number;
  state: AgentState;
}

export interface AgentVersion {
  version: number;
  author: Alias;
  saved_at: Time;
  card: AgentCard;
}

export interface ActiveRun {
  agent: Alias;
  run_id: string;
  session: SessionKey;
  correlation_id: string;
  started_at: Time;
}

export interface FailingSession {
  session: SessionKey;
  failures: number;
  last_error: string;
  stuck: boolean;
}

export interface AgentStatus {
  alias: Alias;
  title: string;
  state: AgentState;
  run: ActiveRun | null;
  unread: number;
  failing: FailingSession[];
}

export interface StoppedChain {
  chain: string;
  stopped_at: Time;
  reason: string | null;
}

export interface Status {
  agents: AgentStatus[];
  stopped_chains: StoppedChain[];
}

export type RunOutcome = "running" | "ok" | "error" | "stopped";

export interface RunRecord {
  run_id: string;
  agent: Alias;
  card_version: number;
  session: SessionKey;
  correlation_id: string;
  started_at: Time;
  finished_at: Time | null;
  outcome: RunOutcome;
  reason: string | null;
  usage: { tokens_in: number; tokens_out: number; cost_micros: number };
}

export type Step =
  | { kind: "prompt" | "thinking" | "answer"; text: string; truncated: boolean }
  | { kind: "tool_call"; tool: string; arguments: unknown }
  | { kind: "tool_result"; tool: string; text: string; truncated: boolean; is_error: boolean };

export type TranscriptEntry = Step & { at: Time };

export interface Message {
  id: string;
  from: Alias;
  to: Alias;
  body: string;
  subject: string | null;
  task_id: string | null;
  in_reply_to: string | null;
  correlation_id: string;
  created_at: Time;
}

export interface NewMessage {
  to: Alias;
  body: string;
  subject: string | null;
  task_id: string | null;
  in_reply_to: string | null;
}

export interface Conversation {
  agent: Alias;
  last: Message;
  unread: number;
}

/** One session of an agent's, as a thread of the operator's (D105). */
export interface Thread {
  session: SessionKey;
  first: Message;
  last: Message;
  unread: number;
}

export type TaskStatus = "open" | "in_progress" | "needs_human" | "done" | "cancelled";

export interface Task {
  id: string;
  title: string;
  context: string;
  status: TaskStatus;
  creator: Alias;
  created_at: Time;
  parent_id: string | null;
  assignee: Alias | null;
  members: Alias[];
  deadline: Time | null;
  result: { text: string; links: string[] } | null;
}

export type Change =
  | { kind: "created" }
  | { kind: "status"; from: TaskStatus; to: TaskStatus }
  | { kind: "result"; result: { text: string; links: string[] } }
  | { kind: "note"; text: string };

export interface TaskView {
  task: Task;
  history: { at: Time; who: Alias; change: Change }[];
}

export interface JournalEntry {
  timestamp: Time;
  actor: Alias;
  component: "gateway" | "runtime" | "broker" | "task_manager";
  action: string;
  decision: "allow" | "deny" | null;
  result: "ok" | "error" | null;
  reason: string | null;
  run_id: string | null;
  correlation_id: string | null;
  task_id: string | null;
  path: string | null;
}
