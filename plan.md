# PMS — Protocole économique, émission, mint & gouvernance

> Document de référence consolidé. Couvre : l'architecture monétaire, la politique d'émission, les règles de mint, la gouvernance et la roadmap. À lire avec les specs techniques associées :
> - `pms-spec-dag-implementation.md` (features protocole DAG + correctifs audit C-1/C-2)
> - `pms-checklist-tests.md` (invariants de sécurité à tester)
> - `pms-spec-emission-budget.md` (spec d'implémentation du **budget d'émission partagé**, §3.1 — ancrée dans le code v0.11.3)
>
> **Statut des valeurs chiffrées :** ce qui est **décidé** est marqué ✅ ; ce qui reste **à calibrer** (souvent avec des données réelles) est marqué 🔧. Rien n'est inventé à la place de décisions non prises.

---

## 0. Principe directeur — surface vs moteur

Le projet vend l'esthétique **Arasaka** (scrip corporatif, dystopie, « la corp prend sa dîme ») mais fait tourner un **moteur digne de confiance** (émission gouvernée, transparente, vérifiable). Les deux ne se contredisent pas : l'utilisateur *vit* une dystopie, mais son PMS est *protégé* par des règles qu'il peut vérifier.

```
SURFACE (Arasaka)          MOTEUR (l'inverse d'Arasaka)
─────────────────          ────────────────────────────
Scrip corporatif      →    Monnaie à émission gouvernée
"La corp contrôle"    →    Politique publique + timelock
Farm = labeur          →    Distribution annoncée
Dîme partout           →    Rake honnête sur le volume
```

Modèle assumé : **non décentralisé, custodial, corporate.** Le DAG appartient à une entreprise. La gouvernance est donc corporate (pas une DAO), mais encadrée par des processus publics.

---

## 1. Architecture monétaire — deux jetons, deux ledgers

DAG multi-ledger, un seul consensus (Single Writer). Deux actifs aux rôles distincts :

| | **Scrip** (ledger farm) | **PMS** (ledger natif) |
|---|---|---|
| Rôle | Monnaie de labeur | Monnaie de réserve du réseau |
| Nature | Inflationniste, jetable | Rare, élastique, crédible |
| Obtention | Produit par effort (farm) | Voir règles de mint (§3) |
| Valeur | Ticket vers le PMS | Adossée à la demande réseau |

Le **scrip** absorbe l'inflation du farm sans contaminer le **PMS**. C'est le modèle à deux jetons (cf. Axie SLP/AXS, STEPN GST/GMT), plus sain qu'un actif unique.

> ⚠️ Distinction clé actée : **consensus ≠ mint.** Un seul consensus (Single Writer). Autant de règles de mint que voulu, toutes validées par le même Coordinator. Aucune « fusion de consensus » n'est nécessaire.

---

## 2. Politique d'émission du PMS

### 2.1 Pas de cap dur — supply élastique gouvernée

✅ **Décision : pas de plafond façon Bitcoin (21M).** Le PMS est fait pour circuler et être brûlé ; un cap dur l'asphyxierait. Modèle retenu : **émission élastique à la Monero** — supply illimitée *en total*, mais régie par une **règle**, jamais par décision libre.

> Le point capital : « illimité » ≠ « discrétionnaire ». La crédibilité ne vient pas d'un cap, elle vient d'une **règle d'émission inviolable que l'opérateur ne peut pas franchir par surprise.**

### 2.2 Une cible, trois signaux (jamais une addition)

L'émission n'est PAS la somme de trois règles (ce serait trois planches à billets cumulées). C'est **un objectif** piloté par **trois signaux** :

```
OBJECTIF MAÎTRE : la masse en circulation suit une trajectoire saine
   plafond : croissance ≤ taux cible    plancher : ne se contracte pas sous un seuil
                          ▲
        ┌─────────────────┼─────────────────┐
   Signal 1            Signal 2           Signal 3
   le burn            l'activité          le temps
 (compense ce      (indexe sur la       (taux cible
  qui est brûlé)    valeur réelle entrée) de fond)
```

Les signaux **informent** la position de l'émission *à l'intérieur* du couloir autorisé. Ils ne peuvent jamais faire dépasser le plafond.

**Déploiement séquencé** (conçu pour les trois, démarré avec un) :
1. **Lancement** : taux cible seul (simple, lisible, crédible).
2. **Maturité** : + compensation du burn (quand le volume de burn est significatif).
3. **Optimisation** : + indexation à l'activité (réglage fin, une fois calibré sur données réelles).

### 2.3 Paramètres d'émission

| Paramètre | Valeur | Statut |
|---|---|---|
| Taux cible (croisière) | **2 % / an** | ✅ (modulable, voir §4) |
| Plafond du couloir (max sans processus lourd) | **10 % / an** | ✅ |
| Plancher anti-asphyxie | à définir | 🔧 |
| Formule exacte (coefficients burn/activité) | à calibrer sur données | 🔧 |
| Budget d'émission par période (bloc/jour) | dérivé du taux + supply courante | 🔧 |

> 10 % est un **plafond d'urgence**, pas un niveau de vie : 10 % d'émission soutenue = dévaluation réelle. Il n'est atteignable que via le processus de gouvernance lourd (§4).

### 2.4 Le burn comme contre-pouvoir

Une partie du rake (§3) est **brûlée**, ce qui réduit la masse. Couplé à l'émission qui le compense, ça donne un système **auto-équilibrant** :

```
Forte activité → beaucoup de burn → la règle ré-émet → masse stable
Faible activité → peu de burn      → peu d'émission   → masse stable
```

À fort volume, le burn peut rendre le PMS légèrement **déflationniste** — favorable aux détenteurs.

---

## 3. Règles de mint du PMS

### 3.1 Le budget partagé — la règle non-négociable

✅ **Toutes les voies de mint puisent dans le MÊME budget d'émission** (défini en §2). Jamais de budgets parallèles.

> 📐 **Spec d'implémentation : `pms-spec-emission-budget.md`** — période dérivée des timestamps de bloc, compteur écrit dans le WriteBatch du bloc (crash-consistent + idempotent au replay), enforcement via mutex d'émission (ferme le TOCTOU), couloir dur par `clamp`, fees exclues du budget. Plan d'implémentation en 6 commits atomiques.

```
        Budget d'émission/période (issu du taux cible)
                          │
   ┌──────────┬───────────┼────────────┬──────────┐
 Achat      Farm      Contribution   Contenu
 direct   (scrip→PMS)    réseau       vendu
   └──────────┴── se partagent le budget ──┴────────┘
        Total émis ≤ budget  →  sinon rejet / file / dégradation du taux
```

Sans ce budget commun, N voies de mint = N planches à billets. Avec lui, on peut en ouvrir autant qu'on veut sans risque inflationniste.

### 3.2 Les quatre voies

| Voie | Qui elle capte | Preuve exigée par le protocole |
|---|---|---|
| **A. Achat direct** (fiat→PMS) | Ceux qui ont l'argent, veulent consommer | Preuve de paiement |
| **B. Farm** (scrip→PMS via FX) | Ceux qui ont le temps, pas l'argent | Preuve de burn de scrip + taux FX en vigueur |
| **C. Contribution réseau** | Ceux qui renforcent l'infra | Preuve de service (nœud, stockage, validation) |
| **D. Contenu vendu** | Les créateurs (fabriquent la demande) | Preuve de vente on-DAG |

✅ **Les quatre sont en cible**, mais **déployées en séquence** (conçues toutes, activées une à une) :

```
Phase 1 : Achat direct seul   → donne la valeur, amorce la liquidité, 1 surface à sécuriser
Phase 2 : + Farm/scrip        → fait entrer le volume une fois la valeur établie
Phase 3 : + Contribution      → quand il y a une infra à soutenir
Phase 4 : + Contenu vendu     → quand l'écosystème créateur existe
```

### 3.3 La conversion scrip→PMS (voie B) — **smart contract**, pas un bridge hardcodé

Le « scrip » est un **token custom** (ex : l'**edenite** sur le ledger eden) — pas
une chose codée en dur. La conversion scrip→PMS est le **goulot contrôlé** de
l'économie, et conformément à l'invariant du projet (*seul le PMS natif est
hardcodé ; tout le custom passe par contrat*), elle est implémentée comme un
**smart contract**, pas comme un mécanisme moteur dédié.

```
Burn de token custom (ex: edenite)            Mint PMS natif (sous budget)
──────────────────────────────────            ───────────────────────────
TokenBurn { asset_id, amount: X }  ──fire──>   contrat OnTokenBurn{asset_id}
  (la supply du token baisse)                   → action MintNative{ R }
                                                → mint  X × R  PMS au burner
                                                   (réservé sur le budget §3.1)
```

Le moteur ne connaît jamais « edenite » : il fournit la **primitive** (burn de
token générique + mint PMS natif sous budget via `EmissionGate`) ; le **contrat**
porte la **politique** (quel token, le taux R, le trigger).

| Question | Décision | Statut |
|---|---|---|
| Qui fixe le taux R ? | **Le contrat** (`MintNative{ rate_num/rate_den }`), piloté par l'opérateur via `/admin/contracts` | ✅ (taux fixe livré ; R dégressif = 🔧) |
| Le scrip est-il brûlé à la conversion ? | **Brûlé** (`PlainPayload::TokenBurn` — destruction réelle, la supply baisse) | ✅ |
| Émission PMS plafonnée ? | **Oui** — `EmissionGate::reserve` AVANT le burn ; budget épuisé ⇒ conversion refusée, aucun burn (sûreté des fonds) | ✅ |

> Piège acté à éviter : « le scrip se burn pour minter du PMS sans limite » = planche à billets via le scrip. La conversion **réserve le budget d'émission §3.1 avant de brûler** — atomique, jamais de dépassement.

### 3.4 Implémentation DAG (livrée, v0.14.0)

La voie B est un **smart contract** (`OnTokenBurn{asset_id}` → `MintNative{R}`),
pas une variante hardcodée. Flux : `POST /v1/wallet/token/burn` → `PlainPayload::TokenBurn`
(burn owner-signé) → `evaluate_token_burn` (le contrat donne R) → `EmissionGate::reserve`
(budget) → mint `PlainPayload::Mint` de PMS natif au burner. **Réserve-avant-burn**
pour l'atomicité (budget épuisé ⇒ rejet complet). Les autres voies natives (baseline,
on-ramp, faucet) restent des mints natifs directs sous le même budget. Voir
[pms-spec-emission-budget.md](pms-spec-emission-budget.md) + la fiche [[budget-emission]].

---

## 4. Gouvernance

### 4.1 Principe — gouvernée, pas figée

✅ **Tout est modifiable** (taux, bornes, règles) — cohérent avec un metaverse évolutif et un horizon long terme. Mais aucun changement n'est **instantané ni secret** : chaque changement passe par un **processus** (annonce + timelock + trace DAG).

> La protection des détenteurs n'est PAS l'immuabilité — c'est l'**impossibilité de changer par surprise**. La Fed change ses taux en permanence et le dollar reste crédible, parce que le *processus* est prévisible. Même logique ici.

### 4.2 Processus gradué par impact

| Niveau | Exemples | Timelock | Statut |
|---|---|---|---|
| **Opérateur** | Ajuster le taux dans le couloir, calibrer un fee | **7 j** | ✅ |
| **Politique** | Changer une borne, ouvrir une voie de mint | **15 j** | ✅ |
| **Lourd (constitution)** | Modifier le pouvoir de mint, le couloir 10 %, qui gouverne | **45 j** | ✅ |

Tout changement : **annoncé à l'avance**, **timelocké** (délai avant effet), **inscrit dans le DAG** (horodaté, public, vérifiable). Pendant le timelock, la communauté garde un **droit de sortie informée** (vendre/partir/contester).

### 4.3 Votes — consultatif d'abord

| Type | Effet | Risque | Reco |
|---|---|---|---|
| Consultatif | Avis, l'entreprise décide | Faible | ✅ **Démarrer ici** |
| Contraignant | Le résultat force la décision | Élevé (cession de pouvoir + risque « titre financier ») | Plus tard, si décidé |
| Pondéré par avoirs | Vote ∝ PMS détenu | Pouvoir aux gros porteurs + signal « titre » renforcé | Prudence |

✅ **Démarrage en consultatif** : légitimité communautaire sans se lier les mains ni s'exposer juridiquement. Durcissement possible ensuite (l'inverse — reprendre un pouvoir donné — est bien plus difficile).

---

## 5. La boucle économique complète

```
Farmeur ──effort/VR──> Scrip ──[burn, taux R]──> PMS (sous budget d'émission)
                                                   │
                                  Acheteur ──fiat──┘ (voie A, amorçage & valeur)
                                                   │
                          dépense en PMS ──> Vidéos / NFT / tokens / accès
                                                   │
                          (à chaque hop : rake opérateur + une part brûlée)
```

**Répartition des rôles (à ne jamais confondre) :**
- **PMS** = la monnaie (rare, élastique, crédible).
- **Le contenu** (vidéos, NFT, accès) = la *raison* d'avoir du PMS. **C'est le vrai produit.**
- **Scrip / farm** = canal de distribution, pas source de valeur.
- **Opérateur (toi)** = prend sa dîme et pilote l'offre, aux mains liées par la gouvernance.

> Acté : « dur à obtenir » n'est PAS une source de valeur. La **demande** crée la valeur. Le système ne tient que si le contenu est réellement désirable. **Question stratégique ouverte n°1 : qu'est-ce qui rend le contenu assez désirable pour soutenir toute l'économie ?**

---

## 6. Ce qui vit où

| Sur le DAG (immuable, vérifiable) | Hors DAG (autre repo, services internes) |
|---|---|
| Soldes, transferts, mint, burn | Site de contenu (vidéos…) |
| Règles d'émission & budget | Farm + anti-sybil (proof of personhood) |
| Paramètres de gouvernance + historique des changements | Comptes / identité / KYC |
| Snapshots de réserve (proof-of-reserves) | Pricing du contenu, on-ramp fiat→PMS |
| Taux FX appliqués (traçabilité) | Calcul des taux, anti-fraude, relevés |

---

## 7. Roadmap

### Phase 0 — Sécurité (bloquant)
- [ ] Correctifs **C-1 / C-2** (binding pubkey↔owner + signatures TX dans le hot path). Voir `pms-spec-dag-implementation.md`.
- [ ] Tests de la **section 1** de `pms-checklist-tests.md` (rouges aujourd'hui = preuve de la faille).
- [ ] **Failover Coordinator** (SPOF de l'audit).

### Phase 1 — Monnaie de base
- [ ] Émission **taux cible seul** (2 %, couloir 10 %) inscrite dans le DAG.
- [ ] Voie de mint **A (achat direct / on-ramp)** + amorçage de liquidité (genesis 🔧).
- [ ] Rake + burn sur les transferts PMS.
- [ ] **Proof-of-reserves** (snapshot ancré) — rend les mains liées vérifiables.

### Phase 2 — Distribution par l'effort
- [ ] Ledger **scrip** + mécanisme de **farm** (anti-sybil : proof of personhood, coût d'entrée ; VR optionnelle pour le coût matériel + signature biomécanique).
- [ ] Pont **scrip→PMS** (voie B) : formule de taux R 🔧, scrip brûlé, plafonné par le budget.
- [ ] Ajout du signal **compensation du burn** à l'émission.

### Phase 3 — Écosystème
- [ ] Voie **C (contribution réseau)**.
- [ ] Voie **D (contenu vendu)** + signal **indexation à l'activité**.
- [ ] Time-lock natif & conditions de déverrouillage (escrow, vesting) — `pms-spec-dag-implementation.md` §2.1-2.2.

### Phase 4 — Gouvernance vivante
- [ ] Processus gradué (7/15/45 j) outillé et tracé dans le DAG.
- [ ] Votes **consultatifs**.
- [ ] (Optionnel, après avis juridique) votes contraignants.

---

## 8. Paramètres encore à fixer (🔧)

| # | Paramètre | Nécessite |
|---|---|---|
| 1 | Plancher anti-asphyxie de la masse | Décision de design |
| 2 | Formule exacte du taux d'émission (coeff. burn/activité) | Données réelles de lancement |
| 3 | Formule du taux FX scrip→PMS (forme de la dégradation) | Modélisation + données |
| 4 | Taux d'émission du farm et sa décroissance | Décision + simulation |
| 5 | Allocation du budget d'émission entre les 4 voies | Décision de politique |
| 6 | Supply genesis (amorçage liquidité) | Décision de lancement |

---

## 9. Avertissement juridique (prérequis, pas option)

Dès qu'il y a **conversion PMS ↔ argent réel**, et *a fortiori* avec des **votes / droits attachés au jeton** et une **entreprise** qui opère : terrain du **titre financier**, de **MiCA** (cadre crypto UE, en application 2024-2025), de l'**AML/KYC** (obligatoire sur les rampes fiat↔crypto) et de la **fiscalité** (revenu imposable pour les farmeurs). À **valider avec un avocat spécialisé avant tout lancement avec de l'argent réel** — ce point peut faire ou défaire le projet indépendamment de la technique. *(Claude n'est pas juriste ; ceci n'est pas un avis juridique.)*

---

## Questions stratégiques ouvertes

1. **Qu'est-ce qui rend le contenu assez désirable** pour soutenir toute l'économie ? (tout repose là-dessus)
2. **Le plancher d'émission** et la **forme de la courbe FX** — à calibrer.
3. **L'amorçage de liquidité** au lancement (genesis + on-ramp).