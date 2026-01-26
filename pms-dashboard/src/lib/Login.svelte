<script lang="ts">
    import { adminToken } from "./stores";
    import { apiCall } from "./api";

    let tokenInput = "";
    let error = "";
    let isLoading = false;

    async function handleLogin() {
        isLoading = true;
        error = "";

        // Set token temporarily to test connection
        adminToken.set(tokenInput);

        try {
            // Verify token with a cheap call (e.g. admin ping)
            await apiCall("/admin/ping");
            // If success, token is already set in store
        } catch (e) {
            console.error(e);
            error = "Invalid token or unable to connect to node.";
            adminToken.set(""); // Reset
        } finally {
            isLoading = false;
        }
    }
</script>

<div class="login-container">
    <div class="glass-panel login-box">
        <h1>PMS Admin</h1>
        <p class="subtitle">Secure Node Access</p>

        <div class="input-group">
            <input
                type="password"
                bind:value={tokenInput}
                placeholder="Enter Admin Token"
                on:keydown={(e) => e.key === "Enter" && handleLogin()}
            />
        </div>

        {#if error}
            <p class="error">{error}</p>
        {/if}

        <button class="primary" on:click={handleLogin} disabled={isLoading}>
            {#if isLoading}Connecting...{:else}Connect{/if}
        </button>
    </div>
</div>

<style>
    .login-container {
        height: 100vh;
        display: flex;
        align-items: center;
        justify-content: center;
    }

    .login-box {
        width: 100%;
        max-width: 400px;
        text-align: center;
        display: flex;
        flex-direction: column;
        gap: 1.5rem;
    }

    h1 {
        font-size: 2rem;
        background: var(--color-accent-gradient);
        -webkit-background-clip: text;
        -webkit-text-fill-color: transparent;
    }

    .subtitle {
        color: var(--color-fg-secondary);
        margin-top: -1rem;
    }

    .input-group input {
        width: 100%;
        box-sizing: border-box;
        padding: 1rem;
        font-size: 1.1rem;
    }

    button {
        width: 100%;
        padding: 1rem;
        font-size: 1.1rem;
    }

    .error {
        color: var(--color-error);
        font-size: 0.9rem;
    }
</style>
