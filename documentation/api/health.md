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
curl http://localhost:3000/livez
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
curl -w "\n%{http_code}" http://localhost:3000/healthz
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
        port: 3000
      initialDelaySeconds: 5
      periodSeconds: 10
    readinessProbe:
      httpGet:
        path: /healthz
        port: 3000
      initialDelaySeconds: 10
      periodSeconds: 5
```

### Script de monitoring simple

```bash
#!/bin/bash
# health_check.sh

ENDPOINT="http://localhost:3000"

# Check liveness
if curl -sf "$ENDPOINT/livez" > /dev/null; then
    echo "✅ Server is alive"
else
    echo "❌ Server is down"
    exit 1
fi

# Check readiness
READY=$(curl -sf "$ENDPOINT/healthz")
if [ "$READY" = "ready" ]; then
    echo "✅ Server is ready"
else
    echo "⚠️ Server is starting: $READY"
    exit 2
fi
```
