---
tags: [runbook, alerting, observability]
created: 2026-04-27
updated: 2026-04-27
version: v0.7.13
---

# Alerting — Connect testnet metrics to Telegram (or anything else)

**Goal**: get woken up at night when the engine breaks, and notified during business hours when it's degraded but not failing.

The whole stack is already deployed (Prometheus + Alertmanager + alert rules). Default receiver is **Telegram via Alertmanager's native `telegram_configs`** — no middleware (no n8n, no PagerDuty, no Slack), one bot, free, push-to-phone with sound.

You only need to provide two values:
1. A **bot token** from @BotFather → goes into `secrets/telegram_bot_token`
2. A **chat_id** for your alert channel → goes into `etc/alertmanager/alertmanager.yml`

Then `curl -X POST /-/reload` and you're paging.

This runbook covers:

1. [Setup Telegram bot + channel](#telegram-setup) (5 min)
2. [Wire it into Alertmanager](#wire) (2 min)
3. [Test end-to-end](#test) (1 min)
4. [Reading alerts + cost guardrails](#reading)
5. [Switching to another provider](#alternatives) (Better Uptime, PagerDuty, Slack, Discord)
6. [What if alerting itself breaks](#fallback)

---

## Telegram setup {#telegram-setup}

### 1. Create a bot

1. Open Telegram, search **@BotFather**.
2. Send `/newbot`.
3. Pick a name (e.g. "PMS Testnet Alerts") and a username (must end in `bot`, e.g. `pms_testnet_alerts_bot`).
4. BotFather replies with a token like `123456789:ABCdef-Ghij_KLmn...`. **Copy it now**, it's the only time it's shown.

### 2. Create the alert channel

1. In Telegram, **New Channel** → make it **private** → name it "PMS Alerts" (or two channels: "PMS Critical" + "PMS Warnings").
2. Open the channel → **Manage Channel** → **Administrators** → **Add Administrator** → search your bot's username → grant it "Post Messages" (uncheck everything else).

### 3. Get the chat_id

The channel ID is what you'll paste into `alertmanager.yml`. Easiest way:

```bash
# Replace <TOKEN> with the bot token from BotFather.
# Then send any message in your channel and run:
curl -s "https://api.telegram.org/bot<TOKEN>/getUpdates" | jq '.result[].channel_post.chat.id'
```

You'll see a negative integer like `-1001234567890`. That's your chat_id. (For direct messages to yourself, it's a positive integer instead — DM the bot first, then run getUpdates.)

### 4. Set the notification sound (Android, iOS similar)

For real "wake me up" behavior:

1. Open the channel in Telegram → tap the channel name at the top → **Notifications**.
2. Set a **custom notification sound** (alarm-style works best).
3. Toggle **Override silent / Do Not Disturb** to ON if your phone has it (Android: also enable in System Settings → Apps → Telegram → Notifications → channel → Important).

For warnings, set the sound to "default" or silent on a separate channel so they don't wake you up.

---

## Wire it into Alertmanager {#wire}

### 1. Drop the bot token into the secrets file

```bash
# On your local machine, in the dag-pms repo root:
echo -n 'PASTE_BOT_TOKEN_HERE' > secrets/telegram_bot_token
chmod 644 secrets/telegram_bot_token
```

The file is .gitignored — it never reaches the repo.

### 2. Set chat_id in `etc/alertmanager/alertmanager.yml`

Open [`etc/alertmanager/alertmanager.yml`](../../etc/alertmanager/alertmanager.yml). Find the two `chat_id: 0` lines (one in `pager`, one in `notify`) and replace `0` with your channel ID:

```yaml
- name: 'pager'
  telegram_configs:
    - bot_token_file: '/etc/alertmanager/telegram_bot_token'
      chat_id: -1001234567890   # <-- your channel
      disable_notifications: false  # plays sound (paging)
      ...

- name: 'notify'
  telegram_configs:
    - bot_token_file: '/etc/alertmanager/telegram_bot_token'
      chat_id: -1001234567890   # same channel, or a separate "warnings" channel
      disable_notifications: true   # silent (no sound)
      ...
```

If you want a single channel for everything (simpler), use the same chat_id on both. The `disable_notifications` flag still differentiates audible (critical) from silent (warning).

### 3. Switch the default receiver from `null` to `pager`

Still in `alertmanager.yml`, find the top-level `route:` block:

```yaml
route:
  receiver: 'null'   # <-- change to 'pager'
```

Change it to `'pager'` so any unmatched alert (rare, mostly safety net) also goes to your phone.

### 4. Push and reload

```bash
# From the repo root, push the changed files to the VPS:
scp -i ~/.ssh/pms_vps secrets/telegram_bot_token \
    pms@87.106.50.82:/opt/pms/secrets/telegram_bot_token
scp -i ~/.ssh/pms_vps etc/alertmanager/alertmanager.yml \
    pms@87.106.50.82:/opt/pms/etc/alertmanager/alertmanager.yml

# Restart alertmanager so it re-reads the bot_token_file and reloads the config.
# (Hot reload via /-/reload doesn't pick up new bind-mount file content
# in some Docker setups; a full restart is the bulletproof option.)
ssh -i ~/.ssh/pms_vps pms@87.106.50.82 \
    'docker restart pms-alertmanager-testnet'
```

If the YAML is malformed, Alertmanager fails to start and the previous config is preserved on disk — you can inspect with `docker logs pms-alertmanager-testnet`.

---

## Test {#test}

### Option A — Synthetic alert (cleanest, no real outage needed)

```bash
ssh -i ~/.ssh/pms_vps pms@87.106.50.82 'docker exec pms-alertmanager-testnet \
  amtool alert add \
    --alertmanager.url=http://localhost:9093 \
    alertname=TestPaging severity=critical component=test \
    --annotation=summary="Test page from runbook" \
    --annotation=description="If you see this on your phone, the pipeline works."'
```

Within ~30 s your phone should buzz with a Telegram notification. The alert auto-resolves after ~5 minutes (the default `resolve_timeout`); you'll get a second message marking it RESOLVED.

To clear it manually:

```bash
ssh pms@87.106.50.82 'docker exec pms-alertmanager-testnet \
  amtool silence add alertname=TestPaging --duration=10m \
    --alertmanager.url=http://localhost:9093'
```

### Option B — Real critical (validates Prometheus → Alertmanager too)

Trigger an actual rule by stopping the engine briefly:

```bash
ssh pms@87.106.50.82 'docker stop pms-engine-testnet'
# Wait ~2 min — the EngineDown rule has `for: 2m` to avoid pinging on flaps.
ssh pms@87.106.50.82 'docker start pms-engine-testnet'
# Within another minute you should get a RESOLVED message.
```

⚠️ This actually stops block production for ~3 minutes. Don't do it on a busy testnet without warning users.

---

## Reading alerts + cost guardrails {#reading}

Each rule in [`alerting_rules.yml`](../../etc/prometheus/alerting_rules.yml) has `summary`, `description`, and `runbook` annotations. The Telegram template renders all three.

The two warnings you'll see most often (and how to react):

- **`PersistQueueBackpressure`** → load-induced. Either traffic spiked or RocksDB compaction is lagging. Check `num_files_at_level0` in `/admin/rocksdb-stats`. If > 50, the engine is being driven faster than it absorbs — back off load or scale up.
- **`BloomSkipRatioLow`** → either the bloom is saturated (capacity reached, rotating frequently) or there's a real surge of duplicate blocks (network retransmits, replay attempts). Inspect `bloom_front_inserted` vs `bloom_capacity_per_segment`.

A **critical** alert means **block production has stopped or data is being lost**. Always investigate within minutes.

### Cost is zero on Telegram

Bot API has no rate-limit issues at our volume (a few alerts per day max). No subscriptions, no SMS quotas, nothing.

The only knobs that matter:

- **`group_interval` / `repeat_interval`** in `alertmanager.yml` — how often it re-sends a still-firing alert. Default `4h` for warning, `1h` for critical. Tighten only if you actually miss alerts.
- **Alert thresholds** in `alerting_rules.yml`. If you find yourself silencing the same warning every day, the threshold is too tight; bump the `>0.8` to `>0.9`, or `for: 5m` to `for: 15m`.
- **Inhibitions** in `alertmanager.yml`. The current config inhibits `persist-pipeline` and `storage` warnings while `EngineDown` is firing — you don't get 5 buzzes for one outage.

---

## Switching to another provider {#alternatives}

The `alertmanager.yml` shipped with this repo has commented-out blocks at the bottom for:

- **Better Uptime** — incoming webhook, free tier 10 SMS/calls per month
- **PagerDuty** — Events API v2, on-call rotations
- **Discord** — incoming webhook, no SMS
- **Slack** — incoming webhook, channel posting

Replace the `telegram_configs:` block in either receiver with the alternative. Each comes with the exact YAML to paste. Most you only need an integration URL or service key.

You can also fan out to multiple destinations by adding multiple receivers in the same `name:` block:

```yaml
- name: 'pager'
  telegram_configs:
    - bot_token_file: '/etc/alertmanager/telegram_bot_token'
      chat_id: -1001234567890
      ...
  webhook_configs:
    - url: 'https://uptime.betterstack.com/api/v1/incoming-webhooks/<TOKEN>'
      send_resolved: true
```

Both fire on every critical alert. Useful if you want Telegram for the buzz + Better Uptime for incident tracking.

---

## What if alerting itself breaks {#fallback}

If Prometheus crashes, Alertmanager crashes, or the Telegram API has an outage, you won't get paged for real problems. Three layers of defense:

1. **Self-scrape on Alertmanager** — Prometheus already scrapes `alertmanager:9093/metrics`. Add a rule to alert when `up{job="alertmanager"} == 0` for 1m. (Already implicit in the `PrometheusTargetMissing` rule.) But if Prometheus is *also* dead, this can't fire.

2. **External healthz monitoring** — Point a free Better Uptime monitor at `https://testnet.pms-network.com/healthz` with a 60-s interval. If the testnet is up, that returns 200; if it's down, you get paged within 1-2 min, *independently* of our internal stack. This catches the case where everything internal goes dark at the same time.

3. **Push-based heartbeat** — Add a second cron job on the engine VPS that pushes "I'm alive" every minute to a UptimeKuma / Healthchecks.io / cronitor.io endpoint. Miss a beat → external service pages you. Belt-and-suspenders. Optional.

The single point of failure pattern that bit us in v0.7.6 ("Prometheus disparu silencieusement") is exactly the kind of thing **option 2** above guards against.

---

## Quick reference

| Task | Command |
|---|---|
| Get bot updates (find chat_id) | `curl https://api.telegram.org/bot<TOKEN>/getUpdates` |
| Push config + restart | `scp etc/alertmanager/alertmanager.yml ... && ssh ... docker restart pms-alertmanager-testnet` |
| Hot reload (after first activation) | `ssh ... 'curl -X POST http://127.0.0.1:9093/-/reload'` |
| Test alert | `docker exec pms-alertmanager-testnet amtool alert add alertname=Test severity=critical ...` |
| Inspect alerts UI | `ssh -L 9093:127.0.0.1:9093 pms@87.106.50.82` then `http://localhost:9093` |
| Silence an alert | `docker exec pms-alertmanager-testnet amtool silence add alertname=X --duration=1h` |
| Tail Alertmanager logs | `ssh ... docker logs -f pms-alertmanager-testnet` |
