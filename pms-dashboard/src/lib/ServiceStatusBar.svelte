<script lang="ts">
    import { onMount, onDestroy } from "svelte";

    interface ServiceStatus {
        name: string;
        status: string;
        detail?: string;
        latency_ms: number;
    }

    interface ServicesSnapshot {
        services: ServiceStatus[];
        checked_at: number;
    }

    type FetchState = "loading" | "ok" | "offline";

    let snapshot: ServicesSnapshot | null = null;
    let fetchState: FetchState = "loading";
    let interval: any;

    function statusColor(status: string): string {
        switch (status) {
            case "up":
                return "var(--color-success, #10b981)";
            case "degraded":
                return "var(--color-warning, #f59e0b)";
            default:
                return "var(--color-error, #ef4444)";
        }
    }

    async function fetchStatus() {
        try {
            const res = await fetch("/services/status");
            if (res.ok) {
                snapshot = await res.json();
                fetchState = "ok";
            } else {
                console.warn(`[ServiceStatus] HTTP ${res.status} from /services/status`);
                fetchState = "offline";
            }
        } catch (e) {
            console.warn("[ServiceStatus] Cannot reach /services/status:", e);
            fetchState = "offline";
        }
    }

    onMount(() => {
        fetchStatus();
        interval = setInterval(fetchStatus, 30_000);
    });

    onDestroy(() => {
        if (interval) clearInterval(interval);
    });
</script>

{#if fetchState === "loading"}
    <div class="status-bar">
        <div class="status-item" title="Checking services...">
            <span class="dot pulse" style="background-color: var(--color-fg-secondary, #71717a)"></span>
            <span class="name">checking...</span>
        </div>
    </div>
{:else if snapshot && snapshot.services.length > 0}
    <div class="status-bar">
        {#each snapshot.services as svc}
            <div
                class="status-item"
                title={svc.detail
                    ? `${svc.name}: ${svc.status} (${svc.latency_ms}ms) — ${svc.detail}`
                    : `${svc.name}: ${svc.status} (${svc.latency_ms}ms)`}
            >
                <span
                    class="dot"
                    style="background-color: {statusColor(svc.status)}"
                ></span>
                <span class="name">{svc.name}</span>
            </div>
        {/each}
    </div>
{:else}
    <div class="status-bar offline">
        <div class="status-item" title="Cannot reach service monitor">
            <span class="dot" style="background-color: var(--color-fg-secondary, #71717a)"></span>
            <span class="name">services offline</span>
        </div>
    </div>
{/if}

<style>
    .status-bar {
        display: flex;
        align-items: center;
        gap: 0.75rem;
        padding: 0.25rem 0.75rem;
        background: rgba(255, 255, 255, 0.04);
        border: 1px solid var(--color-border, rgba(255, 255, 255, 0.1));
        border-radius: 9999px;
        backdrop-filter: blur(12px);
        -webkit-backdrop-filter: blur(12px);
    }

    .status-item {
        display: flex;
        align-items: center;
        gap: 0.375rem;
        cursor: default;
    }

    .dot {
        width: 8px;
        height: 8px;
        border-radius: 50%;
        flex-shrink: 0;
    }

    .name {
        font-size: 0.6875rem;
        font-weight: 500;
        color: var(--color-fg-secondary, #a1a1aa);
        text-transform: uppercase;
        letter-spacing: 0.03em;
    }

    .status-bar.offline {
        opacity: 0.5;
    }

    .dot.pulse {
        animation: pulse 1.5s ease-in-out infinite;
    }

    @keyframes pulse {
        0%, 100% { opacity: 0.4; }
        50% { opacity: 1; }
    }

    @media (max-width: 768px) {
        .status-bar {
            gap: 0.5rem;
            padding: 0.25rem 0.5rem;
        }
        .name {
            display: none;
        }
    }
</style>
