<!-- One Run: what the model saw and did, from Pi's session (D102). -->
<script lang="ts">
  import Confirm from "../components/Confirm.svelte";
  import { api } from "../lib/api";
  import { clock, dollars, sessionLabel, short, tokens } from "../lib/format";
  import { poll } from "../lib/poll.svelte";
  import { took } from "../lib/runs";
  import type { Agent, RunRecord, TranscriptEntry } from "../lib/types";

  let { id }: { id: string } = $props();

  let run = $state<RunRecord | null>(null);
  let agent = $state<Agent | null>(null);
  let steps = $state<TranscriptEntry[]>([]);
  let open = $state<Record<number, boolean>>({});
  let now = $state(Date.now());

  poll(async () => {
    // A finished Run does not change: stop asking once it is done.
    if (run && run.outcome !== "running") return;
    [run, steps] = await Promise.all([
      api.get<RunRecord>(`/runs/${id}`),
      api.get<TranscriptEntry[]>(`/runs/${id}/transcript`),
    ]);
    agent ??= await api.get<Agent>(`/agents/${run.agent}`).catch(() => null);
    now = Date.now();
  }, 3000);

  const calls = $derived(steps.filter((s) => s.kind === "tool_call").length);
  const failedCalls = $derived(steps.filter((s) => s.kind === "tool_result" && s.is_error).length);
  const LONG = 400;

  function time(at: string) {
    return new Date(at).toLocaleTimeString([], { hour12: false });
  }
</script>

{#if run}
  <header class="screen-header">
    <h1>
      <a class="muted" href="/agents">Registry /</a>
      <a class="agent" style="--hue: {agent?.hue ?? 28}" href="/agents/{run.agent}">{run.agent}</a>
      <span class="muted">/</span> Run {clock(run.started_at)}
    </h1>
    <div class="head-right">
      <span class="mono small" class:live={run.outcome === "running"} class:failed={run.outcome === "error"} class:muted={run.outcome === "stopped" || run.outcome === "ok"}>
        {run.outcome} · {took(run, now)}
      </span>
      {#if run.outcome === "running"}
        <Confirm label="Stop chain" question="Stop this chain? Every Run in it ends." confirm="Stop" danger onconfirm={() => api.post(`/chains/${run!.correlation_id}/stop`)} />
      {/if}
    </div>
  </header>

  <div class="screen-body">
    <section class="panel transcript">
      <div class="panel-head"><h2>Transcript</h2><span class="mono small muted">from the Pi session</span></div>
      {#each steps as step, i (i)}
        <div class="step" class:denied={step.kind === "tool_result" && step.is_error}>
          <span class="mono small muted">{time(step.at)}</span>
          <span class="mono small kind" class:live={step.kind === "answer"}>
            {step.kind === "tool_call" || step.kind === "tool_result" ? step.tool : step.kind === "prompt" ? "runtime" : step.kind === "answer" ? "model" : "thinking"}
          </span>
          <div class="content">
            {#if step.kind === "tool_call"}
              <pre class="mono">{JSON.stringify(step.arguments, null, 2)}</pre>
            {:else}
              {@const long = step.text.length > LONG}
              <p class:mono={step.kind !== "answer"} class:muted={step.kind === "thinking"} class="text">
                {long && !open[i] ? step.text.slice(0, LONG) + "…" : step.text}
              </p>
              {#if step.kind === "tool_result"}<span class="mono small" class:failed={step.is_error} class:muted={!step.is_error}>{step.is_error ? "error" : "ok"}{step.truncated ? " · cut at 16 KiB" : ""}</span>{/if}
              {#if long}<button class="text-button quiet" onclick={() => (open[i] = !open[i])}>{open[i] ? "Show less" : "Show all"}</button>{/if}
            {/if}
          </div>
        </div>
      {:else}
        <p class="empty">Pi wrote nothing for this Run.</p>
      {/each}
    </section>

    <section class="panel facts">
      <div class="panel-head"><h2>This Run</h2></div>
      <dl class="panel-body">
        {#if run.reason}<dt>Reason</dt><dd class="mono reason" class:failed={run.outcome === "error"}>{run.reason}</dd>{/if}
        <dt>Session</dt><dd class="mono">{sessionLabel(run.session)}</dd>
        <dt>Chain</dt><dd class="mono"><a href="/journal?chain={run.correlation_id}">{short(run.correlation_id)}</a></dd>
        <dt>Card</dt><dd class="mono">v{run.card_version}</dd>
        <dt>Tokens</dt><dd class="mono">{tokens(run.usage.tokens_in)} in · {tokens(run.usage.tokens_out)} out</dd>
        <dt>Cost</dt><dd class="mono">{dollars(run.usage.cost_micros)}</dd>
        <dt>Tool calls</dt><dd class="mono">{calls}{#if failedCalls} · <span class="failed">{failedCalls} failed</span>{/if}</dd>
        <dt>Journal</dt><dd class="mono"><a href="/journal?run={run.run_id}">{short(run.run_id)}</a></dd>
      </dl>
      <p class="panel-body muted small note">The transcript shows what the agent read. Only you can open it.</p>
    </section>
  </div>
{/if}

<style>
  .head-right {
    display: flex;
    gap: 16px;
    align-items: center;
    flex-wrap: wrap;
  }
  .transcript {
    flex: 999 1 560px;
  }
  .facts {
    flex: 1 1 260px;
  }
  .step {
    padding: 12px 18px;
    display: grid;
    grid-template-columns: 72px 110px 1fr;
    gap: 12px;
    border-top: 1px solid var(--line);
  }
  .step:first-of-type {
    border-top: 0;
  }
  .step.denied .kind {
    color: var(--danger);
  }
  .kind {
    overflow-wrap: anywhere;
  }
  .content {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
    align-items: flex-start;
  }
  .text {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  pre {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    color: var(--muted);
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
    min-width: 0;
  }
  .reason {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .note {
    border-top: 1px solid var(--line);
  }
  @media (max-width: 720px) {
    .step {
      grid-template-columns: 1fr;
      gap: 2px;
    }
  }
</style>
