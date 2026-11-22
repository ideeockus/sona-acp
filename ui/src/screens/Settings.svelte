<script lang="ts">
  import { navigate } from "../lib/router.svelte";
  import { logout, session } from "../lib/session.svelte";
  import { applyTheme, themeChoice, type ThemeChoice } from "../lib/theme";

  let theme = $state<ThemeChoice>(themeChoice());
  const choices: { value: ThemeChoice; label: string }[] = [
    { value: "dark", label: "Dark" },
    { value: "light", label: "Light" },
    { value: "system", label: "System" },
  ];

  async function leave() {
    await logout();
    navigate("/login", true);
  }
</script>

<header class="screen-header"><h1>Settings</h1></header>

<div class="screen-body">
  <section class="panel">
    <div class="panel-head"><h2>Theme</h2></div>
    <div class="panel-body choices" role="radiogroup" aria-label="Theme">
      {#each choices as c (c.value)}
        <label class="choice">
          <input type="radio" name="theme" value={c.value} bind:group={theme} onchange={() => applyTheme(theme)} />
          {c.label}
        </label>
      {/each}
    </div>
  </section>

  <section class="panel">
    <div class="panel-head"><h2>Session</h2></div>
    <div class="panel-body row">
      <span>Logged in as <span class="mono">@{session.alias}</span>. A session lasts seven days from login.</span>
      <button class="outline" onclick={leave}>Log out</button>
    </div>
  </section>
</div>

<style>
  .choices {
    display: flex;
    gap: 24px;
  }
  .choice {
    flex-direction: row;
    align-items: center;
    gap: 8px;
    color: var(--text);
    font-size: 13px;
  }
  .choice input {
    width: auto;
    accent-color: var(--accent);
  }
  .row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 16px;
    flex-wrap: wrap;
  }
</style>
