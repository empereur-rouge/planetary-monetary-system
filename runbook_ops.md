# PMS Node Operations Runbook

This guide details the standard operating procedures for managing a PMS Node in a Dockerized production environment.

## 🚀 Quick Start
**Start the node (Detached mode):**
```bash
./setup_docker_node.sh
# OR manually:
docker compose up -d
```

**Check Health:**
```bash
curl -k https://127.0.0.1:8080/livez
# Output: ok
```

## 🔍 Monitoring & Logs

**View Logs (Live):**
```bash
docker compose logs -f --tail 100 node
```

**Check Metrics:**
Requires `PMS_ADMIN_TOKEN_DEV` (Default: `pms_admin_secret`).
```bash
curl -k -H "Authorization: Bearer pms_admin_secret" https://127.0.0.1:8080/metrics
```
*Key Metrics:*
- `pms_blocks_total`: Total blocks in DAG.
- `pms_blocks_persisted_total`: Count of blocks written to disk.

**Check Caddy (Reverse Proxy):**
```bash
docker compose logs -f caddy
```

## 🛑 Stopping & Restarting

**Graceful Shutdown:**
Sends `SIGTERM` to the node, allowing it to flush RocksDB.
```bash
docker compose stop node
```

**Restart:**
```bash
docker compose restart node
```

**Force Kill (Emergency only):**
```bash
docker compose kill node
```

## 📦 Updates & Maintenance

**Update Node Software:**
1.  `git pull`
2.  Rebuild container:
    ```bash
    docker compose build node
    docker compose up -d node
    ```

**Clean Reset (Wipes Data!):**
```bash
docker compose down -v
```

## 💾 Backup & Restore

**Data Location:**
Data is stored in the `docker_data/` directory (mounted content).
-   `docker_data/pms-db`: RocksDB database.
-   `docker_data/wallet`: Node identity.

**Backup Procedure:**
1.  **Stop the node** (Critical to ensure DB consistency).
    ```bash
    docker compose stop node
    ```
2.  Archive the data directory.
    ```bash
    tar -czvf "backup_$(date +%Y%m%d).tar.gz" docker_data/
    ```
3.  Restart the node.
    ```bash
    docker compose start node
    ```

**Restore Procedure:**
1.  Stop the node.
2.  Extract the archive.
    ```bash
    tar -xzvf backup_ITEM.tar.gz
    ```
3.  Restart.

## 🔧 Troubleshooting

**"Unauthorized" on /metrics:**
-   Ensure you provided the Bearer token: `-H "Authorization: Bearer <token>"`.
-   Check `PMS_ADMIN_TOKEN_DEV` in `docker-compose.yml`.

**Node Stuck "Starting":**
-   Check logs: `docker compose logs node`.
-   Verify RocksDB lock (`LOCK` file in `docker_data/pms-db`). If the previous instance wasn't killed properly, delete the `LOCK` file *only if you are sure no process is running*.

**"429 Too Many Requests":**
-   The load is too high for the configured limits.
-   Adjust `PMS__LIMITS__RATE_LIMIT_RPS` in `docker-compose.yml` and restart.
