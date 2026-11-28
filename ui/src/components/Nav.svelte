<script lang="ts">
  import { route } from "../lib/router.svelte";
  import { session } from "../lib/session.svelte";

  let { unread }: { unread: number } = $props();

  // Storage and Integrations come later: no data, no item.
  const items = [
    { href: "/", label: "Overview", top: true },
    { href: "/triggers", label: "Triggers", top: true },
    { href: "/agents", label: "Agents", top: true },
    { href: "/agents", label: "Registry" },
    { href: "/skills", label: "Skills" },
    { href: "/messages", label: "Messages" },
    { href: "/journal", label: "Journal" },
    { href: "/tasks", label: "Tasks", top: true },
  ];

  function current(href: string, label: string): boolean {
    const path = route.path;
    if (label === "Agents") return false;
    if (href === "/") return path === "/";
    if (label === "Registry") return path.startsWith("/agents") || path.startsWith("/runs");
    return path.startsWith(href);
  }
</script>

<nav aria-label="Main">
  <a class="brand" href="/">
    <img src="/logo.png" alt="" width="18" height="18" />
    <span>sona</span>
  </a>
  <div class="items">
    {#each items as item (item.label)}
      <a
        href={item.href}
        class:top={item.top}
        class:sub={!item.top}
        aria-current={current(item.href, item.label) ? "page" : undefined}
      >
        <span>{item.label}</span>
        {#if item.label === "Messages" && unread > 0}<span class="mono small live">{unread}</span>{/if}
      </a>
    {/each}
  </div>
  <div class="bottom">
    <a class="top" href="/settings" aria-current={route.path === "/settings" ? "page" : undefined}>Settings</a>
    <span class="mono small muted who">@{session.alias}</span>
  </div>
</nav>

<style>
  nav {
    flex: 0 0 212px;
    padding: 20px 12px;
    display: flex;
    flex-direction: column;
    gap: 28px;
    border-right: 1px solid var(--line);
    position: sticky;
    top: 0;
    height: 100vh;
  }
  .brand {
    display: flex;
    justify-content: flex-start;
    align-items: center;
    gap: 10px;
    padding: 0 8px;
    font-size: 15px;
    font-weight: 600;
    letter-spacing: 0.01em;
  }
  .items,
  .bottom {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .bottom {
    margin-top: auto;
    padding-top: 12px;
    border-top: 1px solid var(--line);
  }
  a {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
  }
  .top {
    padding: 7px 8px;
  }
  .sub {
    padding: 6px 8px 6px 22px;
    color: var(--muted);
  }
  a[aria-current="page"] {
    color: var(--accent-text);
    background: var(--accent-soft);
    font-weight: 500;
  }
  .who {
    padding: 7px 8px;
  }
  @media (max-width: 720px) {
    nav {
      position: static;
      height: auto;
      flex: 1 1 100%;
      border-right: 0;
      border-bottom: 1px solid var(--line);
    }
  }
</style>
