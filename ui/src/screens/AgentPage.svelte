<script lang="ts">
  import Confirm from "../components/Confirm.svelte";
  import Ring from "../components/Ring.svelte";
  import { accessLabel } from "../lib/access";
  import { look } from "../lib/agent";
  import { api } from "../lib/api";
  import { clock, dollars, firstLine, since, sessionLabel } from "../lib/format";
  import { poll } from "../lib/poll.svelte";
  import { navigate } from "../lib/router.svelte";
  import { spendSince, startOfToday, took } from "../lib/runs";
  import type { Agent, AgentVersion, RunRecord, Status, Task } from "../lib/types";

  let { alias }: { alias: string } = $props();

  let agent = $state<Agent | null>(null);
  let versions = $state<AgentVersion[]>([]);
  let runs = $state<RunRecord[]>([]);
  let tasks = $state<Task[]>([]);
  let status = $state<Status | null>(null);
  let missing = $state(false);
  let now = $state(Date.now());

  poll(async () => {
    try {
      [agent, versions, runs, tasks, status] = await Promise.all([
        api.get<Agent>(`/agents/${alias}`),
        api.get<AgentVersion[]>(`/agents/${alias}/versions`),
        api.get<RunRecord[]>(`/runs?agent=${alias}`),
        api.get<Task[]>("/tasks"),
        api.get<Status>("/status"),
      ]);
      now = Date.now();
    } catch (e) {
      missing = true;
      throw e;
    }
  });

  const me = $derived(status?.agents.find((a) => a.alias === alias));
  const seen = $derived(look(me));
  const week = $derived(spendSince(runs, now - 7 * 86_400_000));
  const today = $derived(spendSince(runs, startOfToday()));
  const theirs = $derived(
    tasks.filter((t) => t.assignee === alias || t.members.includes(alias) || t.creator === alias),
  );
  const titleOf = (session: string) => tasks.find((t) => `task-${t.id}` === session)?.title;
  const role = (t: Task) => (t.assignee === alias ? "assignee" : t.creator === alias ? "creator" : "member");

  async function setEnabled(enabled: boolean) {
    await api.post(`/agents/${alias}/${enabled ? "enable" : "disable"}`);
    agent = await api.get<Agent>(`/agents/${alias}`);
  }

  async function remove() {
    await api.delete(`/agents/${alias}`);
    navigate("/agents");
  }
</script>

{#if agent}
  <header class="screen-header">
    <div class="title">
      <a class="muted" href="/agents">Registry /</a>
      <Ring hue={agent.hue} state={seen} size={24} />
      <h1 class="agent" style="--hue: {agent.hue}">{agent.alias}</h1>
      <span class="muted">{agent.title}</span>
      <span class="mono small" class:live={seen === "running"} class:failed={seen === "stuck"} class:muted={seen === "idle" || seen === "disabled"}>
        {seen}{#if me?.run} · {since(me.run.started_at, now)}{/if}
      </span>
    </div>
    <div class="actions">
      {#if agent.state === "active"}<a class="primary" href="/messages/{alias}">Message</a>{/if}
      <a class="outline" href="/agents/{alias}/edit">Edit</a>
      {#if agent.state === "active"}
        <Confirm label="Disable" question="Disable @{alias}? Its Run stops; mail waits." confirm="Disable" danger onconfirm={() => setEnabled(false)} />
      {:else}
        <button class="outline" onclick={() => setEnabled(true)}>Enable</button>
      {/if}
    </div>
  </header>

  <div class="screen-body">
    <section class="panel stats">
      <div><span class="muted">Runs today</span><b class="mono">{today.runs}{today.partial ? "+" : ""}</b></div>
      <div><span class="muted">Cost today</span><b class="mono">{dollars(today.cost)}{today.partial ? "+" : ""}</b></div>
      <div><span class="muted">Cost, 7 days</span><b class="mono">{dollars(week.cost)}{week.partial ? "+" : ""}</b></div>
      <div><span class="muted">Failed runs, 7 days</span><b class="mono" class:failed={week.failed > 0}>{week.failed}{week.partial ? "+" : ""}</b></div>
    </section>

    <div class="main-col">
      <section class="panel">
        <div class="panel-head"><h2>Runs</h2><a class="mono small muted" href="/journal?agent={alias}">Journal</a></div>
        <div class="table-wrap">
          <table>
            <thead><tr><th>Started</th><th>About</th><th>Took</th><th>Outcome</th><th>Cost</th></tr></thead>
            <tbody class="mono">
              {#each runs as run (run.run_id)}
                <tr class="link" onclick={() => navigate(`/runs/${run.run_id}`)}>
                  <td><a href="/runs/{run.run_id}">{clock(run.started_at)}</a></td>
                  <td class="about">{titleOf(run.session) ?? sessionLabel(run.session)}</td>
                  <td>{took(run, now)}</td>
                  <td class:live={run.outcome === "running"} class:failed={run.outcome === "error"} class:muted={run.outcome === "stopped"} title={run.reason ?? undefined}>
                    {run.outcome}{#if run.reason} · {firstLine(run.reason)}{/if}
                  </td>
                  <td>{dollars(run.usage.cost_micros)}</td>
                </tr>
              {:else}
                <tr><td colspan="5" class="muted plain">No Runs yet.</td></tr>
              {/each}
            </tbody>
          </table>
        </div>
      </section>

      {#if theirs.length}
        <section class="panel">
          <div class="panel-head"><h2>Tasks</h2></div>
          {#each theirs as task (task.id)}
            <a class="task" href="/tasks/{task.id}">
              <span>{task.title}</span>
              <span class="mono small muted">{role(task)}</span>
              <span class="mono small" class:live={task.status === "needs_human"} class:muted={task.status !== "needs_human"}>{task.status.replace("_", " ")}</span>
            </a>
          {/each}
        </section>
      {/if}
    </div>

    <section class="panel card">
      <div class="panel-head">
        <h2>Card</h2>
        <span class="mono small muted">
          v{agent.version}{#if versions[0]} · {versions.length > 1 ? "edited" : "created"} {since(versions[0].saved_at, now)} ago by @{versions[0].author}{/if}
        </span>
      </div>
      <dl class="panel-body">
        <dt>Model</dt>
        <dd class="mono">{agent.model.provider}/{agent.model.id}</dd>
        <dt>Access</dt>
        <dd>{#each agent.access as a}<span class="chip">{accessLabel(a)}</span>{:else}<span class="muted">Only its own messages</span>{/each}</dd>
        <dt>May write to</dt>
        <dd>{agent.allow_message_to.length ? agent.allow_message_to.map((a) => `@${a}`).join(", ") : "nobody"}</dd>
        <dt>Prompt</dt>
        <dd class="prompt">{agent.prompt}</dd>
      </dl>
      <div class="panel-body delete">
        <p class="muted small">Its runs, messages and journal stay after deletion, and so does its alias.</p>
        <Confirm label="Delete agent" question="Delete @{alias} for good?" confirm="Delete" danger onconfirm={remove} />
      </div>
    </section>
  </div>
{:else if missing}
  <header class="screen-header"><h1>No agent @{alias}</h1></header>
  <p class="empty"><a class="live" href="/agents">Registry</a></p>
{/if}

<style>
  .title,
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 12px;
  }
  .primary {
    color: #1a0f05;
  }
  .stats {
    display: flex;
    flex-wrap: wrap;
  }
  .stats div {
    flex: 1 1 160px;
    padding: 14px 18px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    border-right: 1px solid var(--line);
  }
  .stats div:last-child {
    border-right: 0;
  }
  .stats b {
    font-size: 15px;
    font-weight: 400;
  }
  .main-col {
    flex: 999 1 520px;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 24px;
  }
  .card {
    flex: 1 1 300px;
  }
  .about {
    white-space: nowrap;
  }
  .plain {
    font-family: var(--sans);
  }
  .task {
    padding: 10px 18px;
    display: flex;
    gap: 12px;
    align-items: baseline;
    border-top: 1px solid var(--line);
  }
  .task span:first-child {
    flex: 1;
  }
  dl {
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  dt {
    margin-top: 10px;
    font-size: 11px;
    color: var(--muted);
  }
  dt:first-child {
    margin-top: 0;
  }
  dd {
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chip {
    padding: 2px 8px;
    border: 1px solid var(--line-strong);
    font-size: 12px;
  }
  .prompt {
    white-space: pre-wrap;
    display: block;
    font-family: var(--mono);
    font-size: 12px;
  }
  .delete {
    border-top: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    gap: 10px;
    align-items: flex-start;
  }
</style>
