<!-- Creates an agent, or saves the next version of its card (D95, D101). -->
<script lang="ts">
  import { ACCESS } from "../lib/access";
  import { api, ApiError } from "../lib/api";
  import { navigate } from "../lib/router.svelte";
  import { session } from "../lib/session.svelte";
  import type { Access, Agent, AgentCard } from "../lib/types";

  let { alias = null }: { alias?: string | null } = $props();
  const editing = $derived(alias !== null);

  let card = $state<AgentCard>({
    alias: "",
    title: "",
    model: { provider: "", id: "" },
    access: ["read_files", "send_messages"],
    allow_message_to: [],
    prompt: "",
    hue: 28,
  });
  let version = $state(0);
  let others = $state<string[]>([]);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let loaded = $state(false);

  $effect(() => {
    void api.get<Agent[]>("/agents").then((all) => {
      loaded = !editing;
      others = all.map((a) => a.alias).filter((a) => a !== alias);
      const current = all.find((a) => a.alias === alias);
      if (current) {
        const { version: v, state: _, ...rest } = current;
        card = rest;
        version = v;
        loaded = true;
      }
    });
  });

  const recipients = $derived([...others, ...(session.alias ? [session.alias] : [])]);

  function toggle<T>(list: T[], item: T, on: boolean): T[] {
    return on ? [...list, item] : list.filter((x) => x !== item);
  }

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    error = null;
    // Groups in their usual order, whatever order they were ticked in.
    const access = ACCESS.map((a) => a.key).filter((k) => card.access.includes(k));
    try {
      if (editing) await api.put(`/agents/${alias}`, { ...card, access, version });
      else await api.post("/agents", { ...card, access });
      navigate(`/agents/${card.alias}`);
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        error = "Someone saved this card since you opened it. Reload to see their version.";
      } else {
        error = e instanceof ApiError ? (e.body.detail ?? e.body.kind) : "Could not save.";
      }
    } finally {
      busy = false;
    }
  }
</script>

<header class="screen-header">
  <h1><a class="muted" href="/agents">Registry /</a> {editing ? `Edit @${alias}` : "New agent"}</h1>
  {#if editing}<span class="mono small muted">saves version {version + 1}</span>{/if}
</header>

{#if loaded}
  <form class="screen-body" onsubmit={submit}>
    <section class="panel">
      <div class="panel-head"><h2>Identity</h2></div>
      <div class="panel-body grid">
        <label>
          Alias
          <input bind:value={card.alias} disabled={editing} required pattern="[a-z0-9][a-z0-9-]*" autocomplete="off" />
          <span class="hint">{editing ? "An alias never changes." : "Lowercase letters, digits and dashes. Unique, and never given again."}</span>
        </label>
        <label>
          Title
          <input bind:value={card.title} required />
          <span class="hint">One line other agents see.</span>
        </label>
        <label>
          Provider
          <input bind:value={card.model.provider} required placeholder="anthropic" autocomplete="off" />
        </label>
        <label>
          Model
          <input bind:value={card.model.id} required placeholder="claude-sonnet-5-5" autocomplete="off" />
          <span class="hint">As named in models.json.</span>
        </label>
        <label class="wide">
          Color
          <input type="range" min="0" max="359" bind:value={card.hue} />
          <span class="samples">
            <span data-theme="dark" class="sample agent" style="--hue: {card.hue}">{card.alias || "agent"} on dark</span>
            <span data-theme="light" class="sample agent" style="--hue: {card.hue}">{card.alias || "agent"} on light</span>
          </span>
          <span class="hint">You pick the hue; brightness is set per theme so the name reads in both.</span>
        </label>
      </div>
    </section>

    <section class="panel">
      <div class="panel-head"><h2>Prompt</h2></div>
      <div class="panel-body">
        <textarea bind:value={card.prompt} rows="8" aria-label="Prompt"></textarea>
      </div>
    </section>

    <section class="panel">
      <div class="panel-head"><h2>Grant</h2><span class="mono small muted">only what the agent needs</span></div>
      <div class="panel-body checks">
        {#each ACCESS as group (group.key)}
          <label class="check">
            <input
              type="checkbox"
              checked={card.access.includes(group.key)}
              onchange={(e) => (card.access = toggle<Access>(card.access, group.key, e.currentTarget.checked))}
            />
            <span><b>{group.label}</b> <span class="mono small muted">{group.tools}</span><br /><span class="hint">{group.note}</span></span>
          </label>
        {/each}
        <p class="hint">Reading its own messages is always allowed: that is how an agent learns why it was woken.</p>
      </div>
      <div class="panel-head sub"><h2>May write to</h2></div>
      <div class="panel-body recipients">
        {#each recipients as r (r)}
          <label class="check inline">
            <input
              type="checkbox"
              checked={card.allow_message_to.includes(r)}
              onchange={(e) => (card.allow_message_to = toggle(card.allow_message_to, r, e.currentTarget.checked))}
            />
            <span>@{r}{#if r === session.alias}<span class="muted you">you</span>{/if}</span>
          </label>
        {:else}
          <p class="hint">No other agents yet.</p>
        {/each}
      </div>
    </section>

    <div class="submit">
      {#if error}<p class="error" role="alert">{error}</p>{/if}
      <a class="outline" href={editing ? `/agents/${alias}` : "/agents"}>Cancel</a>
      <button class="primary" disabled={busy}>{editing ? "Save" : "Create agent"}</button>
    </div>
  </form>
{/if}

<style>
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
    gap: 18px;
  }
  .wide {
    grid-column: 1 / -1;
  }
  .hint {
    font-size: 11px;
    color: var(--muted);
  }
  input[type="range"] {
    padding: 0;
    height: 6px;
    border: 0;
    appearance: none;
    background: linear-gradient(
      to right,
      hsl(0 60% 60%),
      hsl(60 60% 60%),
      hsl(120 60% 60%),
      hsl(180 60% 60%),
      hsl(240 60% 60%),
      hsl(300 60% 60%),
      hsl(359 60% 60%)
    );
  }
  input[type="range"]::-webkit-slider-thumb {
    appearance: none;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: var(--text);
    border: 2px solid var(--bg);
  }
  input[type="range"]::-moz-range-thumb {
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: var(--text);
    border: 2px solid var(--bg);
  }
  .samples {
    display: flex;
    gap: 12px;
  }
  .sample {
    color: hsl(var(--hue) var(--agent-s) var(--agent-l));
    padding: 6px 12px;
    font-weight: 500;
    font-size: 13px;
    background: var(--bg);
    border: 1px solid var(--line);
  }
  textarea {
    font-family: var(--mono);
    font-size: 12px;
    resize: vertical;
  }
  .checks,
  .recipients {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .recipients {
    flex-direction: row;
    flex-wrap: wrap;
    gap: 18px;
  }
  .check {
    flex-direction: row;
    align-items: flex-start;
    gap: 10px;
    color: var(--text);
    font-size: 13px;
  }
  .check input {
    margin-top: 3px;
  }
  .check b {
    font-weight: 500;
  }
  .you {
    margin-left: 6px;
  }
  .sub {
    border-top: 1px solid var(--line);
  }
  .submit {
    flex: 1 1 100%;
    display: flex;
    gap: 12px;
    align-items: center;
    justify-content: flex-end;
    flex-wrap: wrap;
  }
</style>
