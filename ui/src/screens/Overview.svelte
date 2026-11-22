<!-- Numbers, what needs you, what runs, and every agent (docs/ui.md). No map yet (D103). -->
<script lang="ts">
  import Ring from "../components/Ring.svelte";
  import { look } from "../lib/agent";
  import { api } from "../lib/api";
  import { since, sessionLabel, short } from "../lib/format";
  import { poll } from "../lib/poll.svelte";
  import type { Agent, Conversation, Status, Task } from "../lib/types";

  let status = $state<Status | null>(null);
  let agents = $state<Agent[]>([]);
  let conversations = $state<Conversation[]>([]);
  let waiting = $state<Task[]>([]);
  let now = $state(Date.now());

  poll(async () => {
    [status, agents, conversations, waiting] = await Promise.all([
      api.get<Status>("/status"),
      api.get<Agent[]>("/agents"),
      api.get<Conversation[]>("/conversations"),
      api.get<Task[]>("/tasks?status=needs_human"),
    ]);
    now = Date.now();
  });

  const hue = (alias: string) => agents.find((a) => a.alias === alias)?.hue ?? null;
  const unread = $derived(conversations.filter((c) => c.unread > 0));
  const stuck = $derived(
    (status?.agents ?? []).flatMap((a) => a.failing.filter((f) => f.stuck).map((f) => ({ agent: a.alias, ...f }))),
  );
  const running = $derived((status?.agents ?? []).filter((a) => a.run));
  const needYou = $derived(unread.length + waiting.length + stuck.length + (status?.stopped_chains.length ?? 0));

  async function act(path: string) {
    await api.post(path);
    status = await api.get<Status>("/status");
  }
</script>

<header class="screen-header">
  <h1>Overview</h1>
  {#if status}
    <p class="numbers">
      <span><b>{running.length}</b> running</span>
      <span><b class:live={needYou > 0}>{needYou}</b> need you</span>
      {#if status.stopped_chains.length}<span><b>{status.stopped_chains.length}</b> chains stopped</span>{/if}
    </p>
  {/if}
</header>

{#if status}
  <div class="screen-body">
    <section class="panel needs" aria-labelledby="needs">
      <div class="panel-head"><h2 id="needs">Needs you</h2><span class="mono small muted">{needYou}</span></div>
      {#each unread as c (c.agent)}
        <article>
          <div class="meta mono small muted">
            <span><span class="live">{c.unread === 1 ? "message" : `${c.unread} messages`}</span> · {c.agent}</span>
            <span>{since(c.last.created_at, now)}</span>
          </div>
          <p>{c.last.body}</p>
          <div class="actions"><a class="text-button" href="/messages/{c.agent}">Reply</a></div>
        </article>
      {/each}
      {#each waiting as task (task.id)}
        <article>
          <div class="meta mono small muted">
            <span><span class="live">needs human</span> · {task.assignee ?? task.creator}</span>
            <span>{since(task.created_at, now)}</span>
          </div>
          <p>{task.title}</p>
          <div class="actions"><a class="text-button" href="/tasks/{task.id}">Open task</a></div>
        </article>
      {/each}
      {#each stuck as s (s.agent + s.session)}
        <article>
          <div class="meta mono small muted">
            <span><span class="failed">stuck</span> · {s.agent} · {sessionLabel(s.session)}</span>
            <span>{s.failures} failures</span>
          </div>
          <p class="mono">{s.last_error}</p>
          <div class="actions">
            <button class="text-button" onclick={() => act(`/agents/${s.agent}/sessions/${s.session}/retry`)}>Retry</button>
            <button class="text-button quiet" onclick={() => act(`/agents/${s.agent}/sessions/${s.session}/skip`)}>Skip</button>
          </div>
        </article>
      {/each}
      {#each status.stopped_chains as c (c.chain)}
        <article>
          <div class="meta mono small muted">
            <span>chain stopped · {short(c.chain)}</span>
            <span>{since(c.stopped_at, now)}</span>
          </div>
          <p>{c.reason ?? "Stopped by you."}</p>
          <div class="actions">
            <button class="text-button" onclick={() => act(`/chains/${c.chain}/resume`)}>Resume</button>
            <a class="text-button quiet" href="/journal?chain={c.chain}">Journal</a>
          </div>
        </article>
      {/each}
      {#if needYou === 0}<p class="empty">Nothing waits for you.</p>{/if}
    </section>

    <section class="panel agents" aria-labelledby="agents">
      <div class="panel-head"><h2 id="agents">Agents</h2><span class="mono small muted">{status.agents.length}</span></div>
      {#each status.agents as a (a.alias)}
        {@const state = look(a)}
        <a class="agent-row" href="/agents/{a.alias}">
          <Ring hue={hue(a.alias)} {state} />
          <span class="who">
            <b class="agent" style="--hue: {hue(a.alias) ?? 28}">{a.alias}</b>
            <span class="muted">{a.title}</span>
          </span>
          <span class="mono small" class:live={state === "running"} class:failed={state === "stuck"} class:muted={state === "idle" || state === "disabled"}>
            {state}{#if a.run} {since(a.run.started_at, now)}{/if}
          </span>
        </a>
      {:else}
        <p class="empty">No agents yet. <a class="live" href="/agents/new">Create one</a></p>
      {/each}
    </section>

    {#if running.length}
      <section class="panel" aria-labelledby="running">
        <div class="panel-head"><h2 id="running">Running</h2></div>
        <div class="table-wrap">
          <table>
            <thead><tr><th>Agent</th><th>Session</th><th>Chain</th><th>For</th></tr></thead>
            <tbody>
              {#each running as a (a.alias)}
                {#if a.run}
                  <tr>
                    <td><a class="agent" style="--hue: {hue(a.alias) ?? 28}" href="/runs/{a.run.run_id}">{a.alias}</a></td>
                    <td class="mono muted">{sessionLabel(a.run.session)}</td>
                    <td class="mono muted"><a href="/journal?chain={a.run.correlation_id}">{short(a.run.correlation_id)}</a></td>
                    <td class="mono live">{since(a.run.started_at, now)}</td>
                  </tr>
                {/if}
              {/each}
            </tbody>
          </table>
        </div>
      </section>
    {/if}
  </div>
{/if}

<style>
  .needs {
    flex: 1 1 320px;
  }
  .agents {
    flex: 999 1 480px;
  }
  article {
    padding: 14px 18px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    border-bottom: 1px solid var(--line);
  }
  article:last-of-type {
    border-bottom: 0;
  }
  .meta {
    display: flex;
    justify-content: space-between;
    gap: 12px;
  }
  article p {
    overflow-wrap: anywhere;
    display: -webkit-box;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .actions {
    display: flex;
    gap: 16px;
  }
  .agent-row {
    padding: 10px 18px;
    display: flex;
    align-items: center;
    gap: 12px;
    border-bottom: 1px solid var(--line);
  }
  .agent-row:last-child {
    border-bottom: 0;
  }
  .agent-row:hover {
    background: var(--accent-soft);
    color: inherit;
  }
  .who {
    flex: 1;
    min-width: 0;
    display: flex;
    gap: 10px;
    align-items: baseline;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .who b {
    font-weight: 500;
  }
</style>
