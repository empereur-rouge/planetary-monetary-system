---
tags: [runbook, alerting, observability]
created: 2026-04-27
updated: 2026-04-27
version: v0.7.12
---

# Alerting — Connect testnet metrics to a paging channel

**Goal**: get woken up at night when the engine breaks, and notified during business hours when it's degraded but not failing.

The whole stack is already deployed on the testnet (Prometheus + Alertmanager + alert rules). The only thing missing is **your webhook URL**. This runbook walks through:

1. [How the alert pipeline works](#pipeline)
2. [Picking a provider](#providers) (Better Uptime / PagerDuty / Slack / Discord)
3. [Pasting your URL into `alertmanager.yml`](#paste-url)
4. [Reloading without restart](#reload)
5. [Sending a test alert](#test)

---

## Pipeline

```
PMS engine ──/metrics/all──> Prometheus ──fires alert──> Alertmanager ──webhook──> Better Uptime / PagerDuty / Slack
                              (rules eval                 (route by                  (your phone / chat)
                               every 15 s)                 severity)
```

- **Rules** live in [etc/prometheus/alerting_rules.yml](../../etc/prometheus/alerting_rules.yml). Two severity tiers: `critical` (page) and `warning` (notify).
- **Routing** lives in [etc/alertmanager/alertmanager.yml](../../etc/alertmanager/alertmanager.yml) — `severity: critical` → `pager` receiver; `severity: warning` → `notify` receiver.
- Until receivers have real URLs, alerts go to a `null` receiver and are silently dropped — so the deploy is safe to ship without configured paging.

The testnet exposes Alertmanager on `127.0.0.1:9093` of the VPS. SSH-tunnel to inspect:
```bash
ssh -L 9093:127.0.0.1:9093 pms@87.106.50.82
# Then open http://localhost:9093 in a browser.
```

---

## Providers

| Provider | Free tier | Phone calls | SMS | Setup time |
|---|---|---|---|---|
| **Better Uptime** | 10 monitors, 10 SMS/mo, 10 phone calls/mo, unlimited webhooks | yes (paid plan) | yes | ~3 min |
| **PagerDuty** | 5 users, unlimited alerts, 5 SMS/phone calls per month | yes | yes | ~5 min |
| **Slack** | unlimited webhooks | no | no | ~2 min |
| **Discord** | unlimited webhooks | no | no | ~1 min |

**Recommendation for solo operator launching a clicker game**: Better Uptime (best price/feature for paging) for `critical`, Discord webhook for `warning`. Total setup time: ~5 min, total cost: free until you exceed 10 SMS/mo.

---

## Paste your URL

Open [etc/alertmanager/alertmanager.yml](../../etc/alertmanager/alertmanager.yml) and find the two `webhook_configs:` blocks (one for `pager`, one for `notify`). Both ship with placeholder URLs:

```yaml
- name: 'pager'
  webhook_configs:
    - url: '<PASTE_YOUR_PAGER_WEBHOOK_URL_HERE>'
      send_resolved: true
```

### Better Uptime → `pager`
1. Log in at [betterstack.com](https://uptime.betterstack.com/).
2. **Integrations** → **+ Add integration** → **Incoming webhook**.
3. Name it "PMS Critical".
4. Copy the URL it gives you (looks like `https://uptime.betterstack.com/api/v1/incoming-webhooks/<token>`).
5. Paste it as `url:` under `pager` → `webhook_configs`.
6. **Configure your on-call schedule** in Better Uptime: **Escalation policies** → set who gets paged when the webhook fires (your phone via SMS+call).

### PagerDuty → `pager` (alternative)
1. Log in at [pagerduty.com](https://pagerduty.com).
2. **Services** → **+ New service** → **Generic Webhook (V2)** → copy the integration key.
3. In `alertmanager.yml`, **replace** the `webhook_configs:` block under `pager:` with:

```yaml
- name: 'pager'
  pagerduty_configs:
    - service_key: '<PASTE_YOUR_PAGERDUTY_INTEGRATION_KEY_HERE>'
      send_resolved: true
      description: '{{ .CommonAnnotations.summary }}'
      details:
        firing: '{{ .Alerts.Firing | len }}'
        runbook: '{{ .CommonAnnotations.runbook }}'
```

### Discord → `notify`
1. Server settings → **Integrations** → **Webhooks** → **New Webhook** → pick a channel → **Copy URL**.
2. Paste as `url:` under `notify` → `webhook_configs`.
3. Discord renders raw JSON adequately. For nice embeds, run a [discord-shim](https://github.com/dataops-tk/alertmanager-discord) container — overkill for a solo op.

### Slack → `notify` (alternative)
1. Slack admin → **Apps** → **Manage apps** → search "Incoming Webhooks" → **Add to Slack** → pick a channel → copy URL.
2. **Replace** the `webhook_configs:` block under `notify:` with:

```yaml
- name: 'notify'
  slack_configs:
    - api_url: '<PASTE_YOUR_SLACK_INCOMING_WEBHOOK_URL_HERE>'
      channel: '#pms-alerts'
      send_resolved: true
      title: '{{ .CommonAnnotations.summary }}'
      text: '{{ .CommonAnnotations.description }}'
```

---

## Reload

After editing `alertmanager.yml` locally, push it and tell Alertmanager to reload (no container restart):

```bash
# Push the new config to the VPS
scp -i ~/.ssh/pms_vps etc/alertmanager/alertmanager.yml \
    pms@87.106.50.82:/opt/pms/etc/alertmanager/alertmanager.yml

# Reload (Alertmanager listens for SIGHUP via this HTTP endpoint)
ssh -i ~/.ssh/pms_vps pms@87.106.50.82 \
    'curl -sf -X POST http://127.0.0.1:9093/-/reload && echo reloaded'
```

If the YAML is malformed, the reload returns 400 and the previous config keeps running — no risk of breaking paging.

---

## Test

Two ways to confirm the pipeline works end-to-end:

### Option A — Synthetic alert via amtool (cleanest)

```bash
ssh pms@87.106.50.82 'docker exec pms-alertmanager-testnet \
  amtool alert add \
    --alertmanager.url=http://localhost:9093 \
    alertname=TestPaging severity=critical component=test \
    --annotation=summary="Test page from runbook"'
```

Wait 30 s — Better Uptime should call/SMS you. Then resolve:

```bash
ssh pms@87.106.50.82 'docker exec pms-alertmanager-testnet \
  amtool silence add alertname=TestPaging --duration=1m \
    --alertmanager.url=http://localhost:9093'
```

### Option B — Trigger a real critical (kill prometheus)

```bash
ssh pms@87.106.50.82 'docker stop pms-prometheus-testnet'
```

Wait 2 min for `EngineDown` to fire (it doesn't actually fire because killing prometheus also kills the rule evaluator, but `up == 0` happens for downstream alerts that *target* prometheus). The cleaner test is Option A.

To recover: `docker start pms-prometheus-testnet`.

---

## What the alerts mean

Each rule in [alerting_rules.yml](../../etc/prometheus/alerting_rules.yml) has `summary`, `description`, and `runbook` annotations. Most of them link back to either `docker logs` or `/admin/rocksdb-stats`.

The two you'll see most often (and how to react):

- **`PersistQueueBackpressure`** (warning) → load-induced. Either traffic spiked or RocksDB compaction is lagging. Check `num_files_at_level0` in `/admin/rocksdb-stats`. If it's > 50, the engine is being driven faster than it can absorb — back off load or scale up.
- **`BloomSkipRatioLow`** (warning) → either bloom is saturated (capacity reached, rotating frequently) or there's a real surge of duplicate blocks (network retransmits, replay attempts). Inspect `bloom_front_inserted` vs `bloom_capacity_per_segment`.

A `critical` alert means **block production has stopped or data is being lost**. Always investigate within minutes.

---

## Cost guardrails

Alertmanager itself is free (open source, runs in 256 MiB). Costs come from your paging provider. Three knobs that bound your spend:

1. **`group_interval` / `repeat_interval`** in `alertmanager.yml` — how often it re-sends a still-firing alert. Default `4h` for warning, `1h` for critical. Tighten if your provider charges per notification.

2. **Alert thresholds** in `alerting_rules.yml`. If you find yourself silencing the same warning every day, the threshold is too tight; bump the `>0.8` to `>0.9`, or the `for: 5m` to `for: 15m`.

3. **Inhibitions** in `alertmanager.yml`. The current config inhibits `persist-pipeline` and `storage` warnings while `EngineDown` is firing — you don't need 5 SMS for one outage. Add more inhibitions as patterns emerge.

---

## When this all goes wrong

If alerting itself fails (Prometheus down, Alertmanager down, webhook provider outage), you have three layers:

1. **Better Uptime / PagerDuty heartbeat checks** — both providers offer a "if I don't hear from you every 60 s, page me" feature. Configure Prometheus to push a heartbeat every minute.
2. **External healthz monitoring** — point Better Uptime's HTTP monitor directly at `https://testnet.pms-network.com/healthz` with a 60 s interval. If the testnet is up, that returns 200; if it's down, you get paged within 1-2 min, *independently* of our internal Prometheus.
3. **Manual escape hatch** — if everything internal is broken, the `Memory watchdog` log lines on the simulator and the `docker logs pms-engine-testnet | grep -i error` will still tell you what's wrong if you can SSH in.

The single point of failure pattern that bit us in v0.7.6 ("Prometheus disparu silencieusement") is exactly the kind of thing **option 2** above guards against.
