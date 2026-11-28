// The control plane's tools for the agent. Each is one API call; the components
// decide what the agent may do, and Gateway checks the grant first (D22).

import { defineTool } from "@earendil-works/pi-coding-agent";
import { mkdir, readdir, readFile, writeFile } from "node:fs/promises";
import { basename, dirname, extname, join, relative } from "node:path";
import { Type } from "typebox";
import { call, download, session, upload } from "./api.ts";

/** The context shows aliases with `@`, the API takes them without. */
const alias = (value: string) => value.replace(/^@/, "");

const reply = (value: unknown) => ({
  content: [{ type: "text" as const, text: JSON.stringify(value ?? null, null, 2) }],
  details: undefined,
});

const Alias = (description: string) => Type.String({ description });
const Id = (description: string) => Type.String({ description });
const Status = Type.Union(
  ["open", "in_progress", "needs_human", "done", "cancelled"].map((s) => Type.Literal(s)),
);

const sendMessage = defineTool({
  name: "send_message",
  label: "Send message",
  description: "Send a message to another agent or to the operator.",
  parameters: Type.Object({
    to: Alias("Recipient alias, e.g. @coder"),
    body: Type.String({ description: "Message text" }),
    subject: Type.Optional(Type.String()),
    task_id: Type.Optional(Id("The task the message is about")),
    in_reply_to: Type.Optional(Id("The message this one answers")),
    files: Type.Optional(
      Type.Array(Id("A file id"), { description: "Files to attach, by id from file_upload or file_list" }),
    ),
  }),
  async execute(_id, p) {
    const message = {
      to: alias(p.to),
      body: p.body,
      subject: p.subject ?? null,
      task_id: p.task_id ?? null,
      in_reply_to: p.in_reply_to ?? null,
      attachments: p.files ?? [],
    };
    return reply(await call("POST", "/v1/messages", message));
  },
});

const readMessages = defineTool({
  name: "read_messages",
  label: "Read messages",
  description: "Read the unread messages of this session, oldest first.",
  parameters: Type.Object({}),
  async execute() {
    return reply(await call("GET", `/v1/sessions/${session()}/messages`));
  },
});

const taskCreate = defineTool({
  name: "task_create",
  label: "Create task",
  description: "Create a task. Its assignee is the one responsible for moving it on.",
  parameters: Type.Object({
    title: Type.String(),
    context: Type.String({ description: "What has to be done and why" }),
    assignee: Type.Optional(Alias("Alias of the responsible agent")),
    members: Type.Optional(Type.Array(Alias("Alias of another participant"))),
    parent_id: Type.Optional(Id("The task this one is part of")),
  }),
  async execute(_id, p) {
    const task = {
      title: p.title,
      context: p.context,
      parent_id: p.parent_id ?? null,
      assignee: p.assignee ? alias(p.assignee) : null,
      members: (p.members ?? []).map(alias),
      deadline: null,
    };
    return reply(await call("POST", "/v1/tasks", task));
  },
});

const taskGet = defineTool({
  name: "task_get",
  label: "Get task",
  description: "Get a task with its history.",
  parameters: Type.Object({ id: Id("Task id") }),
  async execute(_id, p) {
    return reply(await call("GET", `/v1/tasks/${encodeURIComponent(p.id)}`));
  },
});

const taskUpdate = defineTool({
  name: "task_update",
  label: "Update task",
  description: "Change a task's status, set its result or add a note. Closed tasks cannot change.",
  parameters: Type.Object({
    id: Id("Task id"),
    status: Type.Optional(Status),
    result: Type.Optional(
      Type.Object({
        text: Type.String(),
        links: Type.Optional(Type.Array(Type.String())),
      }),
    ),
    note: Type.Optional(Type.String()),
  }),
  async execute(_id, p) {
    const update = {
      status: p.status ?? null,
      result: p.result ? { text: p.result.text, links: p.result.links ?? [] } : null,
      note: p.note ?? null,
    };
    return reply(await call("PATCH", `/v1/tasks/${encodeURIComponent(p.id)}`, update));
  },
});

/** Enough for a browser or a reader to tell what came; anything else is bytes. */
const TYPES: Readonly<Record<string, string>> = {
  ".txt": "text/plain",
  ".md": "text/markdown",
  ".csv": "text/csv",
  ".json": "application/json",
  ".html": "text/html",
  ".css": "text/css",
  ".js": "text/javascript",
  ".ts": "text/plain",
  ".pdf": "application/pdf",
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".jpeg": "image/jpeg",
  ".gif": "image/gif",
  ".webp": "image/webp",
  ".svg": "image/svg+xml",
  ".zip": "application/zip",
};

// The path was resolved and checked by Gateway before the call (index.ts).
const fileUpload = defineTool({
  name: "file_upload",
  label: "Upload file",
  description:
    "Store a file from your workdir in File Storage. Returns its id: attach it to a message with send_message's files.",
  parameters: Type.Object({
    path: Type.String({ description: "The file in your workdir" }),
  }),
  async execute(_id, p) {
    const content = await readFile(p.path);
    const type = TYPES[extname(p.path).toLowerCase()] ?? "application/octet-stream";
    return reply(await upload(basename(p.path), type, content));
  },
});

const fileDownload = defineTool({
  name: "file_download",
  label: "Download file",
  description: "Save a file from File Storage into your workdir, e.g. an attachment of a message you got.",
  parameters: Type.Object({
    file_id: Id("The file's id"),
    path: Type.String({ description: "Where to save it in your workdir" }),
  }),
  async execute(_id, p) {
    const content = await download(p.file_id);
    await mkdir(dirname(p.path), { recursive: true });
    await writeFile(p.path, content);
    return reply({ saved: p.path, size: content.length });
  },
});

const fileList = defineTool({
  name: "file_list",
  label: "List files",
  description: "The files you may read: yours and those attached to messages to you, newest first.",
  parameters: Type.Object({}),
  async execute() {
    return reply(await call("GET", "/v1/files"));
  },
});

// Every agent has its own memory: it outlives sessions and is searched, not shown (D108).
const memoryAdd = defineTool({
  name: "memory_add",
  label: "Remember",
  description:
    "Keep a fact or a decision for later sessions: one short record, not a document. Fails when memory is full: then delete what is no longer true.",
  parameters: Type.Object({ text: Type.String({ description: "What to remember" }) }),
  async execute(_id, p) {
    return reply(await call("POST", "/v1/memory", { text: p.text }));
  },
});

const memorySearch = defineTool({
  name: "memory_search",
  label: "Search memory",
  description: "Find records in your memory containing the text, best matches first.",
  parameters: Type.Object({ query: Type.String({ description: "Text to look for" }) }),
  async execute(_id, p) {
    return reply(await call("GET", `/v1/memory?q=${encodeURIComponent(p.query)}`));
  },
});

const memoryList = defineTool({
  name: "memory_list",
  label: "List memory",
  description: "All records in your memory, newest first.",
  parameters: Type.Object({}),
  async execute() {
    return reply(await call("GET", "/v1/memory"));
  },
});

const memoryDelete = defineTool({
  name: "memory_delete",
  label: "Forget",
  description: "Delete a record from your memory by id.",
  parameters: Type.Object({ id: Id("The record's id") }),
  async execute(_id, p) {
    await call("DELETE", `/v1/memory/${encodeURIComponent(p.id)}`);
    return reply({ deleted: p.id });
  },
});

/** Every file under `dir`, as paths relative to it. Symlinks are left out. */
async function filesUnder(dir: string): Promise<string[]> {
  const entries = await readdir(dir, { withFileTypes: true, recursive: true });
  return entries
    .filter((e) => e.isFile())
    .map((e) => relative(dir, join(e.parentPath, e.name)))
    .filter((p) => !p.split("/").includes(".git"));
}

// The directory was resolved and checked by Gateway before the call (index.ts).
const skillPropose = defineTool({
  name: "skill_propose",
  label: "Propose skill",
  description:
    "Propose a skill you wrote: a directory in your workdir with SKILL.md (frontmatter name and description) and any scripts or references. It goes to the operator's library disabled; you can use it once the operator enables it and grants it to you.",
  parameters: Type.Object({
    path: Type.String({ description: "The skill's directory in your workdir" }),
  }),
  async execute(_id, p) {
    const files = [];
    for (const path of await filesUnder(p.path)) {
      files.push({ path, content: await readFile(join(p.path, path), "utf8") });
    }
    return reply(await call("POST", "/v1/skills/proposals", { files }));
  },
});

export const tools = [
  skillPropose,
  sendMessage,
  readMessages,
  taskCreate,
  taskGet,
  taskUpdate,
  fileUpload,
  fileDownload,
  fileList,
  memoryAdd,
  memorySearch,
  memoryList,
  memoryDelete,
];
