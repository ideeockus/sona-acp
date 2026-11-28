<!-- The operator's conversations: agents, then an agent's threads, then one thread.
     A thread is a chain: everything that grew out of one message, replies from tasks
     the agent took up on the way included (D112). The agent remembers what was said in
     it (D78, D105). Opening a thread reads it. -->
<script lang="ts">
  import Ring from "../components/Ring.svelte";
  import { look } from "../lib/agent";
  import { api, ApiError } from "../lib/api";
  import { bytes, firstLine, since, sessionLabel, short } from "../lib/format";
  import { markdown } from "../lib/markdown";
  import { poll } from "../lib/poll.svelte";
  import { navigate } from "../lib/router.svelte";
  import { session as me } from "../lib/session.svelte";
  import type { Agent, Attachment, Conversation, FileInfo, Message, NewMessage, Status, Task, Thread } from "../lib/types";

  /** `chain` is a thread's chain, or "new" for a thread not started yet. */
  let { alias = null, chain = null }: { alias?: string | null; chain?: string | null } = $props();
  const starting = $derived(chain === "new");

  const PAGE = 50;
  let agents = $state<Agent[]>([]);
  let conversations = $state<Conversation[]>([]);
  let threads = $state<Thread[] | null>(null);
  let status = $state<Status | null>(null);
  let tasks = $state<Task[]>([]);
  let messages = $state<Message[]>([]);
  let more = $state(false);
  /** Messages that were unread when the thread opened, shown as such until it closes. */
  let unreadIds = $state<Set<string>>(new Set());
  let now = $state(Date.now());

  let body = $state("");
  /** Files picked for the next message, uploaded when it is sent. */
  let picked = $state<File[]>([]);
  let sending = $state(false);
  let error = $state<string | null>(null);

  poll(async () => {
    [agents, conversations, status, tasks] = await Promise.all([
      api.get<Agent[]>("/agents"),
      api.get<Conversation[]>("/conversations"),
      api.get<Status>("/status"),
      api.get<Task[]>("/tasks"),
    ]);
    now = Date.now();
    if (!alias) return;
    threads = await api.get<Thread[]>(`/conversations/${alias}/threads`);
    // An agent opens on its latest thread, or on a new one when there is none.
    if (chain === null) {
      navigate(`/messages/${alias}/${threads[0]?.chain ?? "new"}`, true);
      return;
    }
    if (!starting) await refreshThread();
  }, 3000);

  async function refreshThread() {
    const latest = await api.get<Message[]>(`/conversations/${alias}/messages?chain=${chain}`);
    const known = new Set(messages.map((m) => m.id));
    if (messages.length === 0) more = latest.length === PAGE;
    const fresh = latest.filter((m) => !known.has(m.id));
    if (fresh.length === 0) return;
    // What came from the agent since the thread was read last, before marking it read.
    const unread = thread?.unread ?? 0;
    if (unread > 0) {
      const theirs = latest.filter((m) => m.from === alias).slice(-unread);
      unreadIds = new Set([...unreadIds, ...theirs.map((m) => m.id)]);
    }
    messages = [...messages, ...fresh];
    const last = messages.at(-1);
    if (last && unread > 0) await api.post(`/conversations/${alias}/read`, { up_to: last.id });
  }

  async function loadEarlier() {
    const first = messages[0];
    if (!first) return;
    const older = await api.get<Message[]>(`/conversations/${alias}/messages?chain=${chain}&before=${first.id}`);
    more = older.length === PAGE;
    messages = [...older, ...messages];
  }

  const agent = $derived(agents.find((a) => a.alias === alias));
  const mine = $derived(status?.agents.find((a) => a.alias === alias));
  const thread = $derived(threads?.find((t) => t.chain === chain));

  const taskTitle = (id: string | null) => (id ? (tasks.find((t) => t.id === id)?.title ?? null) : null);
  /** A thread is named by its task, or by what opened it. */
  const title = (t: Thread) => taskTitle(t.first.task_id) ?? t.first.subject ?? firstLine(t.first.body);
  const stopped = (c: string) => status?.stopped_chains.some((s) => s.chain === c) ?? false;
  /** A thread is stuck when any session it went through is. */
  const failing = (t: Thread | undefined) => (t ? mine?.failing.find((f) => t.sessions.includes(f.session)) : undefined);
  const working = (c: string) => mine?.run?.correlation_id === c;

  const list = $derived.by(() => {
    const talked = conversations.map((c) => c.agent);
    const rest = agents.map((a) => a.alias).filter((a) => !talked.includes(a));
    return [...talked, ...rest.sort()];
  });

  function day(time: string): string {
    const d = new Date(time);
    const today = new Date();
    const yesterday = new Date(Date.now() - 86_400_000);
    if (d.toDateString() === today.toDateString()) return "today";
    if (d.toDateString() === yesterday.toDateString()) return "yesterday";
    return d.toLocaleDateString([], { month: "short", day: "numeric" });
  }

  function hm(time: string): string {
    return new Date(time).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
  }

  async function send(event?: SubmitEvent) {
    event?.preventDefault();
    if (!alias || !body.trim()) return;
    const last = messages.at(-1);
    sending = true;
    error = null;
    let attachments: string[];
    try {
      attachments = (await Promise.all(picked.map((f) => api.upload<FileInfo>(f)))).map((f) => f.id);
    } catch (e) {
      error = e instanceof ApiError ? (e.body.detail ?? e.body.kind) : "Could not upload.";
      sending = false;
      return;
    }
    const message: NewMessage = {
      to: alias,
      body: body.trim(),
      subject: null,
      // Continues the last message: its chain (D41), and its task if the conversation
      // moved into one, so the agent answers with that task in mind (D112).
      task_id: starting ? null : (last?.task_id ?? null),
      in_reply_to: starting ? null : (last?.id ?? null),
      attachments,
    };
    try {
      const sent = await api.post<Message>("/messages", message);
      body = "";
      picked = [];
      if (starting) {
        navigate(`/messages/${alias}/${sent.correlation_id}`);
      } else {
        await refreshThread();
      }
    } catch (e) {
      error = e instanceof ApiError ? (e.body.detail ?? e.body.kind) : "Could not send.";
    } finally {
      sending = false;
    }
  }

  /** Enter sends, Shift+Enter breaks the line; Enter that ends an IME composition does neither. */
  function onKey(event: KeyboardEvent) {
    if (event.key !== "Enter" || event.shiftKey || event.isComposing) return;
    event.preventDefault();
    void send();
  }

  const IMAGE = /\.(png|jpe?g|gif|webp)$/i;
  const fileUrl = (a: Attachment) => `/v1/files/${a.id}`;

  function pick(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    picked = [...picked, ...Array.from(input.files ?? [])];
    input.value = "";
  }

  async function act(path: string) {
    await api.post(path);
    status = await api.get<Status>("/status");
  }
</script>

<div class="messages">
  <section class="column agents" aria-label="Agents">
    <div class="column-head"><h1>Messages</h1></div>
    {#each list as a (a)}
      {@const c = conversations.find((x) => x.agent === a)}
      {@const card = agents.find((x) => x.alias === a)}
      {@const state = look(status?.agents.find((x) => x.alias === a))}
      <a class="row" href="/messages/{a}" aria-current={a === alias ? "true" : undefined}>
        <Ring hue={card?.hue ?? null} {state} size={24} />
        <span class="row-text">
          <span class="row-top">
            <b class="agent" class:bold={(c?.unread ?? 0) > 0} style="--hue: {card?.hue ?? 28}">{a}</b>
            {#if c && c.unread > 0}
              <span class="mono small live">{c.unread}</span>
            {:else if state === "stuck"}
              <span class="mono small failed">stuck</span>
            {/if}
          </span>
          <span class="preview muted">{card?.title ?? ""}</span>
        </span>
      </a>
    {:else}
      <p class="empty">No agents yet.</p>
    {/each}
  </section>

  {#if alias}
    <section class="column threads" aria-label="Threads with {alias}">
      <div class="column-head">
        <h2 class="agent" style="--hue: {agent?.hue ?? 28}">{alias}</h2>
        <a class="text-button quiet" href="/agents/{alias}">Agent page</a>
      </div>
      {#if agent?.state !== "disabled"}
        <a class="row new" href="/messages/{alias}/new" aria-current={starting ? "true" : undefined}>+ New thread</a>
      {/if}
      {#each threads ?? [] as t (t.chain)}
        {@const f = failing(t)}
        <a class="row" href="/messages/{alias}/{t.chain}" aria-current={t.chain === chain ? "true" : undefined}>
          <span class="row-text">
            <span class="row-top">
              <span class="title" class:bold={t.unread > 0}>{title(t)}</span>
              {#if t.unread > 0}<span class="mono small live">{t.unread}</span>{/if}
            </span>
            <span class="mono small muted">
              {#if working(t.chain)}<span class="live"><span class="pulse">●</span> working</span>
              {:else if f}<span class="failed">{f.stuck ? "stuck" : "failing"}</span>
              {:else if stopped(t.chain)}<span class="failed">stopped</span>
              {:else}{since(t.last.created_at, now)}{/if}
              {#if t.sessions.some((s) => s.startsWith("task-"))} · with a task{/if}
            </span>
          </span>
        </a>
      {/each}
    </section>

    <section class="column thread" aria-labelledby="thread-title">
      <header>
        <h2 id="thread-title">{starting ? "New thread" : thread ? title(thread) : ""}</h2>
        {#if thread?.first.task_id}
          <a class="text-button quiet" href="/tasks/{thread.first.task_id}">Task</a>
        {:else if thread}
          <a class="text-button quiet" href="/journal?chain={thread.first.correlation_id}">Journal</a>
        {/if}
      </header>

      <ol aria-label="Messages">
        {#if starting}
          <li class="center muted">A new thread starts a new conversation: @{alias} will not remember the others.</li>
        {/if}
        {#if more}<li class="center"><button class="text-button quiet" onclick={loadEarlier}>Earlier messages</button></li>{/if}
        {#each messages as m, i (m.id)}
          {@const yours = m.from === me.alias}
          {#if i === 0 || day(messages[i - 1]!.created_at) !== day(m.created_at)}
            <li class="center mono small muted">{day(m.created_at)}</li>
          {/if}
          <li class:yours>
            <span class="meta mono small muted">
              {#if yours}you{:else}<span class="agent" style="--hue: {agent?.hue ?? 28}">{m.from}</span>{/if}
              · {hm(m.created_at)}
              {#if m.task_id}<a class="topic" href="/tasks/{m.task_id}">task · {taskTitle(m.task_id) ?? short(m.task_id)}</a>{/if}
              {#if m.external}<span class="topic">external</span>{/if}
              {#if unreadIds.has(m.id)}<span class="live">unread</span>{/if}
            </span>
            {#if m.subject}<b class="subject">{m.subject}</b>{/if}
            {#if yours}
              <!-- Yours, exactly as you typed it. -->
              <p class="body">{m.body}</p>
            {:else}
              <!-- The agent's Markdown; markdown() leaves raw HTML as text. -->
              <div class="body md">{@html markdown(m.body)}</div>
            {/if}
            {#if m.attachments.length > 0}
              <ul class="files">
                {#each m.attachments as a (a.id)}
                  <li>
                    {#if IMAGE.test(a.name)}
                      <a href={fileUrl(a)} target="_blank" rel="noopener noreferrer"><img src={fileUrl(a)} alt={a.name} /></a>
                    {/if}
                    <a class="mono small" href={fileUrl(a)} download={a.name}>↓ {a.name}</a>
                    <span class="mono small muted">{bytes(a.size)}</span>
                  </li>
                {/each}
              </ul>
            {/if}
          </li>
        {/each}
        {#if thread && working(thread.chain)}
          <li class="mono small live"><span class="pulse">●</span> working · {since(mine!.run!.started_at, now)}</li>
        {/if}
        {#if thread}
          {@const f = failing(thread)}
          {#if f}
            <!-- A failing session holds its messages back: say so where they were written. -->
            <li class="failing mono small">
              <span>
                <span class="failed">{f.stuck ? "stuck" : "failed, will retry"}</span>
                · {sessionLabel(f.session)} · <span class="muted" title={f.last_error}>{firstLine(f.last_error)}</span>
              </span>
              {#if f.stuck}
                <span class="actions">
                  <button class="text-button" onclick={() => act(`/agents/${alias}/sessions/${f.session}/retry`)}>Retry</button>
                  <button class="text-button quiet" onclick={() => act(`/agents/${alias}/sessions/${f.session}/skip`)}>Skip</button>
                </span>
              {/if}
            </li>
          {/if}
          {#if stopped(thread.chain)}
            <li class="failing mono small"><span><span class="failed">stopped</span> · messages wait until the chain is resumed</span></li>
          {/if}
        {/if}
      </ol>

      {#if agent?.state === "disabled"}
        <p class="empty">@{alias} is disabled: it takes no messages until enabled.</p>
      {:else if agent && chain}
        <form onsubmit={send}>
          <textarea
            rows="3"
            aria-label="Message to {alias}"
            placeholder={starting ? `Start a thread with ${alias}…` : `Reply in this thread…`}
            bind:value={body}
            onkeydown={onKey}
          ></textarea>
          {#if picked.length > 0}
            <ul class="files">
              {#each picked as f, i (i)}
                <li>
                  <span class="mono small">{f.name}</span>
                  <span class="mono small muted">{bytes(f.size)}</span>
                  <button type="button" class="text-button quiet" onclick={() => (picked = picked.filter((_, j) => j !== i))}>Remove</button>
                </li>
              {/each}
            </ul>
          {/if}
          <div class="send">
            <label class="attach text-button quiet">
              Attach
              <input type="file" multiple onchange={pick} />
            </label>
            <span class="muted small">Enter sends · Shift+Enter for a new line</span>
            {#if error}<span class="error" role="alert">{error}</span>{/if}
            <button class="primary" disabled={sending || !body.trim()}>Send</button>
          </div>
        </form>
      {/if}
    </section>
  {:else}
    <section class="column thread"><p class="empty">Choose an agent.</p></section>
  {/if}
</div>

<style>
  .messages {
    min-height: 100vh;
    display: flex;
  }
  .column {
    min-width: 0;
    border-right: 1px solid var(--line);
  }
  .agents {
    flex: 0 0 200px;
  }
  .threads {
    flex: 0 0 240px;
  }
  .column-head {
    min-height: 56px;
    padding: 0 20px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    border-bottom: 1px solid var(--line);
  }
  .row {
    padding: 12px 20px;
    display: flex;
    gap: 12px;
    align-items: center;
    border-bottom: 1px solid var(--line);
  }
  .row[aria-current="true"] {
    background: var(--accent-soft);
  }
  .row:hover {
    color: inherit;
    background: var(--accent-soft);
  }
  .row.new {
    color: var(--accent-text);
  }
  .row-text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .row-top {
    display: flex;
    justify-content: space-between;
    gap: 8px;
  }
  .row-top b {
    font-weight: 400;
  }
  .bold,
  .row-top b.bold {
    font-weight: 600;
  }
  .title,
  .preview {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .thread {
    flex: 1 1 auto;
    display: flex;
    flex-direction: column;
    border-right: 0;
  }
  /* Narrow: the columns stack, the thread last. */
  @media (max-width: 900px) {
    .messages {
      flex-direction: column;
    }
    .agents,
    .threads {
      flex: none;
      border-right: 0;
      border-bottom: 1px solid var(--line);
    }
  }
  header {
    min-height: 56px;
    padding: 0 32px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    border-bottom: 1px solid var(--line);
  }
  header h2 {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  ol {
    flex: 1;
    margin: 0;
    padding: 24px 32px;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 18px;
    max-width: 820px;
  }
  .center {
    align-self: center;
  }
  li {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .topic {
    padding: 0 6px;
    border: 1px solid var(--line-strong);
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: baseline;
  }
  .body {
    max-width: 600px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .subject {
    font-weight: 500;
  }
  .failing {
    flex-direction: row;
    flex-wrap: wrap;
    justify-content: space-between;
    gap: 12px;
    padding: 8px 12px;
    border-left: 2px solid var(--danger-line);
  }
  .failing .actions {
    display: flex;
    gap: 16px;
  }
  .md {
    white-space: normal;
  }
  .md :global(:is(p, ul, ol, pre, table, blockquote, h1, h2, h3, h4)) {
    margin: 0 0 8px;
  }
  .md :global(:last-child) {
    margin-bottom: 0;
  }
  .md :global(:is(h1, h2, h3, h4)) {
    font-size: 13px;
    font-weight: 600;
  }
  .md :global(:is(ul, ol)) {
    padding-left: 20px;
  }
  .md :global(code) {
    font-family: var(--mono);
    font-size: 12px;
    padding: 0 3px;
    background: var(--surface);
    border: 1px solid var(--line);
  }
  .md :global(pre) {
    padding: 8px 12px;
    overflow-x: auto;
    background: var(--surface);
    border: 1px solid var(--line);
  }
  .md :global(pre code) {
    padding: 0;
    border: 0;
  }
  .md :global(table) {
    border-collapse: collapse;
    display: block;
    overflow-x: auto;
  }
  .md :global(:is(th, td)) {
    padding: 4px 10px;
    border: 1px solid var(--line);
    text-align: left;
  }
  .md :global(th) {
    font-weight: 600;
  }
  .md :global(blockquote) {
    padding-left: 10px;
    border-left: 2px solid var(--line-strong);
    color: var(--muted);
  }
  .md :global(a) {
    color: var(--accent-text);
    text-decoration: underline;
    text-underline-offset: 3px;
  }
  .yours .body {
    padding: 8px 12px;
    background: var(--surface);
    border: 1px solid var(--line);
  }
  form {
    padding: 16px 32px 24px;
    max-width: 820px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    border-top: 1px solid var(--line);
  }
  textarea {
    resize: vertical;
  }
  .send {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .files {
    margin: 4px 0 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .files li {
    flex-direction: row;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }
  .files img {
    display: block;
    max-width: 240px;
    max-height: 180px;
    border: 1px solid var(--line);
  }
  .attach {
    flex-direction: row;
    cursor: pointer;
  }
  .attach input {
    display: none;
  }
  .send .primary {
    margin-left: auto;
  }
</style>
