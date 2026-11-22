<script lang="ts">
  import Nav from "./components/Nav.svelte";
  import { api } from "./lib/api";
  import { match, navigate, route } from "./lib/router.svelte";
  import { loadSession, session } from "./lib/session.svelte";
  import type { Conversation } from "./lib/types";
  import Login from "./screens/Login.svelte";
  import Overview from "./screens/Overview.svelte";

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
