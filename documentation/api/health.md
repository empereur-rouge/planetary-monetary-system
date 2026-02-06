# 🏥 Health & Status API

> Endpoints pour la vérification de l'état du serveur

---

## GET `/livez`

Vérifie que le processus serveur est actif (liveness probe).

> 💡 **Usage Kubernetes**: Utilisé comme `livenessProbe` pour détecter si le pod doit être redémarré.

### Response

```
ok
```

### HTTP Status

| Code | Signification |
|------|---------------|
| 200 | Processus actif |

### Exemple

```bash
curl -k https://localhost:8443/livez
# ok
```

---

## GET `/healthz`

Vérifie que le serveur est prêt à recevoir des requêtes (readiness probe).

> 💡 **Usage Kubernetes**: Utilisé comme `readinessProbe` pour déterminer si le pod peut recevoir du trafic.

### Vérifications effectuées

- ✅ Base de données RocksDB ouverte
- ✅ Flag `ready` = true (après synchronisation DAG)

### Response (Prêt)

```
ready
```

### Response (En démarrage)

```
starting
```

### HTTP Status

| Code | Signification |
|------|---------------|
| 200 | Serveur prêt |
| 503 | En cours de démarrage |

### Exemple

```bash
curl -k -w "\n%{http_code}" https://localhost:8443/healthz
# ready
# 200
```

---

## GET `/live`

Alias de `/livez` pour compatibilité legacy.

---

## GET `/ready`

Alias de `/healthz` pour compatibilité legacy.

### Response

```
ready
```
ou
```
starting
```

---

## 📊 Monitoring

### Configuration Kubernetes

```yaml
apiVersion: v1
kind: Pod
spec:
  containers:
  - name: pms-server
    livenessProbe:
      httpGet:
        path: /livez
        port: 8443
      initialDelaySeconds: 5
      periodSeconds: 10
    readinessProbe:
      httpGet:
        path: /healthz
        port: 8443
      initialDelaySeconds: 10
      periodSeconds: 5
```

### Script de monitoring simple

```bash
#!/bin/bash
# health_check.sh

ENDPOINT="https://localhost:8443"

# Check liveness
if curl -skf "$ENDPOINT/livez" > /dev/null; then
    echo "✅ Server is alive"
else
    echo "❌ Server is down"
    exit 1
fi

# Check readiness
READY=$(curl -skf "$ENDPOINT/healthz")
if [ "$READY" = "ready" ]; then
    echo "✅ Server is ready"
else
    echo "⚠️ Server is starting: $READY"
    exit 2
fi
```
