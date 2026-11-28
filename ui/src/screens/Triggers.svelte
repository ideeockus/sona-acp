<!-- Schedules: a message to an agent on cron, from @scheduler (D107). Each fire starts
     a thread of its own; a fire waits while the previous one is unread. -->
<script lang="ts">
  import Confirm from "../components/Confirm.svelte";
  import { api, ApiError } from "../lib/api";
  import { clock, since } from "../lib/format";
  import { poll } from "../lib/poll.svelte";
  import type { Agent, NewSchedule, Schedule } from "../lib/types";

  let schedules = $state<Schedule[] | null>(null);
  let agents = $state<Agent[]>([]);
  let now = $state(Date.now());

  poll(async () => {
    [schedules, agents] = await Promise.all([api.get<Schedule[]>("/schedules"), api.get<Agent[]>("/agents")]);
    now = Date.now();
  }, 10000);

  // The form starts in your own zone: a schedule reads its cron there.
  const localZone = Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  let open = $state(false);
  let draft = $state<NewSchedule>({ agent: "", cron: "0 9 * * 1-5", timezone: localZone, subject: null, body: "" });
  let error = $state<string | null>(null);
  let busy = $state(false);

  const hue = (alias: string) => agents.find((a) => a.alias === alias)?.hue ?? 28;

  async function refresh() {
    schedules = await api.get<Schedule[]>("/schedules");
  }

  async function create(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    error = null;
    try {
      await api.post("/schedules", { ...draft, subject: draft.subject || null });
      open = false;
      draft = { ...draft, body: "", subject: null };
      await refresh();
    } catch (e) {
      error = e instanceof ApiError ? (e.body.detail ?? e.body.kind) : "Could not save.";
    } finally {
      busy = false;
    }
  }

  async function toggle(s: Schedule) {
    await api.request("PATCH", `/schedules/${s.id}`, { enabled: !s.enabled });
    await refresh();
  }

  async function remove(s: Schedule) {
    await api.delete(`/schedules/${s.id}`);
    await refresh();
  }
</script>

<header class="screen-header">
  <h1>Triggers</h1>
  {#if !open}<button class="primary" onclick={() => (open = true)}>New schedule</button>{/if}
</header>

<div class="screen-body">
  {#if open}
    <form class="panel" onsubmit={create}>
      <div class="panel-head"><h2>New schedule</h2><span class="mono small muted">from @scheduler</span></div>
      <div class="panel-body grid">
        <label>
          Agent
          <select bind:value={draft.agent} required>
            <option value="" disabled>Choose</option>
            {#each agents.filter((a) => a.state === "active") as a (a.alias)}<option value={a.alias}>{a.alias}</option>{/each}
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
          {#if error}<span class="error" role="alert">{error}</span>{/if}
          <button type="button" class="outline" onclick={() => (open = false)}>Cancel</button>
          <button class="primary" disabled={busy}>Create schedule</button>
        </div>
      </div>
    </form>
  {/if}

  {#if schedules}
    <section class="panel">
      <div class="table-wrap">
        <table>
          <thead><tr><th>Agent</th><th>When</th><th>Message</th><th>Next</th><th>Last</th><th></th></tr></thead>
          <tbody>
            {#each schedules as s (s.id)}
              <tr class:off={!s.enabled}>
                <td><a class="agent" style="--hue: {hue(s.agent)}" href="/messages/{s.agent}">{s.agent}</a></td>
                <td class="mono">{s.cron} <span class="muted">{s.timezone}</span></td>
                <td class="message" title={s.body}>{#if s.subject}<b>{s.subject}</b> · {/if}{s.body}</td>
                <td class="mono">{s.next_fire_at ? clock(s.next_fire_at) : "off"}</td>
                <td class="mono muted">{s.last_fired_at ? since(s.last_fired_at, now) : "—"}</td>
                <td class="row-actions">
                  <button class="text-button quiet" onclick={() => toggle(s)}>{s.enabled ? "Disable" : "Enable"}</button>
                  <Confirm label="Delete" question="Delete this schedule?" confirm="Delete" danger onconfirm={() => remove(s)} />
                </td>
              </tr>
            {:else}
              <tr><td colspan="6" class="muted">No schedules yet.</td></tr>
            {/each}
          </tbody>
        </table>
      </div>
    </section>
  {/if}
</div>

<style>
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
    gap: 18px;
  }
  .wide {
    grid-column: 1 / -1;
  }
  .hint {
    font-size: 11px;
    color: var(--muted);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    align-items: center;
    gap: 12px;
  }
  .message {
    max-width: 360px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .message b {
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
  form {
    margin-bottom: 16px;
  }
</style>
