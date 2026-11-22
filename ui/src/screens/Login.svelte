<!-- The credential is exchanged for a session cookie and forgotten (D103). -->
<script lang="ts">
  import { ApiError } from "../lib/api";
  import { navigate, route } from "../lib/router.svelte";
  import { login } from "../lib/session.svelte";

  let credential = $state("");
  let error = $state<string | null>(null);
  let busy = $state(false);

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    error = null;
    try {
      await login(credential.trim());
      credential = "";
      const next = route.query.get("next");
      navigate(next?.startsWith("/") && !next.startsWith("//") ? next : "/", true);
    } catch (e) {
      error = e instanceof ApiError && e.status === 401 ? "This credential is not valid." : "Could not log in.";
    } finally {
      busy = false;
    }
  }
</script>

<main>
  <form onsubmit={submit}>
    <img src="/logo.png" alt="" width="48" height="48" />
    <h1>Log in to sona</h1>
    <label>
      Operator credential
      <input
        type="password"
        name="credential"
        autocomplete="current-password"
        placeholder="sona_plane_op_…"
        bind:value={credential}
        required
      />
    </label>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <button class="primary" disabled={busy || !credential.trim()}>Log in</button>
    <p class="muted small"><code>control-plane init</code> printed it once.</p>
  </form>
</main>

<style>
  main {
    min-height: 100vh;
    display: grid;
    place-items: center;
    padding: 24px 16px;
  }
  form {
    width: min(360px, 100%);
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  h1 {
    font-size: 24px;
    font-weight: 300;
  }
  code {
    font-family: var(--mono);
  }
  .primary {
    padding: 10px 16px;
    box-shadow: 0 8px 24px rgba(247, 135, 27, 0.25);
  }
</style>
