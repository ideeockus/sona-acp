<script lang="ts">
  import { api } from "../lib/api";
  import { clock } from "../lib/format";
  import { poll } from "../lib/poll.svelte";
  import { navigate, route } from "../lib/router.svelte";
  import { STATUSES, statusLabel } from "../lib/tasks";
  import type { Task, TaskStatus } from "../lib/types";

  let tasks = $state<Task[] | null>(null);
  const status = $derived(route.query.get("status") as TaskStatus | null);

  poll(async () => {
    tasks = await api.get<Task[]>(status ? `/tasks?status=${status}` : "/tasks");
  }, 6000);
</script>

<header class="screen-header">
  <h1>Tasks</h1>
  <nav class="tabs mono small" aria-label="Status">
    <a href="/tasks" aria-current={status === null ? "page" : undefined}>all</a>
    {#each STATUSES as s (s)}
      <a href="/tasks?status={s}" aria-current={status === s ? "page" : undefined} class:live={s === "needs_human"}>{statusLabel(s)}</a>
    {/each}
  </nav>
</header>

{#if tasks}
  <div class="screen-body">
    <section class="panel">
      <div class="table-wrap">
        <table>
          <thead><tr><th>Task</th><th>Status</th><th>Assignee</th><th>Created by</th><th>Created</th></tr></thead>
          <tbody>
            {#each tasks as task (task.id)}
              <tr class="link" onclick={() => navigate(`/tasks/${task.id}`)}>
                <td><a href="/tasks/{task.id}">{task.title}</a></td>
                <td class="mono" class:live={task.status === "needs_human"} class:muted={task.status === "done" || task.status === "cancelled"}>{statusLabel(task.status)}</td>
                <td class="mono">{task.assignee ? `@${task.assignee}` : "—"}</td>
                <td class="mono muted">@{task.creator}</td>
                <td class="mono muted">{clock(task.created_at)}</td>
              </tr>
            {:else}
              <tr><td colspan="5" class="muted">No tasks{status ? ` in ${statusLabel(status)}` : ""}.</td></tr>
            {/each}
          </tbody>
        </table>
      </div>
    </section>
  </div>
{/if}

<style>
  .tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .tabs a {
    padding: 4px 8px;
    border-radius: 4px;
  }
  .tabs a[aria-current="page"] {
    background: var(--accent-soft);
    color: var(--accent-text);
  }
</style>
