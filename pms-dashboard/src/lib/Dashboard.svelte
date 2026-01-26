<script lang="ts">
    import { onMount, onDestroy } from "svelte";
    import { apiCall } from "./api";
    import { nodeStatus, networkPeers, adminToken } from "./stores";
    import PerformanceChart from "./PerformanceChart.svelte";

    let interval: any;

    // TPS Chart Data
    let tpsHistory: number[] = new Array(30).fill(0);
    let chartLabels: string[] = new Array(30).fill("");
    let lastBlockCount = 0;
    let lastTime = Date.now();

    async function fetchData() {
        try {
            const metricsText = await apiCall("/metrics");
            parseMetrics(metricsText);
            const peers = await apiCall("/v1/nodes");
            networkPeers.set(peers);
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

        // Update store
        nodeStatus.set(data);

        // Calculate TPS
        const currentBlocks = parseInt(data["pms_blocks_total"] || "0");
        // Using Data.now() for approximation.
        // Ideally backend provides pms_tps gauge, but we calculate locally for now.
        const now = Date.now();

        if (lastBlockCount > 0) {
            const deltaBlocks = currentBlocks - lastBlockCount;
            const deltaTime = (now - lastTime) / 1000; // seconds

            // Filter out crazy jumps if reload happened or long pause
            if (deltaTime > 0 && deltaTime < 10) {
                const tps = Math.max(0, deltaBlocks / deltaTime);

                // Shift history
                tpsHistory = [...tpsHistory.slice(1), tps];

                // Update Labels
                const timeStr = new Date().toLocaleTimeString();
                chartLabels = [...chartLabels.slice(1), timeStr];
            }
        }

        lastBlockCount = currentBlocks;
        lastTime = now;
    }

    onMount(() => {
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

<div class="dashboard">
    <header class="glass-panel">
        <div class="logo">PMS Node</div>
        <div class="actions">
            <button class="secondary" on:click={logout}>Logout</button>
        </div>
    </header>

    <div class="grid">
        <!-- Status Cards -->
        <div class="glass-panel card">
            <h3>DAG Size</h3>
            <div class="value">{$nodeStatus?.pms_blocks_total || "0"}</div>
            <div class="label">Total Blocks</div>
        </div>

        <div class="glass-panel card">
            <h3>Blocks Persisted</h3>
            <div class="value">
                {$nodeStatus?.pms_blocks_persisted_total || "0"}
            </div>
            <div class="label">Confirmed</div>
        </div>

        <div class="glass-panel card">
            <h3>Peers</h3>
            <div class="value">{$networkPeers.length}</div>
            <div class="label">Connected Nodes</div>
        </div>

        <!-- Real-time Chart -->
        <div class="glass-panel card wide-chart">
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

        <!-- Peer List -->
        <div class="glass-panel wide">
            <h3>Network Peers</h3>
            <div class="table-container">
                <table>
                    <thead>
                        <tr>
                            <th>ID</th>
                            <th>Address</th>
                            <th>Role</th>
                        </tr>
                    </thead>
                    <tbody>
                        {#each $networkPeers as peer}
                            <tr>
                                <td class="mono"
                                    >{peer.id.substring(0, 16)}...</td
                                >
                                <td class="mono">{peer.address}</td>
                                <td>{peer.role || "Node"}</td>
                            </tr>
                        {/each}
                        {#if $networkPeers.length === 0}
                            <tr
                                ><td colspan="3" class="empty"
                                    >No peers connected</td
                                ></tr
                            >
                        {/if}
                    </tbody>
                </table>
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

    .logo {
        font-weight: 700;
        font-size: 1.5rem;
        background: var(--color-accent-gradient);
        -webkit-background-clip: text;
        -webkit-text-fill-color: transparent;
    }

    .grid {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
        gap: 1.5rem;
    }

    .card {
        display: flex;
        flex-direction: column;
        align-items: flex-start;
    }

    .card h3 {
        font-size: 0.9rem;
        color: var(--color-fg-secondary);
        text-transform: uppercase;
        letter-spacing: 0.05em;
        margin-bottom: 0.5rem;
    }

    .card .value {
        font-size: 2.5rem;
        font-weight: 700;
        color: var(--color-fg-primary);
    }

    .wide {
        grid-column: 1 / -1;
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

    .table-container {
        overflow-x: auto;
        margin-top: 1rem;
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
    }

    td {
        padding: 1rem;
        border-bottom: 1px solid rgba(255, 255, 255, 0.05);
    }

    .mono {
        font-family: monospace;
        color: var(--color-accent);
    }

    .empty {
        text-align: center;
        color: var(--color-fg-secondary);
        padding: 2rem;
    }

    button.secondary {
        background: transparent;
        border: 1px solid var(--color-border);
    }

    button.secondary:hover {
        background: rgba(255, 255, 255, 0.05);
    }
</style>
