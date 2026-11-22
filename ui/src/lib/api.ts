// The control plane API on the same origin (D98). The session cookie goes along by
// itself; the page never sees a credential after login (D103).

import type { ApiErrorBody } from "./types";
import { navigate } from "./router.svelte";

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly body: ApiErrorBody,
  ) {
    super(body.detail ?? body.kind);
  }
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const response = await fetch(`/v1${path}`, {
    method,
    credentials: "same-origin",
    headers: body === undefined ? {} : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  const json = text ? JSON.parse(text) : null;
  if (!response.ok) {
    const error = new ApiError(response.status, json ?? { kind: "internal" });
    // The session ended: back to login, then back here.
    if (response.status === 401 && path !== "/session") {
      navigate(`/login?next=${encodeURIComponent(location.pathname)}`);
    }
    throw error;
  }
  return json as T;
}

export const api = {
  get: <T>(path: string) => request<T>("GET", path),
  post: <T = null>(path: string, body?: unknown) => request<T>("POST", path, body),
  put: <T>(path: string, body: unknown) => request<T>("PUT", path, body),
  delete: (path: string) => request<null>("DELETE", path),
};
