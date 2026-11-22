<!-- What agents did: Gateway decisions, Runs, Broker and task changes (D42). -->
<script lang="ts">
  import { api } from "../lib/api";
  import { clock, firstLine, short } from "../lib/format";
  import { poll } from "../lib/poll.svelte";
  import { navigate, route } from "../lib/router.svelte";
  import type { Agent, JournalEntry } from "../lib/types";

  let entries = $state<JournalEntry[]>([]);
  let agents = $state<Agent[]>([]);
  let decision = $state<"" | "allow" | "deny">("");

  // One server-side filter, from the query string: links from other screens set it.
  const scope = $derived.by(() => {
    for (const key of ["chain", "run", "task", "agent"] as const) {
      const value = route.query.get(key);
      if (value) return { key, value };
    }
    return null;
  });

  poll(async () => {
    const query = scope ? `?${scope.key}=${encodeURIComponent(scope.value)}` : "";
    [entries, agents] = await Promise.all([api.get<JournalEntry[]>(`/journal${query}`), api.get<Agent[]>("/agents")]);
  }, 5000);

  const shown = $derived(
    entries
      .filter((e) => !decision || e.decision === decision)
      .slice()
      .reverse(),
  );
  const hue = (alias: string) => agents.find((a) => a.alias === alias)?.hue ?? null;

  function chooseAgent(alias: string) {
    navigate(alias ? `/journal?agent=${encodeURIComponent(alias)}` : "/journal", true);
  }
</script>

<header class="screen-header">
  <h1><span class="muted">Agents /</span> Journal</h1>
  <div class="filters">
    {#if scope && scope.key !== "agent"}
      <span class="chip mono small">{scope.key} {short(scope.value)} <a href="/journal" aria-label="Clear filter">×</a></span>
    {:else}
      <select aria-label="Agent" value={scope?.value ?? ""} onchange={(e) => chooseAgent(e.currentTarget.value)}>
        <option value="">All agents</option>
        {#each agents as a (a.alias)}<option value={a.alias}>{a.alias}</option>{/each}
      </select>
    {/if}
    <select aria-label="Gateway decision" bind:value={decision}>
      <option value="">All decisions</option>
      <option value="allow">allow</option>
      <option value="deny">deny</option>
    </select>
  </div>
</header>

<div class="screen-body">
  <section class="panel">
    <div class="table-wrap">
      <table>
        <thead>
          <tr><th>Time</th><th>Agent</th><th>Action</th><th>Path</th><th>Outcome</th><th>Reason</th><th>Run</th></tr>
        </thead>
        <tbody class="mono">
          {#each shown as e, i (i)}
            {@const bad = e.decision === "deny" || e.result === "error"}
            <tr>
              <td class="muted nowrap">{clock(e.timestamp)}</td>
              <td class="plain"><span class="agent" style="--hue: {hue(e.actor) ?? 28}">{e.actor}</span></td>
              <td>{e.action} <span class="muted small">{e.component.replace("_", " ")}</span></td>
              <td class="muted path">{e.path ?? ""}</td>
              <td class:failed={bad} class:muted={!bad}>{e.decision ?? e.result ?? ""}</td>
              <td class:failed={bad} class="reason" title={e.reason ?? undefined}>{e.reason ? firstLine(e.reason) : ""}</td>
              <td>{#if e.run_id}<a class="muted" href="/runs/{e.run_id}">{short(e.run_id)}</a>{/if}</td>
            </tr>
          {:else}
            <tr><td colspan="7" class="muted plain">Nothing recorded{scope ? " for this filter" : ""} yet.</td></tr>
          {/each}
        </tbody>
      </table>
    </div>
  </section>
</div>

<style>
  .filters {
    display: flex;
    gap: 12px;
    align-items: center;
  }
  select {
    width: auto;
  }
  .chip {
    padding: 4px 8px;
    border: 1px solid var(--line-strong);
  }
  .chip a {
    margin-left: 6px;
  }
  .nowrap {
    white-space: nowrap;
  }
  .plain {
    font-family: var(--sans);
    font-size: 13px;
  }
  .path,
  .reason {
    overflow-wrap: anywhere;
    max-width: 320px;
  }
</style>
