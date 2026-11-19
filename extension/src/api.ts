// The control plane API, as our hook and tools call it (D85).
//
// Runtime puts the address, the Run token and the session into Pi's environment (D9).

/** Long enough for a busy control plane; a hung one must not hang the agent. */
const TIMEOUT_MS = 30_000;

export class ApiError extends Error {}

/** Sends one request and returns the JSON reply, `undefined` when it is empty. */
export async function call(method: string, path: string, body?: unknown): Promise<unknown> {
  const url = process.env.SONA_PLANE_API_URL;
  const token = process.env.SONA_PLANE_RUN_TOKEN;
  if (!url || !token) {
    throw new ApiError("the control plane address or the Run token is missing");
  }
  const response = await fetch(url + path, {
    method,
    headers: {
      authorization: `Bearer ${token}`,
      ...(body !== undefined && { "content-type": "application/json" }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(TIMEOUT_MS),
  });
  const text = await response.text();
  if (!response.ok) {
    throw new ApiError(describe(response.status, text));
  }
  return text ? JSON.parse(text) : undefined;
}

/** The session this Run works in, `task-<id>` or `chain-<id>` (D78). */
export function session(): string {
  const session = process.env.SONA_PLANE_SESSION;
  if (!session) {
    throw new ApiError("the session is missing");
  }
  return session;
}

/** The API answers with `{ kind, detail }` (D85); anything else is shown as is. */
function describe(status: number, text: string): string {
  try {
    const { kind, detail } = JSON.parse(text) as { kind: string; detail?: string };
    return detail ? `${kind.replaceAll("_", " ")}: ${detail}` : kind;
  } catch {
    return `HTTP ${status}: ${text}`;
  }
}
