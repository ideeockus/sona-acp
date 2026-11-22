import type { SessionKey, Time } from "./types";

/** `20m`, `3h`: the largest whole unit, as `sapctl status` shows it. */
export function since(time: Time, now = Date.now()): string {
  const seconds = Math.max(0, Math.floor((now - Date.parse(time)) / 1000));
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h`;
  return `${Math.floor(seconds / 86400)}d`;
}

export function clock(time: Time): string {
  const date = new Date(time);
  const sameDay = date.toDateString() === new Date().toDateString();
  const hm = date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
  return sameDay ? hm : `${date.toLocaleDateString([], { month: "short", day: "numeric" })} ${hm}`;
}

/** First 8 hex digits: enough to tell ids apart on one screen. */
export function short(id: string): string {
  return id.replace(/^(task|chain)-/, "").slice(0, 8);
}

export function sessionLabel(session: SessionKey): string {
  const [kind] = session.split("-", 1);
  return `${kind}-${short(session)}`;
}

export function dollars(micros: number): string {
  if (micros === 0) return "$0";
  return `$${(micros / 1e6).toFixed(micros < 10_000 ? 4 : 2)}`;
}

/** The first line, for a cell; the whole text goes in the title. */
export function firstLine(text: string): string {
  const line = text.split("\n", 1)[0] ?? "";
  return line.length > 90 ? `${line.slice(0, 90)}…` : line;
}

export function tokens(count: number): string {
  return count < 1000 ? `${count}` : `${(count / 1000).toFixed(count < 10_000 ? 1 : 0)}k`;
}
