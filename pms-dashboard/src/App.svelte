<script lang="ts">
  import { onMount } from "svelte";
  import { adminToken } from "./lib/stores";
  import Login from "./lib/Login.svelte";
  import Dashboard from "./lib/Dashboard.svelte";
</script>

<main>
  {#if !$adminToken}
    <Login />
  {:else}
    <Dashboard />
  {/if}
</main>

<style>
  main {
    max-width: 1280px;
    margin: 0 auto;
    padding: 2rem;
  }
</style>
