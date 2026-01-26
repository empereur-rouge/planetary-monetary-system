<script lang="ts">
    import { onMount, onDestroy } from "svelte";
    import Chart from "chart.js/auto";

    export let dataPoints: number[] = [];
    export let labels: string[] = [];
    export let label: string = "Metric";
    export let color: string = "#7c3aed";

    let canvas: HTMLCanvasElement;
    let chart: Chart;

    $: if (chart && dataPoints) {
        chart.data.labels = labels;
        chart.data.datasets[0].data = dataPoints;
        chart.update("none"); // 'none' for smooth animation
    }

    onMount(() => {
        const ctx = canvas.getContext("2d");
        if (!ctx) return;

        // Create gradient
        const gradient = ctx.createLinearGradient(0, 0, 0, 400);
        gradient.addColorStop(
            0,
            color.replace(")", ", 0.5)").replace("rgb", "rgba"),
        );
        gradient.addColorStop(1, "rgba(0,0,0,0)");

        // Hex to simple rgba hack for gradient if color is hex
        // For now assuming passed color is fine or handled by Chart.js utils,
        // but let's just use a simple approach: force a specific color style.

        chart = new Chart(ctx, {
            type: "line",
            data: {
                labels: labels,
                datasets: [
                    {
                        label: label,
                        data: dataPoints,
                        borderColor: color,
                        backgroundColor:
                            color === "#7c3aed"
                                ? "rgba(124, 58, 237, 0.2)"
                                : "rgba(16, 185, 129, 0.2)",
                        borderWidth: 2,
                        fill: true,
                        tension: 0.4,
                        pointRadius: 0,
                    },
                ],
            },
            options: {
                responsive: true,
                maintainAspectRatio: false,
                plugins: {
                    legend: { display: false },
                },
                scales: {
                    x: { display: false },
                    y: {
                        grid: { color: "rgba(255,255,255,0.05)" },
                        ticks: { color: "#a1a1aa" },
                    },
                },
                animation: { duration: 0 },
            },
        });
    });

    onDestroy(() => {
        if (chart) chart.destroy();
    });
</script>

<div class="chart-container">
    <canvas bind:this={canvas}></canvas>
</div>

<style>
    .chart-container {
        position: relative;
        height: 100%;
        width: 100%;
        min-height: 200px;
    }
</style>
