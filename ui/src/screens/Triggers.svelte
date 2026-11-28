<!-- What wakes agents besides you (D35): schedules, sent from @scheduler (D107), and
     adapters, external senders such as @github that bring events in (D110). -->
<script lang="ts">
  import Confirm from "../components/Confirm.svelte";
  import { api, ApiError } from "../lib/api";
  import { clock, since } from "../lib/format";
  import { poll } from "../lib/poll.svelte";
  import type { Adapter, Agent, NewSchedule, Schedule } from "../lib/types";

  let schedules = $state<Schedule[] | null>(null);
  let adapters = $state<Adapter[]>([]);
  let agents = $state<Agent[]>([]);
  let now = $state(Date.now());

  poll(async () => {
    [schedules, adapters, agents] = await Promise.all([
      api.get<Schedule[]>("/schedules"),
      api.get<Adapter[]>("/adapters"),
      api.get<Agent[]>("/agents"),
    ]);
    now = Date.now();
  }, 10000);

  const active = $derived(agents.filter((a) => a.state === "active"));
  const hue = (alias: string) => agents.find((a) => a.alias === alias)?.hue ?? 28;
  const message = (e: unknown, fallback: string) => (e instanceof ApiError ? (e.body.detail ?? e.body.kind) : fallback);

  // ---- schedules ----

  // The form starts in your own zone: a schedule reads its cron there.
  const localZone = Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  let scheduling = $state(false);
  let draft = $state<NewSchedule>({ agent: "", cron: "0 9 * * 1-5", timezone: localZone, subject: null, body: "" });
  let scheduleError = $state<string | null>(null);
  let busy = $state(false);

  async function createSchedule(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    scheduleError = null;
    try {
      await api.post("/schedules", { ...draft, subject: draft.subject || null });
      scheduling = false;
      draft = { ...draft, body: "", subject: null };
      schedules = await api.get<Schedule[]>("/schedules");
    } catch (e) {
      scheduleError = message(e, "Could not save.");
    } finally {
      busy = false;
    }
  }

  async function toggle(s: Schedule) {
    await api.request("PATCH", `/schedules/${s.id}`, { enabled: !s.enabled });
    schedules = await api.get<Schedule[]>("/schedules");
  }

  async function removeSchedule(s: Schedule) {
    await api.delete(`/schedules/${s.id}`);
    schedules = await api.get<Schedule[]>("/schedules");
  }

  // ---- adapters ----

  let adding = $state(false);
  let adapter = $state<Adapter>({ alias: "", title: "", allow_message_to: [] });
  let adapterError = $state<string | null>(null);
  /** A credential just issued: shown once, then gone. */
  let issued = $state<{ alias: string; credential: string } | null>(null);
  let copied = $state(false);

  async function createAdapter(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    adapterError = null;
    try {
      const { credential } = await api.post<{ credential: string }>("/adapters", adapter);
      issued = { alias: adapter.alias, credential };
      adding = false;
      adapter = { alias: "", title: "", allow_message_to: [] };
      adapters = await api.get<Adapter[]>("/adapters");
    } catch (e) {
      adapterError = message(e, "Could not create.");
    } finally {
      busy = false;
    }
  }

  async function rotate(a: Adapter) {
    const { credential } = await api.post<{ credential: string }>(`/adapters/${a.alias}/rotate`);
    issued = { alias: a.alias, credential };
    copied = false;
  }

  async function removeAdapter(a: Adapter) {
    await api.delete(`/adapters/${a.alias}`);
    adapters = await api.get<Adapter[]>("/adapters");
  }

  async function copy() {
    if (!issued) return;
    await navigator.clipboard.writeText(issued.credential);
    copied = true;
  }

  function pick(list: string[], alias: string, on: boolean): string[] {
    return on ? [...list, alias] : list.filter((a) => a !== alias);
  }
</script>

<header class="screen-header"><h1>Triggers</h1></header>

<div class="screen-body">
  <section class="panel">
    <div class="panel-head">
      <h2>Schedules</h2>
      {#if !scheduling}<button class="text-button" onclick={() => (scheduling = true)}>+ New schedule</button>{/if}
    </div>

    {#if scheduling}
      <form class="panel-body grid add" onsubmit={createSchedule}>
        <label>
          Agent
          <select bind:value={draft.agent} required>
            <option value="" disabled>Choose</option>
            {#each active as a (a.alias)}<option value={a.alias}>{a.alias}</option>{/each}
          </select>
        </label>
        <label>
          Cron
          <input class="mono" bind:value={draft.cron} required autocomplete="off" />
          <span class="hint">minute hour day month weekday · "0 9 * * 1-5" is 09:00 on weekdays</span>
        </label>
        <label>
          Time zone
          <input class="mono" bind:value={draft.timezone} required autocomplete="off" />
          <span class="hint">IANA name; the cron reads in its local time</span>
        </label>
        <label>
          Subject
          <input bind:value={draft.subject} autocomplete="off" />
        </label>
        <label class="wide">
          Message
          <textarea rows="4" bind:value={draft.body} required></textarea>
          <span class="hint">Each fire starts a new thread. While the agent has not read the last one, the next fire is skipped.</span>
        </label>
        <div class="wide actions">
          {#if scheduleError}<span class="error" role="alert">{scheduleError}</span>{/if}
          <button type="button" class="outline" onclick={() => (scheduling = false)}>Cancel</button>
          <button class="primary" disabled={busy}>Create schedule</button>
        </div>
      </form>
    {/if}

    {#if schedules}
      <div class="table-wrap">
        <table>
          <thead><tr><th>Agent</th><th>When</th><th>Message</th><th>Next</th><th>Last</th><th></th></tr></thead>
          <tbody>
            {#each schedules as s (s.id)}
              <tr class:off={!s.enabled}>
                <td><a class="agent" style="--hue: {hue(s.agent)}" href="/messages/{s.agent}">{s.agent}</a></td>
                <td class="mono">{s.cron} <span class="muted">{s.timezone}</span></td>
                <td class="clip" title={s.body}>{#if s.subject}<b>{s.subject}</b> · {/if}{s.body}</td>
                <td class="mono">{s.next_fire_at ? clock(s.next_fire_at) : "off"}</td>
                <td class="mono muted">{s.last_fired_at ? since(s.last_fired_at, now) : "—"}</td>
                <td class="row-actions">
                  <button class="text-button quiet" onclick={() => toggle(s)}>{s.enabled ? "Disable" : "Enable"}</button>
                  <Confirm label="Delete" question="Delete this schedule?" confirm="Delete" danger onconfirm={() => removeSchedule(s)} />
                </td>
              </tr>
            {:else}
              <tr><td colspan="6" class="muted">No schedules yet.</td></tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </section>

  <section class="panel">
    <div class="panel-head">
      <h2>Adapters</h2>
      {#if !adding}<button class="text-button" onclick={() => (adding = true)}>+ New adapter</button>{/if}
    </div>

    {#if issued}
      <div class="panel-body issued" role="status">
        <p>The credential of <span class="mono">@{issued.alias}</span>. It is shown this once: put it into the adapter now.</p>
        <div class="secret">
          <code class="mono">{issued.credential}</code>
          <button class="outline" onclick={copy}>{copied ? "Copied" : "Copy"}</button>
          <button class="text-button quiet" onclick={() => ((issued = null), (copied = false))}>Done</button>
        </div>
      </div>
    {/if}

    {#if adding}
      <form class="panel-body grid add" onsubmit={createAdapter}>
        <label>
          Alias
          <input bind:value={adapter.alias} required pattern="[a-z0-9][a-z0-9-]*" placeholder="github" autocomplete="off" />
          <span class="hint">Its events come from @{adapter.alias || "alias"}. Never given again once deleted.</span>
        </label>
        <label>
          Title
          <input bind:value={adapter.title} required placeholder="Issues of the billing repo" autocomplete="off" />
        </label>
        <fieldset class="wide">
          <legend>May write to</legend>
          {#each active as a (a.alias)}
            <label class="check">
              <input
                type="checkbox"
                checked={adapter.allow_message_to.includes(a.alias)}
                onchange={(e) => (adapter.allow_message_to = pick(adapter.allow_message_to, a.alias, e.currentTarget.checked))}
              />
              <span>@{a.alias}</span>
            </label>
          {/each}
        </fieldset>
        <p class="wide hint">
          The adapter is your program: it posts events to <span class="mono">/v1/messages</span> with its credential. Agents see its text marked external, as data and never instructions, and cannot reply to it.
        </p>
        <div class="wide actions">
          {#if adapterError}<span class="error" role="alert">{adapterError}</span>{/if}
          <button type="button" class="outline" onclick={() => (adding = false)}>Cancel</button>
          <button class="primary" disabled={busy}>Create adapter</button>
        </div>
      </form>
    {/if}

    <div class="table-wrap">
      <table>
        <thead><tr><th>Adapter</th><th>What it brings</th><th>May write to</th><th></th></tr></thead>
        <tbody>
          {#each adapters as a (a.alias)}
            <tr>
              <td class="mono">@{a.alias}</td>
              <td>{a.title}</td>
              <td class="mono small">{a.allow_message_to.map((t) => `@${t}`).join(", ") || "nobody"}</td>
              <td class="row-actions">
                <Confirm label="New credential" question="Replace its credential? The old one stops at once." confirm="Replace" onconfirm={() => rotate(a)} />
                <Confirm label="Delete" question="Delete @{a.alias}?" confirm="Delete" danger onconfirm={() => removeAdapter(a)} />
              </td>
            </tr>
          {:else}
            <tr><td colspan="4" class="muted">No adapters yet.</td></tr>
          {/each}
        </tbody>
      </table>
    </div>
  </section>
</div>

<style>
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
    gap: 18px;
  }
  .add {
    border-bottom: 1px solid var(--line);
  }
  .wide {
    grid-column: 1 / -1;
  }
  .hint {
    font-size: 11px;
    color: var(--muted);
    margin: 0;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    align-items: center;
    gap: 12px;
  }
  fieldset {
    margin: 0;
    padding: 0;
    border: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 8px 18px;
  }
  legend {
    padding: 0 0 6px;
    font-size: 12px;
    color: var(--muted);
  }
  .check {
    flex-direction: row;
    align-items: center;
    gap: 8px;
    color: var(--text);
    font-size: 13px;
  }
  .issued {
    display: flex;
    flex-direction: column;
    gap: 10px;
    border-bottom: 1px solid var(--line);
    background: var(--accent-soft);
  }
  .issued p {
    margin: 0;
  }
  .secret {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 12px;
  }
  .secret code {
    padding: 6px 10px;
    background: var(--bg);
    border: 1px solid var(--line);
    overflow-wrap: anywhere;
  }
  .clip {
    max-width: 360px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .clip b {
    font-weight: 500;
  }
  .row-actions {
    white-space: nowrap;
    display: flex;
    gap: 16px;
    justify-content: flex-end;
  }
  .off td:not(.row-actions) {
    opacity: 0.55;
  }
  .panel + .panel {
    margin-top: 16px;
  }
</style>
