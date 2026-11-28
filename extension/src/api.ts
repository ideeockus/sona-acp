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

/** A file goes up or down in one request; 25 MiB may take a while. */
const FILE_TIMEOUT_MS = 120_000;

/** Stores the bytes in File Storage under `name` and returns the file's info (D106). */
export async function upload(name: string, contentType: string, content: Uint8Array): Promise<unknown> {
  const response = await fetch(`${address()}/v1/files?name=${encodeURIComponent(name)}`, {
    method: "POST",
    headers: { authorization: `Bearer ${token()}`, "content-type": contentType },
    // A copy on its own ArrayBuffer: what fetch takes as a body.
    body: new Uint8Array(content),
    signal: AbortSignal.timeout(FILE_TIMEOUT_MS),
  });
  const text = await response.text();
  if (!response.ok) {
    throw new ApiError(describe(response.status, text));
  }
  return JSON.parse(text);
}

/** The bytes of a file in File Storage. */
export async function download(id: string): Promise<Uint8Array> {
  const response = await fetch(`${address()}/v1/files/${encodeURIComponent(id)}`, {
    headers: { authorization: `Bearer ${token()}` },
    signal: AbortSignal.timeout(FILE_TIMEOUT_MS),
  });
  if (!response.ok) {
    throw new ApiError(describe(response.status, await response.text()));
  }
  return new Uint8Array(await response.arrayBuffer());
}

function address(): string {
  const url = process.env.SONA_PLANE_API_URL;
  if (!url) {
    throw new ApiError("the control plane address is missing");
  }
  return url;
}

function token(): string {
  const token = process.env.SONA_PLANE_RUN_TOKEN;
  if (!token) {
    throw new ApiError("the Run token is missing");
  }
  return token;
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
