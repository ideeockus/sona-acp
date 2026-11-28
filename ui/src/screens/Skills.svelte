<!-- The skill library (D109): from git or proposed by agents, enabled after review.
     An agent sees a skill only when its card lists it and it is enabled here. -->
<script lang="ts">
  import Confirm from "../components/Confirm.svelte";
  import { api, ApiError } from "../lib/api";
  import { bytes, since } from "../lib/format";
  import { markdown } from "../lib/markdown";
  import { poll } from "../lib/poll.svelte";
  import { navigate } from "../lib/router.svelte";
  import type { Agent, GitSkill, Skill, SkillDetail } from "../lib/types";

  /** The skill under review, if any. */
  let { name = null }: { name?: string | null } = $props();

  let skills = $state<Skill[] | null>(null);
  let agents = $state<Agent[]>([]);
  let detail = $state<SkillDetail | null>(null);
  let now = $state(Date.now());

  poll(async () => {
    [skills, agents] = await Promise.all([api.get<Skill[]>("/skills"), api.get<Agent[]>("/agents")]);
    if (name) detail = await api.get<SkillDetail>(`/skills/${name}`);
    now = Date.now();
  }, 10000);

  let open = $state(false);
  let draft = $state<GitSkill>({ url: "", git_ref: null, path: null });
  let error = $state<string | null>(null);
  let busy = $state(false);

  const holders = (skill: string) => agents.filter((a) => a.skills.includes(skill)).map((a) => a.alias);
  const origin = (s: Skill) =>
    s.source.kind === "git"
      ? `${s.source.url.replace(/^https:\/\//, "")}${s.source.path ? `/${s.source.path}` : ""} @ ${s.source.commit.slice(0, 7)}`
      : `proposed by @${s.source.by}`;

  /** SKILL.md without its frontmatter: name and description are shown above. */
  const body = (text: string) => text.replace(/^---\r?\n[\s\S]*?\r?\n---\r?\n?/, "");

  async function refresh() {
    skills = await api.get<Skill[]>("/skills");
    if (name) detail = await api.get<SkillDetail>(`/skills/${name}`);
  }

  async function add(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    error = null;
    try {
      const skill = await api.post<Skill>("/skills", {
        url: draft.url.trim(),
        git_ref: draft.git_ref?.trim() || null,
        path: draft.path?.trim() || null,
      });
      open = false;
      draft = { url: "", git_ref: null, path: null };
      navigate(`/skills/${skill.name}`);
    } catch (e) {
      error = e instanceof ApiError ? (e.body.detail ?? e.body.kind) : "Could not fetch.";
    } finally {
      busy = false;
    }
  }

  async function act(skill: string, action: "enable" | "disable" | "update") {
    error = null;
    try {
      await api.post(`/skills/${skill}/${action}`);
      await refresh();
    } catch (e) {
      error = e instanceof ApiError ? (e.body.detail ?? e.body.kind) : "Could not do it.";
    }
  }

  async function remove(skill: string) {
    await api.delete(`/skills/${skill}`);
    navigate("/skills");
  }
</script>

<header class="screen-header">
  <h1>{#if name}<a class="muted" href="/skills">Skills /</a> {name}{:else}Skills{/if}</h1>
</header>

<div class="screen-body">
  {#if error}<p class="error" role="alert">{error}</p>{/if}

  {#if open && !name}
    <section class="panel">
      <div class="panel-head"><h2>Add a skill</h2><span class="mono small muted">it arrives disabled: review it, then enable</span></div>
      <div class="sources">
    <form class="source" onsubmit={add}>
      <h3>From a git repository</h3>
      <div class="grid">
        <label class="wide">
          Repository
          <input class="mono" bind:value={draft.url} required placeholder="https://github.com/anthropics/skills" autocomplete="off" />
        </label>
        <label>
          Ref
          <input class="mono" bind:value={draft.git_ref} placeholder="default branch" autocomplete="off" />
          <span class="hint">A branch, tag or commit. The commit is pinned; Update fetches the ref again.</span>
        </label>
        <label>
          Path
          <input class="mono" bind:value={draft.path} placeholder="skills/pdf" autocomplete="off" />
          <span class="hint">The skill's directory, with SKILL.md in it.</span>
        </label>
        <div class="wide actions">
          <button type="button" class="outline" onclick={() => (open = false)}>Cancel</button>
          <button class="primary" disabled={busy}>{busy ? "Fetching…" : "Fetch"}</button>
        </div>
      </div>
    </form>
        <div class="source">
          <h3>Written by an agent</h3>
          <p class="muted">An agent writes a skill in its workdir and calls <span class="mono">skill_propose</span>. It lands here disabled, marked with who proposed it.</p>
          <p class="muted">Public directories index GitHub without review, and a skill is instructions plus scripts: read SKILL.md and the scripts before enabling.</p>
        </div>
      </div>
    </section>
  {/if}

  {#if name && detail}
    {@const s = detail}
    <section class="panel">
      <div class="panel-head">
        <h2>{s.name}</h2>
        <span class="mono small" class:live={s.enabled} class:muted={!s.enabled}>{s.enabled ? "enabled" : "disabled — review, then enable"}</span>
      </div>
      <div class="panel-body review">
        <p>{s.description}</p>
        <dl>
          <dt>Source</dt>
          <dd class="mono">{origin(s)}</dd>
          <dt>Version</dt>
          <dd class="mono">{s.version} · {since(s.updated_at, now)} ago</dd>
          <dt>Granted to</dt>
          <dd>{holders(s.name).map((a) => `@${a}`).join(", ") || "nobody yet: tick it in an agent's card"}</dd>
          <dt>Files</dt>
          <dd class="mono small">{#each s.files as f (f.path)}<div>{f.path} <span class="muted">{bytes(f.size)}</span></div>{/each}</dd>
        </dl>
        <div class="actions">
          {#if s.source.kind === "git"}<button class="outline" onclick={() => act(s.name, "update")}>Update</button>{/if}
          <Confirm label="Delete" question="Delete {s.name} from the library?" confirm="Delete" danger onconfirm={() => remove(s.name)} />
          {#if s.enabled}
            <button class="outline" onclick={() => act(s.name, "disable")}>Disable</button>
          {:else}
            <button class="primary" onclick={() => act(s.name, "enable")}>Enable</button>
          {/if}
        </div>
      </div>
      <div class="panel-head sub"><h2>SKILL.md</h2><span class="mono small muted">what the model will read</span></div>
      <!-- Someone else's text: markdown() leaves raw HTML as text. -->
      <div class="panel-body md">{@html markdown(body(s.instructions))}</div>
    </section>
  {:else if skills}
    <section class="panel">
      <div class="panel-head">
        <h2>Library</h2>
        {#if !open}<button class="text-button" onclick={() => (open = true)}>+ Add skill</button>{/if}
      </div>
      <div class="table-wrap">
        <table>
          <thead><tr><th>Skill</th><th>When to use</th><th>Source</th><th>Agents</th><th>State</th></tr></thead>
          <tbody>
            {#each skills as s (s.name)}
              <tr class="link" onclick={() => navigate(`/skills/${s.name}`)}>
                <td class="mono"><a href="/skills/{s.name}">{s.name}</a></td>
                <td class="description" title={s.description}>{s.description}</td>
                <td class="mono muted small clip" title={origin(s)}>{origin(s)}</td>
                <td class="mono small">{holders(s.name).map((a) => `@${a}`).join(", ") || "—"}</td>
                <td class="mono small" class:live={s.enabled} class:muted={!s.enabled}>{s.enabled ? "enabled" : "disabled"}</td>
              </tr>
            {:else}
              <tr><td colspan="5" class="muted">No skills yet. Add one from git, or an agent proposes one with skill_propose.</td></tr>
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
  .sources {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
  }
  .source {
    padding: 14px 18px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .source + .source {
    border-left: 1px solid var(--line);
  }
  .source h3 {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
  }
  .source p {
    margin: 0;
  }
  .clip {
    max-width: 260px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .description {
    max-width: 420px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .review {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .review p {
    margin: 0;
  }
  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 6px 16px;
    margin: 0;
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .sub {
    border-top: 1px solid var(--line);
  }
  .md :global(:is(p, ul, ol, pre, h1, h2, h3)) {
    margin: 0 0 8px;
  }
  .md :global(:is(h1, h2, h3)) {
    font-size: 13px;
    font-weight: 600;
  }
  .md :global(pre) {
    padding: 8px 12px;
    overflow-x: auto;
    background: var(--bg);
    border: 1px solid var(--line);
  }
  .md :global(code) {
    font-family: var(--mono);
    font-size: 12px;
  }
  .panel + .panel {
    margin-top: 16px;
  }
</style>
