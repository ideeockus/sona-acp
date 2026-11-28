<!-- An agent: a ring in its color (the logo's motif). The state is a dot and a word,
     never the color alone; a stuck agent's ring is broken (docs/ui.md). -->
<script lang="ts">
  type State = "running" | "idle" | "stuck" | "disabled";
  let { hue, state, size = 28 }: { hue: number | null; state: State; size?: number } = $props();
  const r = $derived(size / 2 - 3);
</script>

<svg aria-hidden="true" width={size} height={size} viewBox="0 0 {size} {size}" style="--hue: {hue ?? 28}">
  <circle
    cx={size / 2}
    cy={size / 2}
    {r}
    fill="var(--surface)"
    stroke={state === "disabled" ? "var(--idle)" : "hsl(var(--hue) var(--agent-s) var(--agent-l))"}
    stroke-width={state === "running" ? 2.5 : 1.5}
    stroke-dasharray={state === "stuck" ? "4 3" : state === "disabled" ? "1 3" : undefined}
  />
  {#if state === "running" || state === "stuck"}
    <circle
      class:pulse={state === "running"}
      cx={size / 2 + r * 0.7071}
      cy={size / 2 - r * 0.7071}
      r="3.5"
      fill={state === "running" ? "var(--accent)" : "var(--danger-line)"}
    />
  {/if}
</svg>

<style>
  svg {
    flex: none;
    overflow: visible;
  }
</style>
