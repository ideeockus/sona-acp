// Who is logged in. `null` until known; the shell waits for it.

import { api } from "./api";
import type { SessionInfo } from "./types";

let alias = $state<string | null>(null);

export const session = {
  get alias() {
    return alias;
  },
};

/** Asks the API; false when there is no session. */
export async function loadSession(): Promise<boolean> {
  try {
    alias = (await api.get<SessionInfo>("/session")).alias;
    return true;
  } catch {
    alias = null;
    return false;
  }
}

export async function login(credential: string) {
  alias = (await api.post<SessionInfo>("/session", { credential })).alias;
}

export async function logout() {
  await api.delete("/session");
  alias = null;
}
