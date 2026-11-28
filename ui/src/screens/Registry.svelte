<script lang="ts">
  import Ring from "../components/Ring.svelte";
  import { look } from "../lib/agent";
  import { api } from "../lib/api";
  import { dollars } from "../lib/format";
  import { poll } from "../lib/poll.svelte";
  import { navigate } from "../lib/router.svelte";
  import { spendSince, startOfToday } from "../lib/runs";
  import type { Agent, RunRecord, Status } from "../lib/types";

  let agents = $state<Agent[] | null>(null);
  let status = $state<Status | null>(null);
  let runs = $state<RunRecord[]>([]);

  poll(async () => {
    [agents, status, runs] = await Promise.all([
      api.get<Agent[]>("/agents"),
      api.get<Status>("/status"),
      api.get<RunRecord[]>("/runs"),
    ]);
  }, 8000);

  function today(alias: string) {
    const spend = spendSince(
      runs.filter((r) => r.agent === alias),
      startOfToday(),
    );
    if (spend.runs === 0) return "—";
    return dollars(spend.cost) + (spendSince(runs, startOfToday()).partial ? "+" : "");
  }
</script>

<header class="screen-header">
  <h1><span class="muted">Agents /</span> Registry</h1>
  <a class="primary" href="/agents/new">New agent</a>
</header>

{#if agents}
  <div class="screen-body">
    <section class="panel">
      <div class="table-wrap">
        <table>
          <thead>
            <tr><th>Agent</th><th>State</th><th>Model</th><th>Access</th><th>Cost today</th></tr>
          </thead>
          <tbody>
            {#each agents as agent (agent.alias)}
              {@const state = look(status?.agents.find((a) => a.alias === agent.alias))}
              <tr class="link" onclick={() => navigate(`/agents/${agent.alias}`)}>
                <td>
                  <a class="who" href="/agents/{agent.alias}">
                    <Ring hue={agent.hue} {state} size={22} />
                    <b class="agent" style="--hue: {agent.hue}">{agent.alias}</b>
                    <span class="muted">{agent.title}</span>
                  </a>
                </td>
                <td class="mono" class:live={state === "running"} class:failed={state === "stuck"} class:muted={state === "idle" || state === "disabled"}>{state}</td>
                <td class="mono muted">{agent.model.provider}/{agent.model.id}</td>
                <td class="mono">{agent.access.length} of 5</td>
                <td class="mono">{today(agent.alias)}</td>
              </tr>
            {:else}
              <tr><td colspan="5" class="muted">No agents yet. <a class="live" href="/agents/new">Create the first one</a></td></tr>
            {/each}
          </tbody>
        </table>
      </div>
    </section>
  </div>
{/if}

<style>
  .primary {
    display: inline-block;
    color: #1a0f05;
  }
  .who {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .who b {
    font-weight: 500;
  }
</style>
