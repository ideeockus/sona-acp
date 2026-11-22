<!-- A task: what it is about, its Runs and history, and the operator's answer. An answer
     to needs_human is a message to the assignee: a note would wake nobody (docs/ui.md). -->
<script lang="ts">
  import Confirm from "../components/Confirm.svelte";
  import { api, ApiError } from "../lib/api";
  import { clock, dollars, since } from "../lib/format";
  import { poll } from "../lib/poll.svelte";
  import { took } from "../lib/runs";
  import { statusLabel } from "../lib/tasks";
  import type { Change, RunRecord, TaskView } from "../lib/types";

  let { id }: { id: string } = $props();

  let view = $state<TaskView | null>(null);
  let runs = $state<RunRecord[]>([]);
  let missing = $state(false);
  let answer = $state("");
  let note = $state("");
  let error = $state<string | null>(null);
  let busy = $state(false);
  let sent = $state(false);
  let now = $state(Date.now());

  poll(async () => {
    try {
      [view, runs] = await Promise.all([api.get<TaskView>(`/tasks/${id}`), api.get<RunRecord[]>("/runs")]);
      now = Date.now();
    } catch (e) {
      missing = true;
      throw e;
    }
  }, 5000);

  const task = $derived(view?.task);
  const closed = $derived(task?.status === "done" || task?.status === "cancelled");
  const theirRuns = $derived(runs.filter((r) => r.session === `task-${id}`));

  function describe(change: Change): string {
    switch (change.kind) {
      case "created":
        return "created";
      case "status":
        return `${statusLabel(change.from)} → ${statusLabel(change.to)}`;
      case "result":
        return `result: ${change.result.text}`;
      case "note":
        return `note: ${change.text}`;
    }
  }

  async function run(action: () => Promise<unknown>) {
    busy = true;
    error = null;
    try {
      await action();
      view = await api.get<TaskView>(`/tasks/${id}`);
    } catch (e) {
      error = e instanceof ApiError ? (e.body.detail ?? e.body.kind) : "Something went wrong.";
    } finally {
      busy = false;
    }
  }

  function sendAnswer(event: SubmitEvent) {
    event.preventDefault();
    if (!task?.assignee) return;
    const to = task.assignee;
    void run(async () => {
      await api.post("/messages", { to, body: answer.trim(), subject: null, task_id: id, in_reply_to: null });
      answer = "";
      sent = true;
    });
  }

  function addNote(event: SubmitEvent) {
    event.preventDefault();
    void run(async () => {
      await api.request("PATCH", `/tasks/${id}`, { status: null, result: null, note: note.trim() });
      note = "";
    });
  }

  const close = (status: "done" | "cancelled") =>
    run(() => api.request("PATCH", `/tasks/${id}`, { status, result: null, note: null }));
</script>

{#if view && task}
  <header class="screen-header">
    <h1><a class="muted" href="/tasks">Tasks /</a> {task.title}</h1>
    <div class="head-right">
      <span class="mono" class:live={task.status === "needs_human"} class:muted={closed}>{statusLabel(task.status)}</span>
      {#if !closed}
        <Confirm label="Done" question="Close as done?" confirm="Close" onconfirm={() => close("done")} />
        <Confirm label="Cancel task" question="Cancel this task?" confirm="Cancel task" danger onconfirm={() => close("cancelled")} />
      {/if}
    </div>
  </header>

  <div class="screen-body">
    <div class="main-col">
      {#if task.status === "needs_human" && task.assignee}
        <section class="panel">
          <div class="panel-head"><h2>Your answer</h2><span class="mono small muted">a message to @{task.assignee} about this task</span></div>
          <form class="panel-body" onsubmit={sendAnswer}>
            <textarea rows="3" bind:value={answer} placeholder="Answer @{task.assignee}…" aria-label="Answer"></textarea>
            <div class="row">
              {#if sent}<span class="mono small muted">Sent. It wakes @{task.assignee}.</span>{/if}
              <button class="primary" disabled={busy || !answer.trim()}>Send answer</button>
            </div>
          </form>
        </section>
      {/if}

      <section class="panel">
        <div class="panel-head"><h2>Context</h2></div>
        <p class="panel-body pre">{task.context || "—"}</p>
        {#if task.result}
          <div class="panel-head sub"><h2>Result</h2></div>
          <div class="panel-body">
            <p class="pre">{task.result.text}</p>
            {#each task.result.links as link (link)}<p class="mono small muted">{link}</p>{/each}
          </div>
        {/if}
      </section>

      <section class="panel">
        <div class="panel-head"><h2>Runs</h2><a class="mono small muted" href="/journal?task={id}">Journal</a></div>
        <div class="table-wrap">
          <table>
            <thead><tr><th>Agent</th><th>Started</th><th>Took</th><th>Outcome</th><th>Cost</th></tr></thead>
            <tbody class="mono">
              {#each theirRuns as r (r.run_id)}
                <tr>
                  <td><a href="/runs/{r.run_id}">{r.agent}</a></td>
                  <td>{clock(r.started_at)}</td>
                  <td>{took(r, now)}</td>
                  <td class:live={r.outcome === "running"} class:failed={r.outcome === "error"}>{r.outcome}</td>
                  <td>{dollars(r.usage.cost_micros)}</td>
                </tr>
              {:else}
                <tr><td colspan="5" class="muted plain">No Runs among the latest.</td></tr>
              {/each}
            </tbody>
          </table>
        </div>
      </section>

      <section class="panel">
        <div class="panel-head"><h2>History</h2></div>
        <div class="table-wrap">
          <table>
            <tbody>
              {#each view.history as h, i (i)}
                <tr>
                  <td class="mono muted narrow">{clock(h.at)}</td>
                  <td class="mono narrow">@{h.who}</td>
                  <td class="pre">{describe(h.change)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
        {#if !closed}
          <form class="panel-body note" onsubmit={addNote}>
            <input bind:value={note} placeholder="Add a note to the history" aria-label="Note" />
            <button class="outline" disabled={busy || !note.trim()}>Add note</button>
          </form>
        {/if}
      </section>
      {#if error}<p class="error" role="alert">{error}</p>{/if}
    </div>

    <section class="panel details">
      <div class="panel-head"><h2>Details</h2></div>
      <dl class="panel-body">
        <dt>Assignee</dt><dd class="mono">{task.assignee ? `@${task.assignee}` : "—"}</dd>
        <dt>Members</dt><dd class="mono">{task.members.length ? task.members.map((m) => `@${m}`).join(", ") : "—"}</dd>
        <dt>Created</dt><dd class="mono">{clock(task.created_at)} by @{task.creator}</dd>
        {#if task.deadline}<dt>Deadline</dt><dd class="mono">{clock(task.deadline)} · {since(task.deadline, now)}</dd>{/if}
        {#if task.parent_id}<dt>Parent</dt><dd class="mono"><a href="/tasks/{task.parent_id}">task</a></dd>{/if}
      </dl>
    </section>
  </div>
{:else if missing}
  <header class="screen-header"><h1>No such task</h1></header>
  <p class="empty"><a class="live" href="/tasks">Tasks</a></p>
{/if}

<style>
  .head-right {
    display: flex;
    gap: 12px;
    align-items: center;
    flex-wrap: wrap;
  }
  .main-col {
    flex: 999 1 520px;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 24px;
  }
  .details {
    flex: 1 1 260px;
  }
  .pre {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .sub {
    border-top: 1px solid var(--line);
  }
  form {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .row {
    display: flex;
    gap: 12px;
    align-items: center;
    justify-content: flex-end;
  }
  .note {
    flex-direction: row;
    border-top: 1px solid var(--line);
  }
  .note button {
    white-space: nowrap;
  }
  .narrow {
    width: 1%;
    white-space: nowrap;
  }
  .plain {
    font-family: var(--sans);
  }
  dl {
    margin: 0;
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 6px 16px;
  }
  dt {
    color: var(--muted);
    font-size: 12px;
  }
  dd {
    margin: 0;
  }
</style>
