<script lang="ts">
  import Nav from "./components/Nav.svelte";
  import { api } from "./lib/api";
  import { match, navigate, route } from "./lib/router.svelte";
  import { loadSession, session } from "./lib/session.svelte";
  import type { Conversation } from "./lib/types";
  import Login from "./screens/Login.svelte";
  import AgentForm from "./screens/AgentForm.svelte";
  import AgentPage from "./screens/AgentPage.svelte";
  import Journal from "./screens/Journal.svelte";
  import Messages from "./screens/Messages.svelte";
  import Overview from "./screens/Overview.svelte";
  import Registry from "./screens/Registry.svelte";
  import RunPage from "./screens/RunPage.svelte";
  import Settings from "./screens/Settings.svelte";
  import TaskPage from "./screens/TaskPage.svelte";
  import Skills from "./screens/Skills.svelte";
  import Triggers from "./screens/Triggers.svelte";
  import Tasks from "./screens/Tasks.svelte";

  let checked = $state(false);
  let unread = $state(0);

  $effect(() => {
    void loadSession().then((ok) => {
      checked = true;
      if (!ok && route.path !== "/login") navigate(`/login?next=${encodeURIComponent(route.path)}`, true);
    });
  });

  // The Messages count in the menu, on every screen.
  $effect(() => {
    if (!session.alias) return;
    const tick = () =>
      api
        .get<Conversation[]>("/conversations")
        .then((c) => (unread = c.reduce((sum, x) => sum + x.unread, 0)))
        .catch(() => {});
    void tick();
    const timer = setInterval(tick, 10_000);
    return () => clearInterval(timer);
  });
</script>

{#if route.path === "/login"}
  <Login />
{:else if checked && session.alias}
  <div class="shell">
    <Nav {unread} />
    <main>
      {#if match("/")}
        <Overview />
      {:else if match("/agents")}
        <Registry />
      {:else if match("/agents/new")}
        <AgentForm />
      {:else if match("/agents/:alias/edit")}
        {#key route.path}<AgentForm alias={match("/agents/:alias/edit")!.alias} />{/key}
      {:else if match("/agents/:alias")}
        {#key route.path}<AgentPage alias={match("/agents/:alias")!.alias!} />{/key}
      {:else if match("/messages")}
        <Messages />
      {:else if match("/messages/:alias")}
        {#key route.path}<Messages alias={match("/messages/:alias")!.alias} />{/key}
      {:else if match("/messages/:alias/:session")}
        {@const m = match("/messages/:alias/:session")!}
        {#key route.path}<Messages alias={m.alias} session={m.session} />{/key}
      {:else if match("/skills")}
        <Skills />
      {:else if match("/skills/:name")}
        {#key route.path}<Skills name={match("/skills/:name")!.name} />{/key}
      {:else if match("/triggers")}
        <Triggers />
      {:else if match("/journal")}
        <Journal />
      {:else if match("/tasks")}
        <Tasks />
      {:else if match("/tasks/:id")}
        {#key route.path}<TaskPage id={match("/tasks/:id")!.id!} />{/key}
      {:else if match("/settings")}
        <Settings />
      {:else if match("/runs/:id")}
        {#key route.path}<RunPage id={match("/runs/:id")!.id!} />{/key}
      {:else}
        <header class="screen-header"><h1>Not found</h1></header>
        <p class="empty">Nothing lives at {route.path}. <a class="live" href="/">Overview</a></p>
      {/if}
    </main>
  </div>
{/if}

<style>
  .shell {
    min-height: 100vh;
    display: flex;
    flex-wrap: wrap;
  }
  main {
    flex: 999 1 560px;
    min-width: 0;
  }
</style>
