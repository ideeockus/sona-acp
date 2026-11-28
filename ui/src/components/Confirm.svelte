<!-- A dangerous action asks once, in place: the button turns into the question. -->
<script lang="ts">
  let {
    label,
    question,
    confirm = "Yes",
    danger = false,
    onconfirm,
  }: { label: string; question: string; confirm?: string; danger?: boolean; onconfirm: () => Promise<void> | void } =
    $props();
  let asking = $state(false);
  let busy = $state(false);

  async function yes() {
    busy = true;
    try {
      await onconfirm();
    } finally {
      busy = false;
      asking = false;
    }
  }
</script>

{#if asking}
  <span class="ask" role="group" aria-label={question}>
    <span>{question}</span>
    <button class="text-button" class:danger disabled={busy} onclick={yes}>{confirm}</button>
    <button class="text-button quiet" onclick={() => (asking = false)}>Cancel</button>
  </span>
{:else}
  <button class="outline" class:danger-outline={danger} onclick={() => (asking = true)}>{label}</button>
{/if}

<style>
  .ask {
    display: inline-flex;
    gap: 12px;
    align-items: baseline;
    font-size: 12px;
  }
  .danger-outline {
    color: var(--danger);
  }
  .danger-outline:hover {
    border-color: var(--danger-line);
  }
</style>
