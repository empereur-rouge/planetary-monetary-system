<script lang="ts">
    import { onMount, onDestroy } from "svelte";
    import { apiCall, ledgerApiCall } from "./api";
    import { nodeStatus, networkPeers, adminToken, ledgerList, selectedLedgerId, tokenList, type LedgerSummary, type TokenInfo } from "./stores";
    import PerformanceChart from "./PerformanceChart.svelte";
    import ServiceStatusBar from "./ServiceStatusBar.svelte";
    import { fade, fly } from "svelte/transition";

    let interval: any;

    function safeFormat(value: any, fractionDigits: number = 8): string {
        if (value === null || value === undefined || value === "") return "-";
        const num = parseFloat(value);
        if (isNaN(num)) return "-";
        return num.toLocaleString(undefined, { maximumFractionDigits: fractionDigits });
    }

    // TPS Chart Data
    let tpsHistory: number[] = new Array(30).fill(0);
    let chartLabels: string[] = new Array(30).fill("");
    let lastBlockCount = 0;
    let lastTime = Date.now();

    // Data States
    let supplyInfo: any = null;
    let nodeInfo: any = null;
    let tokens: TokenInfo[] = [];

    // Ledger management
    let ledgers: LedgerSummary[] = [];
    let showCreateForm = false;
    let createError = "";
    let createLoading = false;
    let newId = "";
    let newNetworkId = "";
    let newPrefix = "";

    async function fetchLedgers() {
        try {
            const res = await apiCall("/admin/ledgers");
            const list: LedgerSummary[] = res.ledgers || [];
            ledgerList.set(list);
            ledgers = list;
            // Reset if selected ledger no longer exists
            const currentId = $selectedLedgerId;
            if (currentId && !list.find((l) => l.id === currentId)) {
                selectedLedgerId.set(null);
            }
        } catch (e) {
            console.error("Failed to fetch ledgers", e);
            ledgerList.set([]);
            ledgers = [];
        }
    }

    async function fetchData() {
        try {
            const [metricsText, registryRes, p2pRes, supplyRes, nodeRes, tokensRes] =
                await Promise.all([
                    ledgerApiCall("/metrics").catch((err) => {
                        console.error("Metrics failed", err);
                        return "";
                    }),
                    apiCall("/v1/nodes").catch((err) => {
                        console.error("Nodes failed", err);
                        return { nodes: [] };
                    }),
                    apiCall("/v1/peers").catch((err) => {
                        console.error("Peers failed", err);
                        return [];
                    }),
                    ledgerApiCall("/v1/supply").catch((err) => {
                        console.error("Supply failed", err);
                        return null;
                    }),
                    apiCall("/admin/ping").catch((err) => {
                        console.error("Ping failed", err);
                        return null;
                    }),
                    ledgerApiCall("/v1/tokens").catch((err) => {
                        console.error("Tokens failed", err);
                        return { tokens: [] };
                    }),
                ]);

            if (metricsText) parseMetrics(metricsText);

            if (supplyRes) supplyInfo = supplyRes;
            if (nodeRes) nodeInfo = nodeRes;

            const tokenData: TokenInfo[] = tokensRes?.tokens || [];
            tokenList.set(tokenData);
            tokens = tokenData;

            const registryNodes = (registryRes.nodes || []).map((n: any) => ({
                ...n,
                id: n.node_pk,
                role: "Node",
            }));

            const p2pPeers = (p2pRes || []).map((addr: string, i: number) => ({
                id: `peer-${i}`,
                address: addr,
                role: "P2P",
            }));

            const allPeers = [...registryNodes, ...p2pPeers];
            networkPeers.set(allPeers);
        } catch (e) {
            console.error("Failed to fetch dashboard data", e);
        }
    }

    function parseMetrics(text: string) {
        const lines = text.split("\n");
        const data: any = {};
        lines.forEach((line) => {
            if (line.startsWith("#") || line.trim() === "") return;
            const parts = line.split(" ");
            if (parts.length >= 2) {
                data[parts[0]] = parts[1];
            }
        });

        nodeStatus.set(data);

        const currentBlocks = parseInt(data["pms_blocks_persisted_total"] || "0");
        const now = Date.now();

        if (lastBlockCount > 0) {
            const deltaBlocks = currentBlocks - lastBlockCount;
            const deltaTime = (now - lastTime) / 1000;

            if (deltaTime > 0 && deltaTime < 10) {
                const tps = Math.max(0, deltaBlocks / deltaTime);
                tpsHistory = [...tpsHistory.slice(1), tps];
                const timeStr = new Date().toLocaleTimeString();
                chartLabels = [...chartLabels.slice(1), timeStr];
            }
        }

        lastBlockCount = currentBlocks;
        lastTime = now;
    }

    function resetDashboardData() {
        tpsHistory = new Array(30).fill(0);
        chartLabels = new Array(30).fill("");
        lastBlockCount = 0;
        lastTime = Date.now();
        supplyInfo = null;
        nodeInfo = null;
        tokens = [];
        networkPeers.set([]);
        nodeStatus.set(null);
        tokenList.set([]);
    }

    function onLedgerChange(event: Event) {
        const select = event.target as HTMLSelectElement;
        const value = select.value;
        selectedLedgerId.set(value === "" ? null : value);
        resetDashboardData();
        fetchData();
    }

    function selectLedger(id: string) {
        selectedLedgerId.set(id);
        resetDashboardData();
        fetchData();
    }

    async function createLedger() {
        createError = "";
        createLoading = true;
        try {
            await apiCall("/admin/ledgers/create", "POST", {
                id: newId,
                network_id: newNetworkId,
                prefix: newPrefix,
            });
            await fetchLedgers();
            selectedLedgerId.set(newId);
            newId = "";
            newNetworkId = "";
            newPrefix = "";
            showCreateForm = false;
            resetDashboardData();
            fetchData();
        } catch (e: any) {
            createError = e.message || "Creation failed";
        } finally {
            createLoading = false;
        }
    }

    onMount(async () => {
        await fetchLedgers();
        fetchData();
        interval = setInterval(fetchData, 2000);
    });

    onDestroy(() => {
        if (interval) clearInterval(interval);
    });

    function logout() {
        adminToken.set("");
    }
</script>

<div class="dashboard" in:fade>
    <header class="glass-panel">
        <div class="brand">
            <div class="logo">PMS Node</div>
            {#if nodeInfo}
                <span class="badge" title="Network ID">{nodeInfo.network}</span>
                <span class="badge outline" title="Node Role"
                    >{nodeInfo.role || "Node"}</span
                >
            {/if}
            <ServiceStatusBar />
        </div>

        {#if ledgers.length > 1}
            <div class="ledger-selector">
                <label class="ledger-label" for="ledger-select">Ledger</label>
                <select
                    id="ledger-select"
                    class="ledger-dropdown"
                    value={$selectedLedgerId || ""}
                    on:change={onLedgerChange}
                >
                    <option value="">Default</option>
                    {#each ledgers as ledger}
                        <option value={ledger.id}
                            >{ledger.id} ({ledger.network_id})</option
                        >
                    {/each}
                </select>
            </div>
        {/if}

        <div class="actions">
            <button class="secondary" on:click={logout}>Logout</button>
        </div>
    </header>

    <div class="grid">
        <!-- Ledger Management Section -->
        <div
            class="glass-panel card ledger-management"
            in:fly={{ y: 20, duration: 500, delay: 0 }}
        >
            <div class="ledger-header-row">
                <h3>Ledgers</h3>
                <button
                    class="secondary small"
                    on:click={() => (showCreateForm = !showCreateForm)}
                >
                    {showCreateForm ? "Cancel" : "+ New Ledger"}
                </button>
            </div>

            <div class="ledger-grid">
                {#each ledgers as ledger}
                    <button
                        class="ledger-chip"
                        class:active={$selectedLedgerId === ledger.id ||
                            (!$selectedLedgerId && ledgers.length === 1)}
                        on:click={() => selectLedger(ledger.id)}
                    >
                        <span class="chip-name">{ledger.id}</span>
                        <span class="chip-meta"
                            >{ledger.block_count} blocks</span
                        >
                    </button>
                {/each}
            </div>

            {#if showCreateForm}
                <div
                    class="create-form"
                    transition:fly={{ y: -10, duration: 200 }}
                >
                    <div class="form-row">
                        <input
                            bind:value={newId}
                            placeholder="Ledger ID (ex: nft)"
                        />
                        <input
                            bind:value={newNetworkId}
                            placeholder="Network ID (ex: pms-nft)"
                        />
                        <input
                            bind:value={newPrefix}
                            placeholder="DB Prefix (ex: nft)"
                        />
                    </div>
                    {#if createError}
                        <p class="form-error">{createError}</p>
                    {/if}
                    <button
                        class="primary"
                        on:click={createLedger}
                        disabled={createLoading ||
                            !newId ||
                            !newNetworkId ||
                            !newPrefix}
                    >
                        {createLoading ? "Creating..." : "Create Ledger"}
                    </button>
                </div>
            {/if}
        </div>

        <!-- Status Cards -->
        <div
            class="glass-panel card"
            in:fly={{ y: 20, duration: 500, delay: 50 }}
        >
            <h3>DAG Size</h3>
            <div class="value">{$nodeStatus?.pms_blocks_total || "0"}</div>
            <div class="label">Total Blocks</div>
        </div>

        <div
            class="glass-panel card"
            in:fly={{ y: 20, duration: 500, delay: 100 }}
        >
            <h3>Blocks Persisted</h3>
            <div class="value">
                {$nodeStatus?.pms_blocks_persisted_total || "0"}
            </div>
            <div class="label">Confirmed</div>
        </div>

        <div
            class="glass-panel card"
            in:fly={{ y: 20, duration: 500, delay: 150 }}
        >
            <h3>Peers</h3>
            <div class="value">{$networkPeers.length}</div>
            <div class="label">Connected Nodes</div>
        </div>

        <div
            class="glass-panel card"
            in:fly={{ y: 20, duration: 500, delay: 200 }}
        >
            <h3>Circulating Supply</h3>
            <div class="value">
                {supplyInfo
                    ? safeFormat(supplyInfo.circulating_supply, 8)
                    : "-"}
            </div>
            <div class="label">{supplyInfo?.symbol || "PMS"}</div>
        </div>

        <div
            class="glass-panel card wallet-card"
            in:fly={{ y: 20, duration: 500, delay: 250 }}
        >
            <h3>Wallet Balances</h3>
            <div class="wallet-rows">
                <div class="wallet-row">
                    <span class="wallet-label">Node Identity</span>
                    <span class="wallet-value">
                        {supplyInfo ? safeFormat(supplyInfo.node_balance, 4) : "-"} {supplyInfo?.symbol || "PMS"}
                    </span>
                </div>
                <div class="wallet-row">
                    <span class="wallet-label">Coordinator</span>
                    <span class="wallet-value">
                        {supplyInfo ? safeFormat(supplyInfo.admin_balance, 4) : "-"} {supplyInfo?.symbol || "PMS"}
                    </span>
                </div>
                <div class="wallet-row">
                    <span class="wallet-label">Treasury</span>
                    <span class="wallet-value">
                        {supplyInfo ? safeFormat(supplyInfo.treasury_balance, 4) : "-"} {supplyInfo?.symbol || "PMS"}
                    </span>
                </div>
            </div>
        </div>

        <!-- Token Registry -->
        {#if tokens.length > 0}
            <div
                class="glass-panel card token-card"
                in:fly={{ y: 20, duration: 500, delay: 275 }}
            >
                <h3>Tokens</h3>
                <div class="table-container">
                    <table>
                        <thead>
                            <tr>
                                <th>Symbol</th>
                                <th>Name</th>
                                <th>Asset ID</th>
                                <th style="text-align: right;">Decimals</th>
                                <th style="text-align: right;">Max Supply</th>
                            </tr>
                        </thead>
                        <tbody>
                            {#each tokens as token}
                                <tr>
                                    <td class="mono token-symbol">{token.symbol}</td>
                                    <td>{token.name}</td>
                                    <td class="mono">{token.asset_id}</td>
                                    <td class="mono number">{token.decimals}</td>
                                    <td class="mono number">{token.max_supply ? safeFormat(token.max_supply) : "Unlimited"}</td>
                                </tr>
                            {/each}
                        </tbody>
                    </table>
                </div>
            </div>
        {/if}

        <!-- Real-time Chart -->
        <div
            class="glass-panel card wide-chart"
            in:fly={{ y: 20, duration: 500, delay: 300 }}
        >
            <h3>Throughput (Transactions Per Second)</h3>
            <div class="chart-wrapper">
                <PerformanceChart
                    dataPoints={tpsHistory}
                    labels={chartLabels}
                    label="TPS"
                    color="#10b981"
                />
            </div>
        </div>

        <div class="split-tables">
            <!-- Treasury Details -->
            <div
                class="glass-panel"
                in:fly={{ y: 20, duration: 500, delay: 400 }}
            >
                <h3>Treasury Wallets</h3>
                <div class="table-container">
                    <table>
                        <thead>
                            <tr>
                                <th>Address</th>
                                <th style="text-align: right;">Balance</th>
                            </tr>
                        </thead>
                        <tbody>
                            {#if supplyInfo && supplyInfo.treasury_details}
                                {#each supplyInfo.treasury_details as wallet}
                                    <tr>
                                        <td class="mono">{wallet.address}</td>
                                        <td class="mono number">
                                            {safeFormat(wallet.balance, 4)} {supplyInfo?.symbol || "PMS"}
                                        </td>
                                    </tr>
                                {/each}
                            {:else}
                                <tr>
                                    <td colspan="2" class="empty"
                                        >No treasury data</td
                                    >
                                </tr>
                            {/if}
                        </tbody>
                    </table>
                </div>
            </div>
        </div>
    </div>
</div>

<style>
    .dashboard {
        max-width: 1400px;
        margin: 0 auto;
        padding: 2rem;
    }

    header {
        display: flex;
        justify-content: space-between;
        align-items: center;
        margin-bottom: 2rem;
        padding: 1rem 2rem;
    }

    .brand {
        display: flex;
        align-items: center;
        gap: 1rem;
    }

    .logo {
        font-weight: 700;
        font-size: 1.5rem;
        background: var(--color-accent-gradient);
        -webkit-background-clip: text;
        -webkit-text-fill-color: transparent;
    }

    .badge {
        font-size: 0.75rem;
        padding: 0.25em 0.75em;
        background: rgba(255, 255, 255, 0.1);
        border-radius: 99px;
        color: var(--color-fg-secondary);
        font-weight: 600;
        text-transform: uppercase;
        letter-spacing: 0.05em;
    }

    .badge.outline {
        background: transparent;
        border: 1px solid var(--color-border);
    }

    /* Ledger selector in header */
    .ledger-selector {
        display: flex;
        align-items: center;
        gap: 0.5rem;
    }

    .ledger-label {
        font-size: 0.75rem;
        color: var(--color-fg-secondary);
        text-transform: uppercase;
        letter-spacing: 0.05em;
        font-weight: 600;
    }

    .ledger-dropdown {
        background: rgba(255, 255, 255, 0.05);
        border: 1px solid var(--color-border);
        color: var(--color-fg-primary);
        padding: 0.4em 2em 0.4em 0.8em;
        border-radius: 8px;
        font-size: 0.9rem;
        font-family: inherit;
        outline: none;
        cursor: pointer;
        transition: border-color 0.2s;
        -webkit-appearance: none;
        appearance: none;
        background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='12' height='12' viewBox='0 0 12 12'%3E%3Cpath fill='%23a1a1aa' d='M2 4l4 4 4-4'/%3E%3C/svg%3E");
        background-repeat: no-repeat;
        background-position: right 0.5em center;
    }

    .ledger-dropdown:focus {
        border-color: var(--color-accent);
    }

    .ledger-dropdown:hover {
        background-color: rgba(255, 255, 255, 0.08);
        border-color: rgba(255, 255, 255, 0.2);
    }

    .ledger-dropdown option {
        background: #18181b;
        color: var(--color-fg-primary);
    }

    .grid {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
        gap: 1.5rem;
    }

    .card {
        display: flex;
        flex-direction: column;
        align-items: flex-start;
        justify-content: space-between;
        min-height: 140px;
    }

    .card h3 {
        font-size: 0.85rem;
        color: var(--color-fg-secondary);
        text-transform: uppercase;
        letter-spacing: 0.05em;
        margin-bottom: 0.5rem;
    }

    .card .value {
        font-size: 2.2rem;
        font-weight: 700;
        color: var(--color-fg-primary);
        line-height: 1.1;
        margin-top: auto;
    }

    .card .label {
        font-size: 0.85rem;
        color: var(--color-fg-secondary);
        margin-top: 0.5rem;
        opacity: 0.7;
    }

    .wallet-card {
        min-height: 180px;
    }

    .token-card {
        grid-column: 1 / -1;
        min-height: auto;
    }

    .token-symbol {
        font-weight: 700;
        color: #10b981;
    }

    .wallet-rows {
        width: 100%;
        margin-top: auto;
    }

    .wallet-row {
        display: flex;
        justify-content: space-between;
        align-items: center;
        padding: 0.4rem 0;
        border-bottom: 1px solid rgba(255, 255, 255, 0.05);
    }

    .wallet-row:last-child {
        border-bottom: none;
    }

    .wallet-label {
        font-size: 0.85rem;
        color: var(--color-fg-secondary);
    }

    .wallet-value {
        font-size: 1.1rem;
        font-weight: 600;
        color: var(--color-fg-primary);
        font-family: monospace;
    }

    /* Ledger management section */
    .ledger-management {
        grid-column: 1 / -1;
        min-height: auto;
    }

    .ledger-header-row {
        display: flex;
        justify-content: space-between;
        align-items: center;
        margin-bottom: 1rem;
    }

    .ledger-grid {
        display: flex;
        flex-wrap: wrap;
        gap: 0.5rem;
    }

    .ledger-chip {
        display: flex;
        flex-direction: column;
        align-items: flex-start;
        padding: 0.6rem 1rem;
        border-radius: 10px;
        background: rgba(255, 255, 255, 0.05);
        border: 1px solid var(--color-border);
        cursor: pointer;
        transition: all 0.2s;
        color: inherit;
    }

    .ledger-chip:hover {
        background: rgba(255, 255, 255, 0.08);
        border-color: rgba(255, 255, 255, 0.2);
    }

    .ledger-chip.active {
        border-color: var(--color-accent);
        background: rgba(109, 40, 217, 0.15);
    }

    .chip-name {
        font-weight: 600;
        color: var(--color-fg-primary);
        font-size: 0.95rem;
    }

    .chip-meta {
        font-size: 0.75rem;
        color: var(--color-fg-secondary);
        margin-top: 0.2rem;
    }

    .create-form {
        margin-top: 1rem;
    }

    .form-row {
        display: flex;
        gap: 0.5rem;
        margin-bottom: 0.75rem;
    }

    .form-row input {
        flex: 1;
    }

    .form-error {
        color: var(--color-error);
        font-size: 0.85rem;
        margin: 0.5rem 0;
    }

    button.small {
        font-size: 0.8rem;
        padding: 0.4em 0.8em;
    }

    .wide-chart {
        grid-column: 1 / -1;
        min-height: 400px;
    }

    .chart-wrapper {
        width: 100%;
        flex: 1;
        min-height: 300px;
    }

    table {
        width: 100%;
        border-collapse: collapse;
    }

    th {
        text-align: left;
        padding: 1rem;
        color: var(--color-fg-secondary);
        border-bottom: 1px solid var(--color-border);
        font-size: 0.9rem;
        text-transform: uppercase;
        letter-spacing: 0.05em;
    }

    td {
        padding: 1rem;
        border-bottom: 1px solid rgba(255, 255, 255, 0.05);
        color: var(--color-fg-secondary);
    }

    .mono {
        font-family: monospace;
        color: var(--color-accent);
        font-size: 0.95rem;
    }

    .empty {
        text-align: center;
        color: var(--color-fg-secondary);
        padding: 2rem;
        font-style: italic;
    }

    button.secondary {
        background: transparent;
        border: 1px solid var(--color-border);
        font-size: 0.9rem;
    }

    button.secondary:hover {
        background: rgba(255, 255, 255, 0.05);
        border-color: rgba(255, 255, 255, 0.2);
    }

    .split-tables {
        grid-column: 1 / -1;
        display: grid;
        grid-template-columns: 1fr;
        gap: 1.5rem;
    }

    .table-container {
        overflow-x: auto;
        margin-top: 1rem;
        max-height: 400px;
        overflow-y: auto;
    }

    .table-container::-webkit-scrollbar {
        height: 8px;
        width: 8px;
    }

    .table-container::-webkit-scrollbar-track {
        background: rgba(255, 255, 255, 0.02);
        border-radius: 4px;
    }

    .table-container::-webkit-scrollbar-thumb {
        background: rgba(255, 255, 255, 0.1);
        border-radius: 4px;
    }

    .table-container::-webkit-scrollbar-thumb:hover {
        background: rgba(255, 255, 255, 0.2);
    }
</style>
