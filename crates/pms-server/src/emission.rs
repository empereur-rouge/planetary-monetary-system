//! Budget d'émission partagé du PMS natif (plan §3.1 — la règle non-négociable).
//!
//! Toutes les voies de mint de PMS natif sur le ledger **main** (baseline taux
//! cible, on-ramp fiat, pont scrip→PMS, faucet) puisent dans **un seul** budget
//! par période. Sans ce budget commun, N voies de mint = N planches à billets ;
//! avec lui, on peut en ouvrir autant qu'on veut sans risque inflationniste.
//!
//! ## Deux propriétés de sécurité (voir `pms-spec-emission-budget.md`)
//!
//! - **P1 — Plafond** : sur toute période *p*, `Σ émission_p ≤ budget_p`. Garanti
//!   par le check-and-decrement atomique de [`EmissionGate::reserve`] sous le
//!   mutex (ferme le TOCTOU : le chemin de forge des mints est concurrent et n'a
//!   aucun lock global aujourd'hui).
//! - **P2 — Exactement-une-fois** : le compteur est persisté **avant** que le
//!   bloc ne soit forgé (« counter-first »). À tout instant la valeur durable
//!   est `≥` ce qui a réellement été émis ; un crash ne peut causer qu'une
//!   sous-émission conservatrice (jamais de dépassement), auto-réparée au
//!   prochain rollover d'epoch.
//!
//! ## Le couloir DUR
//!
//! Le budget est `supply_ref × clamp(taux_cible, plancher, plafond) × frac_année`.
//! Le `clamp` au plafond est la matérialisation en code de la promesse du plan
//! §2.1 : même si la config pousse le taux cible à 50 %, le budget est plafonné
//! au plafond (10 %/an par défaut). C'est inviolable sans changer la config de
//! boot (pas de hot-swap tant que la gouvernance timelock n'existe pas).

use pms_storage::rocks_store::store::RocksStore;
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use serde::{Deserialize, Serialize};

/// Secondes par an — convention `/365` du moteur (cohérent avec l'inflation
/// historique `supply × rate / 365`). 365 × 86 400.
pub const SECONDS_PER_YEAR: u64 = 365 * 86_400;

/// État du budget d'émission de la période courante. Singleton persisté
/// (ancre de recovery au boot), miroir RAM = source de vérité runtime.
// `Default` dérivé : tous les champs valent leur zéro (`u64`=0,
// `Decimal::default()`==`Decimal::ZERO`). Pas de footgun derived-vs-serde ici —
// la struct n'a aucun `#[serde(default=...)]` (la persistance passe par le DTO
// `PersistedEmission`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EmissionEpochState {
    /// Index de la période courante = `now_ms / (epoch_duration_sec × 1000)`.
    pub epoch_id: u64,
    /// Supply native figée au début de la période (D1 : budget stable/auditable).
    pub supply_ref: Decimal,
    /// Budget de la période (calculé une fois au rollover).
    pub budget: Decimal,
    /// Cumul réservé/émis dans cette période (monotone croissant intra-epoch).
    pub emitted: Decimal,
}

/// DTO de persistance — Decimal encodés en **string** pour un round-trip exact
/// (même approche que `total_burned`, jamais de perte de précision float).
#[derive(Serialize, Deserialize)]
struct PersistedEmission {
    epoch_id: u64,
    supply_ref: String,
    budget: String,
    emitted: String,
}

impl EmissionEpochState {
    fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&PersistedEmission {
            epoch_id: self.epoch_id,
            supply_ref: self.supply_ref.to_string(),
            budget: self.budget.to_string(),
            emitted: self.emitted.to_string(),
        })
    }

    fn from_json(s: &str) -> Option<Self> {
        let p: PersistedEmission = serde_json::from_str(s).ok()?;
        Some(Self {
            epoch_id: p.epoch_id,
            supply_ref: Decimal::from_str_exact(&p.supply_ref).ok()?,
            budget: Decimal::from_str_exact(&p.budget).ok()?,
            emitted: Decimal::from_str_exact(&p.emitted).ok()?,
        })
    }

    /// Budget restant de la période (jamais négatif).
    pub fn remaining(&self) -> Decimal {
        (self.budget - self.emitted).max(Decimal::ZERO)
    }
}

/// Paramètres du couloir d'émission (issus de `FeesSettings`).
#[derive(Debug, Clone, Copy)]
pub struct EmissionParams {
    /// Taux cible (% / an) — `annual_inflation_percent`.
    pub target_pct: f64,
    /// Plafond DUR du couloir (% / an) — `annual_ceiling_percent`.
    pub ceiling_pct: f64,
    /// Plancher du couloir (% / an) — `annual_floor_percent`.
    pub floor_pct: f64,
    /// Durée d'une période (secondes) — `emission_epoch_duration_sec`.
    pub epoch_duration_sec: u64,
}

/// Voie de mint qui consomme le budget. Sert au labelling des métriques et à
/// distinguer la baseline (résidu) des voies à montant explicite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Voie {
    /// Baseline taux cible : minte le **résidu** (`budget − emitted`).
    Baseline,
    /// On-ramp fiat→PMS (voie A) : montant explicite.
    OnRamp,
    /// Pont scrip→PMS (voie B) vers main : montant explicite.
    BridgeScrip,
    /// Faucet (dev/testnet) : montant explicite.
    Faucet,
}

impl Voie {
    /// Label métrique stable.
    pub fn as_str(&self) -> &'static str {
        match self {
            Voie::Baseline => "baseline",
            Voie::OnRamp => "onramp",
            Voie::BridgeScrip => "bridge_scrip",
            Voie::Faucet => "faucet",
        }
    }
}

/// Erreur d'émission.
#[derive(Debug)]
pub enum EmissionError {
    /// Le budget de la période est épuisé : le montant demandé dépasse le
    /// restant. P1 appliqué — l'émission est refusée (jamais clampée en
    /// silence, jamais de dépassement).
    BudgetExhausted {
        voie: Voie,
        requested: Decimal,
        remaining: Decimal,
        budget: Decimal,
    },
    /// L'écriture durable du compteur a échoué — la réservation est annulée
    /// (rien n'a été réservé), l'appelant ne doit pas forger.
    Persist(anyhow::Error),
}

impl std::fmt::Display for EmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmissionError::BudgetExhausted {
                voie,
                requested,
                remaining,
                budget,
            } => write!(
                f,
                "emission budget exhausted (voie={}, requested={}, remaining={}, budget={})",
                voie.as_str(),
                requested,
                remaining,
                budget
            ),
            EmissionError::Persist(e) => write!(f, "emission counter persist failed: {}", e),
        }
    }
}

impl std::error::Error for EmissionError {}

/// Taux effectif borné au couloir `[plancher, plafond]`. **Pur.**
///
/// Le `clamp` est l'invariant de sécurité du couloir : quel que soit le taux
/// cible configuré, le résultat est dans `[floor, ceiling]`. Robuste si la
/// config a `floor > ceiling` (on prend l'intervalle normalisé).
pub fn effective_rate_pct(target_pct: f64, ceiling_pct: f64, floor_pct: f64) -> f64 {
    let lo = floor_pct.min(ceiling_pct).max(0.0);
    let hi = ceiling_pct.max(floor_pct).max(0.0);
    target_pct.clamp(lo, hi)
}

/// Budget d'émission d'une période. **Pur** — testable sans I/O.
///
/// `budget = supply_ref × (taux_effectif / 100) × (epoch / 1 an)`, arrondi à
/// 8 décimales. `supply_ref` négative ou `epoch_duration_sec == 0` ⇒ budget 0.
pub fn compute_epoch_budget(supply_ref: Decimal, p: EmissionParams) -> Decimal {
    if p.epoch_duration_sec == 0 {
        return Decimal::ZERO;
    }
    let eff = effective_rate_pct(p.target_pct, p.ceiling_pct, p.floor_pct);
    let rate = Decimal::from_f64(eff).unwrap_or(Decimal::ZERO) / Decimal::from(100u32);
    let frac_year = Decimal::from(p.epoch_duration_sec) / Decimal::from(SECONDS_PER_YEAR);
    (supply_ref.max(Decimal::ZERO) * rate * frac_year).round_dp(8)
}

/// Réservation accordée par le gate — montant autorisé pour cette émission.
#[derive(Debug, Clone)]
pub struct Reservation {
    /// Montant que l'appelant peut minter (résidu pour la baseline).
    pub amount: Decimal,
}

/// Gate d'émission — sérialise tous les forges de mint de PMS natif main et
/// applique le budget. Partagé via `Arc` dans `AppState`.
pub struct EmissionGate {
    state: tokio::sync::Mutex<EmissionEpochState>,
}

impl EmissionGate {
    /// Construit un gate à partir d'un état initial.
    pub fn new(initial: EmissionEpochState) -> Self {
        Self {
            state: tokio::sync::Mutex::new(initial),
        }
    }

    /// Recharge l'état durable depuis le store (recovery au boot). Vide si aucun
    /// état n'a jamais été ancré.
    pub fn load(store: &RocksStore) -> Self {
        let initial = store
            .latest_emission_epoch_state()
            .ok()
            .flatten()
            .and_then(|s| EmissionEpochState::from_json(&s))
            .unwrap_or_default();
        Self::new(initial)
    }

    /// Snapshot de l'état courant (pour les métriques). Clone bon marché.
    pub async fn snapshot(&self) -> EmissionEpochState {
        self.state.lock().await.clone()
    }

    /// Réserve une émission, atomiquement sous le mutex (ferme le TOCTOU).
    ///
    /// - `requested = None` (ou voie `Baseline`) ⇒ réserve le **résidu**
    ///   (`budget − emitted`) : la baseline complète jusqu'à la cible.
    /// - `requested = Some(x)` ⇒ réserve `x`, refusé si `x > restant` (P1).
    ///
    /// Le compteur est persisté **avant** le retour (counter-first → P2). En cas
    /// d'échec de persistance, la réservation RAM est annulée et `Persist` est
    /// renvoyée — l'appelant ne forge pas.
    ///
    /// `current_supply` n'est utilisée qu'au rollover (figée comme `supply_ref`).
    /// Un retour `Ok` avec `amount == 0` signifie « rien à émettre » (budget déjà
    /// consommé / nul) — l'appelant doit no-op, pas forger un bloc vide.
    pub async fn reserve(
        &self,
        store: &RocksStore,
        now_ms: u64,
        current_supply: Decimal,
        params: EmissionParams,
        voie: Voie,
        requested: Option<Decimal>,
    ) -> Result<Reservation, EmissionError> {
        let mut st = self.state.lock().await;

        // Rollover FORWARD uniquement : une horloge qui recule ne réinitialise
        // jamais `emitted` (sinon ré-émission ⇒ dépassement). Le premier appel
        // (epoch_id=0) roule toujours (now_ms/epoch_dur ≫ 0).
        let epoch_dur_ms = params.epoch_duration_sec.saturating_mul(1000).max(1);
        let epoch = now_ms / epoch_dur_ms;
        if epoch > st.epoch_id {
            st.epoch_id = epoch;
            st.supply_ref = current_supply.max(Decimal::ZERO);
            st.budget = compute_epoch_budget(st.supply_ref, params);
            st.emitted = Decimal::ZERO;
        }

        let remaining = st.remaining();
        let amount = match (voie, requested) {
            (Voie::Baseline, _) => remaining, // résidu
            (_, Some(req)) => req,
            (_, None) => remaining, // défensif : voie sans montant ⇒ résidu
        };

        if amount <= Decimal::ZERO {
            return Ok(Reservation {
                amount: Decimal::ZERO,
            });
        }
        if amount > remaining {
            // P1 appliqué dans le gate : compté ici, donc TOUTE voie qui dépasse
            // le budget est observée sans que chaque handler ait à y penser.
            crate::metrics::EMISSION_REJECTIONS
                .with_label_values(&[voie.as_str()])
                .inc();
            return Err(EmissionError::BudgetExhausted {
                voie,
                requested: amount,
                remaining,
                budget: st.budget,
            });
        }

        // Réservation optimiste + écriture durable counter-first.
        st.emitted += amount;
        if let Err(e) = persist_locked(store, &st) {
            st.emitted -= amount; // rollback RAM
            return Err(EmissionError::Persist(e));
        }
        Ok(Reservation { amount })
    }

    /// Annule une réservation dont la persistance du bloc a échoué (rollback du
    /// budget consommé). Best-effort sur l'écriture durable : si elle échoue, la
    /// valeur durable sur-compte ⇒ sous-émission conservatrice (P1 préservé).
    pub async fn release(&self, store: &RocksStore, amount: Decimal) {
        if amount <= Decimal::ZERO {
            return;
        }
        let mut st = self.state.lock().await;
        st.emitted = (st.emitted - amount).max(Decimal::ZERO);
        let _ = persist_locked(store, &st);
    }
}

/// Sérialise + persiste l'état (appelé sous le mutex du gate).
fn persist_locked(store: &RocksStore, st: &EmissionEpochState) -> anyhow::Result<()> {
    let json = st.to_json()?;
    store.record_emission_epoch_state(&json)
}

#[cfg(test)]
mod tests {
    //! Tests PURS du calcul du budget — golden hardcodés, indépendants de la
    //! formule de prod (anti-tautologie). Les tests du gate (TOCTOU, exhaustion,
    //! crash-reload, rollback, résidu) sont dans
    //! `crates/pms-server/tests/emission_budget_test.rs` (nécessitent un store).
    use super::*;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str_exact(s).unwrap()
    }

    #[test]
    fn t1_budget_from_target_rate() {
        // supply 1000, cible 2 %/an, période 1 jour → 1000 × 0.02 / 365.
        let p = EmissionParams {
            target_pct: 2.0,
            ceiling_pct: 10.0,
            floor_pct: 0.0,
            epoch_duration_sec: 86_400,
        };
        let budget = compute_epoch_budget(dec("1000"), p);
        println!("T1 budget(supply=1000, target=2%/yr, epoch=1d) = {}", budget);
        assert_eq!(
            budget,
            dec("0.05479452"),
            "budget journalier à 2 %/an sur 1000 de supply"
        );
    }

    #[test]
    fn t2_corridor_clamps_to_ceiling() {
        // La config pousse 50 %/an mais le plafond est 10 % → le budget DOIT
        // utiliser 10 %, jamais 50 %. C'est le cœur de P1 (couloir inviolable).
        let p = EmissionParams {
            target_pct: 50.0,
            ceiling_pct: 10.0,
            floor_pct: 0.0,
            epoch_duration_sec: 86_400,
        };
        let budget = compute_epoch_budget(dec("1000"), p);
        let at_ceiling = dec("0.27397260"); // 1000 × 0.10 / 365
        let at_50pct = dec("1.36986301"); // 1000 × 0.50 / 365 (ce qu'un bug NON-clampé donnerait)
        println!(
            "T2 budget(target=50%, ceiling=10%) = {} (attendu plafond={}, bug 50%={})",
            budget, at_ceiling, at_50pct
        );
        assert_eq!(
            budget, at_ceiling,
            "le couloir DOIT ramener la cible 50 % au plafond 10 %"
        );
        assert_ne!(
            budget, at_50pct,
            "le budget ne DOIT JAMAIS honorer une cible au-dessus du plafond"
        );
    }

    #[test]
    fn t_effective_rate_clamps_both_ways() {
        // cible sous le plancher → plancher
        assert_eq!(effective_rate_pct(1.0, 10.0, 5.0), 5.0);
        // cible au-dessus du plafond → plafond
        assert_eq!(effective_rate_pct(50.0, 10.0, 0.0), 10.0);
        // cible dans le couloir → inchangée
        assert_eq!(effective_rate_pct(7.0, 10.0, 0.0), 7.0);
        // config incohérente (floor > ceiling) → intervalle normalisé, jamais de panique
        let r = effective_rate_pct(7.0, 2.0, 9.0);
        println!("T_eff clamp(7, ceiling=2, floor=9) = {}", r);
        assert!((2.0..=9.0).contains(&r));
    }

    #[test]
    fn t_zero_epoch_yields_zero_budget() {
        let p = EmissionParams {
            target_pct: 2.0,
            ceiling_pct: 10.0,
            floor_pct: 0.0,
            epoch_duration_sec: 0,
        };
        assert_eq!(compute_epoch_budget(dec("1000"), p), Decimal::ZERO);
    }

    #[test]
    fn t_state_json_round_trips_exactly() {
        // Round-trip de persistance : pas de perte de précision (Decimal en str).
        let st = EmissionEpochState {
            epoch_id: 20_123,
            supply_ref: dec("1234567.89012345"),
            budget: dec("0.05479452"),
            emitted: dec("0.01234567"),
        };
        let json = st.to_json().unwrap();
        println!("T_json persisted = {}", json);
        let back = EmissionEpochState::from_json(&json).unwrap();
        assert_eq!(st, back, "le round-trip JSON doit être exact (Decimal en string)");
    }
}
