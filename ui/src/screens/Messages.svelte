<!-- The operator's conversation with each agent: one thread over every session, each
     message tagged with its task or chain (D78, D102). Opening a thread reads it. -->
<script lang="ts">
  import Ring from "../components/Ring.svelte";
  import { look } from "../lib/agent";
  import { api, ApiError } from "../lib/api";
  import { firstLine, since, sessionLabel, short } from "../lib/format";
  import { poll } from "../lib/poll.svelte";
  import { session } from "../lib/session.svelte";
  import type { Agent, Conversation, Message, NewMessage, Status, Task } from "../lib/types";

  let { alias = null }: { alias?: string | null } = $props();

  const PAGE = 50;
  let agents = $state<Agent[]>([]);
  let conversations = $state<Conversation[]>([]);
  let status = $state<Status | null>(null);
  let tasks = $state<Task[]>([]);
  let thread = $state<Message[]>([]);
  let more = $state(false);
  /** Messages that were unread when the thread opened, shown as such until it closes. */
  let unreadIds = $state<Set<string>>(new Set());
  let now = $state(Date.now());

  let body = $state("");
  let topic = $state("");
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
    if (alias) await refreshThread();
  }, 3000);

  async function refreshThread() {
    const latest = await api.get<Message[]>(`/conversations/${alias}/messages`);
    const known = new Set(thread.map((m) => m.id));
    if (thread.length === 0) more = latest.length === PAGE;
    const fresh = latest.filter((m) => !known.has(m.id));
    if (fresh.length === 0) return;
    // What came from the agent since the thread was read last, before marking it read.
    const conversation = conversations.find((c) => c.agent === alias);
    if (conversation && conversation.unread > 0) {
      const theirs = latest.filter((m) => m.from === alias).slice(-conversation.unread);
      unreadIds = new Set([...unreadIds, ...theirs.map((m) => m.id)]);
    }
    thread = [...thread, ...fresh];
    const last = thread.at(-1);
    if (last && (conversation?.unread ?? 0) > 0) {
      await api.post(`/conversations/${alias}/read`, { up_to: last.id });
    }
    if (!topic) topic = topics.at(-1)?.key ?? "new";
  }

  async function loadEarlier() {
    const first = thread[0];
    if (!first) return;
    const older = await api.get<Message[]>(`/conversations/${alias}/messages?before=${first.id}`);
    more = older.length === PAGE;
    thread = [...older, ...thread];
  }

  const agent = $derived(agents.find((a) => a.alias === alias));
  const me = $derived(status?.agents.find((a) => a.alias === alias));
  const taskTitle = (id: string | null) => (id ? (tasks.find((t) => t.id === id)?.title ?? `task ${short(id)}`) : null);
  const stopped = (chain: string) => status?.stopped_chains.some((c) => c.chain === chain) ?? false;
  const label = (m: Message) => taskTitle(m.task_id) ?? `chain ${short(m.correlation_id)}`;

  /** Where a reply can go: each session of the thread, newest last, or a new chain. */
  const topics = $derived.by(() => {
    const seen = new Map<string, { key: string; text: string; last: Message }>();
    for (const m of thread) {
      const key = m.task_id ? `task-${m.task_id}` : `chain-${m.correlation_id}`;
      seen.delete(key);
      seen.set(key, { key, text: label(m) + (stopped(m.correlation_id) ? " · stopped" : ""), last: m });
    }
    return [...seen.values()];
  });

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
    const target = topics.find((t) => t.key === topic);
    const message: NewMessage = {
      to: alias,
      body: body.trim(),
      subject: null,
      task_id: target?.last.task_id ?? null,
      // Continues the chain of that message (D41); none starts a new chain.
      in_reply_to: target?.last.id ?? null,
    };
    sending = true;
    error = null;
    try {
      await api.post("/messages", message);
      body = "";
      await refreshThread();
      topic = topics.at(-1)?.key ?? "new";
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

  async function act(path: string) {
    await api.post(path);
    status = await api.get<Status>("/status");
  }
</script>

<div class="messages">
  <section class="list" aria-label="Agents">
    <div class="list-head"><h1>Messages</h1></div>
    {#each list as a (a)}
      {@const c = conversations.find((x) => x.agent === a)}
      {@const card = agents.find((x) => x.alias === a)}
      {@const state = look(status?.agents.find((x) => x.alias === a))}
      <a class="conv" href="/messages/{a}" aria-current={a === alias ? "true" : undefined}>
        <Ring hue={card?.hue ?? null} {state} size={24} />
        <span class="conv-text">
          <span class="conv-top">
            <b class="agent" class:bold={(c?.unread ?? 0) > 0} style="--hue: {card?.hue ?? 28}">{a}</b>
            {#if c && c.unread > 0}
              <span class="mono small live">{c.unread}</span>
            {:else if state === "stuck"}
              <span class="mono small failed">stuck</span>
            {:else if c}
              <span class="mono small muted">{since(c.last.created_at, now)}</span>
            {/if}
          </span>
          <span class="preview muted">{c ? c.last.body : "No messages yet"}</span>
        </span>
      </a>
    {:else}
      <p class="empty">No agents yet.</p>
    {/each}
  </section>

  {#if alias}
    <section class="thread" aria-labelledby="thread-title">
      <header>
        <div class="who">
          <h2 id="thread-title" class="agent" style="--hue: {agent?.hue ?? 28}">{alias}</h2>
          <span class="muted">{agent?.title ?? ""}</span>
        </div>
        <a class="text-button quiet" href="/agents/{alias}">Agent page</a>
      </header>

      <ol aria-label="Messages with {alias}">
        {#if more}<li class="center"><button class="text-button quiet" onclick={loadEarlier}>Earlier messages</button></li>{/if}
        {#each thread as m, i (m.id)}
          {@const mine = m.from === session.alias}
          {#if i === 0 || day(thread[i - 1]!.created_at) !== day(m.created_at)}
            <li class="center mono small muted">{day(m.created_at)}</li>
          {/if}
          <li class:mine>
            <span class="meta mono small muted">
              {#if mine}you{:else}<span class="agent" style="--hue: {agent?.hue ?? 28}">{m.from}</span>{/if}
              · {hm(m.created_at)}
              {#if m.task_id}<a class="topic" href="/tasks/{m.task_id}">{label(m)}</a>{:else}<a class="topic" href="/journal?chain={m.correlation_id}">{label(m)}</a>{/if}
              {#if unreadIds.has(m.id)}<span class="live">unread</span>{/if}
            </span>
            {#if m.subject}<b class="subject">{m.subject}</b>{/if}
            <p class="body">{m.body}</p>
          </li>
        {:else}
          <li class="center muted">No messages with @{alias} yet.</li>
        {/each}
        {#if me?.run}
          <li class="mono small live working"><span class="pulse">●</span> working · {since(me.run.started_at, now)}</li>
        {/if}
        {#each me?.failing ?? [] as f (f.session)}
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
        {/each}
      </ol>

      {#if agent?.state === "disabled"}
        <p class="empty">@{alias} is disabled: it takes no messages until enabled.</p>
      {:else if agent}
        <form onsubmit={send}>
          <label>
            Message to {alias}
            <textarea rows="3" placeholder="Write to {alias}… Enter sends, Shift+Enter for a new line" bind:value={body} onkeydown={onKey}></textarea>
          </label>
          <div class="send">
            <label class="about">
              About
              <select bind:value={topic}>
                {#each topics as t (t.key)}<option value={t.key}>{t.text}</option>{/each}
                <option value="new">New conversation</option>
              </select>
            </label>
            {#if error}<span class="error" role="alert">{error}</span>{/if}
            <button class="primary" disabled={sending || !body.trim()}>Send</button>
          </div>
        </form>
      {/if}
    </section>
  {:else}
    <section class="thread"><p class="empty">Choose an agent.</p></section>
  {/if}
</div>

<style>
  .messages {
    min-height: 100vh;
    display: flex;
    flex-wrap: wrap;
  }
  .list {
    flex: 1 1 260px;
    max-width: 100%;
    border-right: 1px solid var(--line);
  }
  .list-head {
    min-height: 56px;
    padding: 0 20px;
    display: flex;
    align-items: center;
    border-bottom: 1px solid var(--line);
  }
  .conv {
    padding: 12px 20px;
    display: flex;
    gap: 12px;
    align-items: center;
    border-bottom: 1px solid var(--line);
  }
  .conv[aria-current="true"] {
    background: var(--accent-soft);
  }
  .conv:hover {
    color: inherit;
    background: var(--accent-soft);
  }
  .conv-text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .conv-top {
    display: flex;
    justify-content: space-between;
    gap: 8px;
  }
  .conv-top b {
    font-weight: 400;
  }
  .conv-top b.bold {
    font-weight: 600;
  }
  .preview {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .thread {
    flex: 999 1 480px;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  header {
    min-height: 56px;
    padding: 0 32px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    border-bottom: 1px solid var(--line);
  }
  .who {
    display: flex;
    gap: 12px;
    align-items: baseline;
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
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: baseline;
  }
  .topic {
    padding: 0 6px;
    border: 1px solid var(--line-strong);
  }
  .body {
    max-width: 600px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .mine .body {
    padding: 8px 12px;
    background: var(--surface);
    border: 1px solid var(--line);
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
    gap: 12px;
    align-items: flex-end;
    justify-content: space-between;
    flex-wrap: wrap;
  }
  .about {
    flex-direction: row;
    align-items: center;
    gap: 8px;
  }
  .about select {
    width: auto;
    max-width: 340px;
  }
  @media (max-width: 720px) {
    header,
    ol,
    form {
      padding-left: 16px;
      padding-right: 16px;
    }
  }
</style>
