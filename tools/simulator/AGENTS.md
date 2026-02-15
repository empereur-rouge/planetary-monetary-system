# Guide des Agents

## Structure d'un agent

Chaque agent est defini dans un bloc `[[agents]]` (inline dans le config principal ou dans un fichier externe via `agent_files`) :

```toml
[[agents]]
count = 5                    # Nombre d'agents a spawner
interval_ms = 1000           # Intervalle entre chaque tick (ms)
name_prefix = "rnd"          # Prefixe du nom → rnd-0, rnd-1, rnd-2...
ai_interval = 5              # (smart uniquement) Appeler Gemini tous les N ticks

[agents.behavior]
type = "random"              # Type : "random", "smart", "observer"
# ... parametres specifiques au type

[agents.game]                # Optionnel : config game Edenite
enabled = true
# ... parametres game
```

### Parametres communs

| Parametre | Type | Default | Description |
|-----------|------|---------|-------------|
| `count` | entier | **requis** | Nombre d'agents crees avec cette config. Chaque agent a son propre wallet, solde et cubes. `count = 10` cree 10 agents independants. |
| `interval_ms` | entier | `2000` | Millisecondes entre chaque tick. Un tick = une iteration complete (game loop + envoi PMS). `100` = 10 actions/s, `1000` = 1 action/s, `5000` = 1 action toutes les 5s. |
| `name_prefix` | string | `"agent"` | Prefixe pour nommer les agents. Avec `"fast"` et `count = 3` → `fast-0`, `fast-1`, `fast-2`. L'index est global (incremente sur tous les groupes). |
| `ai_interval` | entier | `5` | Smart uniquement. Ticks entre chaque appel Gemini. Avec `ai_interval = 3` et `interval_ms = 1000` → Gemini appele toutes les 3s. |

---

## Random

L'agent Random est le type principal pour generer du trafic. Il fait deux choses en parallele :
- **Transactions PMS** : envoie des montants aleatoires a des peers aleatoires
- **Game loop Edenite** (si configure) : burn cubes → gagner EDN → envoyer EDN → re-mint

### Config

```toml
[[agents]]
count = 5
interval_ms = 1000
name_prefix = "rnd"

[agents.behavior]
type = "random"
min_amount = 0.1
max_amount = 5.0
send_probability = 0.5
```

### Parametres behavior

| Parametre | Type | Default | Ce qu'il fait |
|-----------|------|---------|---------------|
| `min_amount` | decimal | `0.1` | Montant minimum de PMS envoye par transaction. L'agent tire un montant aleatoire entre min et max a chaque tick. |
| `max_amount` | decimal | `5.0` | Montant maximum de PMS envoye. Pour des micro-tx : `0.01`/`0.1`. Pour du stress test : `10`/`100`. |
| `send_probability` | decimal | `0.5` | Probabilite (0.0 a 1.0) d'envoyer du PMS a chaque tick. `1.0` = envoie a chaque tick. `0.0` = n'envoie jamais de PMS (utile pour un agent game-only). `0.5` = 50% de chance. |

### Ce que fait l'agent a chaque tick

```
1. Refresh balance PMS (tous les 5 ticks)
2. Si solde PMS < 10 → auto-refuel 50 PMS via faucet
3. Game loop (si [agents.game] configure) :
   - A des cubes ? → batch burn tous les cubes → gagner EDN
   - A de l'EDN ?  → envoyer un % aleatoire a un peer
   - Rien ?        → re-mint des cubes
4. Tirage aleatoire : send_probability % de chance d'envoyer du PMS
   - Si oui : choisir un peer au hasard, envoyer entre min_amount et max_amount PMS
```

---

## Smart

L'agent Smart est pilote par l'IA Google Gemini. A intervalles reguliers, il envoie tout son contexte (solde, liste des peers, messages recus, historique d'actions) a Gemini et execute la directive recue.

### Config

```toml
[[agents]]
count = 1
interval_ms = 3000
name_prefix = "ai"
ai_interval = 5

[agents.behavior]
type = "smart"
system_prompt = "Tu es un trader prudent. N'envoie jamais plus de 2 PMS a la fois."
```

### Parametres behavior

| Parametre | Type | Default | Ce qu'il fait |
|-----------|------|---------|---------------|
| `system_prompt` | string | prompt generique | Instructions donnees a Gemini pour guider le comportement. C'est le levier principal de personnalisation. Exemples : "Tu es un trader agressif qui envoie de gros montants", "Tu es genereux et distribues a tout le monde equitablement", "Tu es un observateur qui n'agit que quand il recoit un message". |

### Directives Gemini

A chaque appel, Gemini repond avec une directive JSON :

| Directive | Ce qu'elle fait |
|-----------|-----------------|
| **Send** | Envoyer X PMS a un agent nomme. Gemini choisit le destinataire, le montant, et peut donner une raison. |
| **Wait** | Ne rien faire. Gemini decide d'attendre (ex: "j'attends que mon solde remonte"). |
| **Observe** | Interroger le reseau : nombre de tips, supply circulante, ou solde d'un agent. |
| **Message** | Envoyer un message texte a un autre agent. Les messages sont affiches dans le TUI et le dashboard web. |

### Ce que fait l'agent a chaque tick

```
1. Collecter les messages recus dans la boite de reception
2. Mettre a jour le cache de balance (tick 0 uniquement)
3. Tous les ai_interval ticks :
   - Construire le contexte (solde, peers, messages, 10 dernieres actions)
   - Appeler Gemini avec system_prompt + contexte
   - Parser la directive JSON
4. Executer la directive courante
5. En cas d'erreur : broadcast un message d'erreur avec auto-diagnostic
```

### Prerequis

L'agent smart necessite une section `[gemini]` dans le config principal :

```toml
# Option 1 : Cle API directe
[gemini]
api_key = "env:GEMINI_API_KEY"    # Lit depuis la variable d'environnement

# Option 2 : OAuth (ouvre le navigateur au premier lancement)
[gemini]
client_id = "..."
client_secret = "..."
```

---

## Observer

L'agent Observer est passif. Il ne fait aucune transaction. Son role est de monitorer le reseau et alimenter le pipeline de metriques (affiche dans le TUI et le dashboard web).

### Config

```toml
[[agents]]
count = 1
interval_ms = 5000
name_prefix = "obs"

[agents.behavior]
type = "observer"
```

### Pas de parametres behavior

L'Observer n'a aucun parametre configurable au-dela des parametres communs.

### Ce que fait l'agent a chaque tick

L'observer alterne entre 3 requetes (une par tick, en rotation) :

| Tick | Requete | Metrique emise |
|------|---------|----------------|
| 0 | Tips du DAG (max 10) | `TipsCount` |
| 1 | Supply circulante + nb UTXOs | `SupplyUpdate` |
| 2 | Solde de chaque agent enregistre | `BalanceUpdate` par agent |
| 3 | Tips du DAG | ... (cycle) |

Les metriques sont consommees par le TUI (sparklines TPS, latence, table agents) et le dashboard web (WebSocket).

---

## Configuration Game (Edenite)

La section `[agents.game]` active le game loop Edenite pour un groupe d'agents Random. Sans cette section, l'agent ne fait que des transactions PMS.

```toml
[agents.game]
enabled = true
cubes_per_agent = 5
cubes_per_remint = 3
edn_send_min_pct = 10.0
edn_send_max_pct = 50.0
```

### Parametres

| Parametre | Type | Default | Ce qu'il fait |
|-----------|------|---------|---------------|
| `enabled` | bool | `true` | Active/desactive le game loop. `false` = l'agent Random ne fait que des transactions PMS, il ignore completement les cubes et l'EDN. |
| `cubes_per_agent` | entier | `5` | Nombre de cubes NFT mintes pour chaque agent **au demarrage**. Les cubes sont la matiere premiere : on les burn pour gagner de l'EDN. Plus il y en a, plus le premier cycle de burn rapporte d'EDN. |
| `cubes_per_remint` | entier | `3` | Nombre de cubes a re-mint quand l'agent est a sec (ni cubes, ni EDN). C'est le "recharge" automatique. Augmenter pour des cycles de burn plus longs. |
| `edn_send_min_pct` | decimal | `10.0` | Pourcentage **minimum** du solde EDN envoye lors d'un transfert. Avec `10.0`, l'agent envoie au moins 10% de son EDN a un peer. |
| `edn_send_max_pct` | decimal | `50.0` | Pourcentage **maximum** du solde EDN envoye. L'envoi reel est un tirage aleatoire entre min_pct et max_pct. Avec `10/50`, l'agent envoie entre 10% et 50%. |

### Cycle du game loop (a chaque tick)

```
Demarrage : funder mint 5 cubes par agent (cubes_per_agent)

Tick 1 : a 5 cubes
  → batch burn les 5 cubes en UN SEUL appel API
  → calcul reward : somme de (weight * size * density) / diviseur pour chaque cube
  → mint le total EDN sur le ledger eden
  → solde EDN augmente

Tick 2 : 0 cubes, a de l'EDN
  → choisir un peer au hasard
  → envoyer entre 10% et 50% du solde EDN (aleatoire)
  → solde EDN diminue

Tick 3 : 0 cubes, a de l'EDN
  → meme chose, envoie EDN a un peer

...

Tick N : 0 cubes, EDN < seuil (0.00000001)
  → re-mint 3 cubes (cubes_per_remint)
  → delai 50ms entre chaque mint

Tick N+1 : a 3 cubes
  → batch burn les 3 cubes → gagner EDN
  → cycle recommence
```

### Formule de reward

Chaque cube a des attributs aleatoires generes au mint :

| Attribut | Range | Exemple |
|----------|-------|---------|
| `weight` | 100 - 1000 | 523.7 |
| `size` | 100 - 500 | 312.4 |
| `density` | 1 - 20 | 8.3 |

```
EDN par cube = (weight * size * density) / 19,300,000,000
```

Exemple : `(523.7 * 312.4 * 8.3) / 19,300,000,000 = 0.0000703 EDN`

Avec 5 cubes, un agent gagne typiquement entre 0.00001 et 0.001 EDN par cycle.

---

## Fichiers agents externes

Au lieu de definir les agents inline dans le config principal, on peut les mettre dans des fichiers separes :

```toml
# simulator.toml (avant la premiere section [])
agent_files = ["agents_random.toml", "agents_smart.toml"]
```

Chaque fichier contient des `[[agents]]` :

```toml
# agents_random.toml
[[agents]]
count = 10
interval_ms = 500
name_prefix = "fast"

[agents.behavior]
type = "random"
min_amount = 0.01
max_amount = 1.0
send_probability = 0.8

[agents.game]
enabled = true
cubes_per_agent = 10
cubes_per_remint = 5
```

Les chemins sont **relatifs au dossier du config principal**. Tous les agents (inline + fichiers) sont fusionnes.

> **Important** : `agent_files` doit etre place **avant** la premiere section `[...]` dans le TOML (c'est une cle root).

---

## Exemples

### Stress test (volume maximum)

```toml
[[agents]]
count = 50
interval_ms = 100
name_prefix = "stress"

[agents.behavior]
type = "random"
min_amount = 0.01
max_amount = 0.1
send_probability = 1.0
```

50 agents, 10 actions/s chacun, ~500 tx/s total.

### Game-only (focus Edenite, pas d'envoi PMS)

```toml
[[agents]]
count = 10
interval_ms = 500
name_prefix = "miner"

[agents.behavior]
type = "random"
min_amount = 0.01
max_amount = 0.01
send_probability = 0.0        # Pas d'envoi PMS

[agents.game]
enabled = true
cubes_per_agent = 10
cubes_per_remint = 5
edn_send_min_pct = 5.0
edn_send_max_pct = 20.0
```

Les agents ne font que le game loop : burn cubes → envoyer EDN → re-mint.

### Simulation realiste (profils varies)

```toml
# Utilisateurs normaux
[[agents]]
count = 10
interval_ms = 2000
name_prefix = "user"

[agents.behavior]
type = "random"
min_amount = 1.0
max_amount = 10.0
send_probability = 0.3

[agents.game]
enabled = true
cubes_per_agent = 5
cubes_per_remint = 3

# Whales (gros envois, peu frequents)
[[agents]]
count = 2
interval_ms = 10000
name_prefix = "whale"

[agents.behavior]
type = "random"
min_amount = 50.0
max_amount = 200.0
send_probability = 0.8

# Monitoring
[[agents]]
count = 1
interval_ms = 3000
name_prefix = "monitor"

[agents.behavior]
type = "observer"
```

### Multi-fichier

```toml
# simulator.toml
agent_files = ["agents_users.toml", "agents_whales.toml", "agents_bots.toml"]
```

Chaque fichier a ses propres `[[agents]]`. Permet de creer des centaines d'agents avec des comportements differents en combinant des profils.
