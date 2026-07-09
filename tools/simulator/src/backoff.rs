//! Backoff sur erreurs d'état + throttle global de logs anti-spam.
//!
//! Deux outils complémentaires contre le hot-loop de retries/logs quand une
//! ressource distante est dans un état durablement bloquant (ex: wallet
//! coordinator à sec — incident testnet du 2026-07-09 : ~24 000
//! `Refuel failed` WARN/heure, réduisant la fenêtre de rétention des logs
//! Docker à ~4 h) :
//!
//! - [`StateBackoff`] — backoff exponentiel **par agent** : après une erreur
//!   d'ÉTAT (cf. [`crate::error::SimError::is_state_error`]), les tentatives
//!   sont suspendues pendant une durée qui double à chaque échec consécutif
//!   (jitter ±20 % pour éviter un thundering herd à la récupération). Les
//!   erreurs transitoires (réseau, timeout, 5xx) ne déclenchent PAS de
//!   backoff — retry au tick suivant, comportement historique.
//! - [`log_throttle`] — throttle **global** par clé : émet au plus un log par
//!   période, en comptant les occurrences étouffées entre deux émissions.
//!   Indispensable quand des centaines d'agents partagent la même cause
//!   racine (un seul wallet source) : sans lui, même avec backoff par agent,
//!   N agents × 1 warn/cap = des milliers de lignes/heure.
//!
//! Ce pattern suit la règle globale « state-divergence vs transient errors » :
//! une erreur d'état signifie que l'état distant rend l'opération impossible
//! tant qu'un tiers n'intervient pas — retenter à chaud est du spam pur.

use std::time::{Duration, Instant};

/// Période standard des warns throttlés via [`log_throttle`] : au plus une
/// ligne par minute et par clé, pour toute la flotte.
pub const WARN_THROTTLE_PERIOD: Duration = Duration::from_secs(60);

/// Jitter appliqué à chaque backoff d'état (±20 %).
const JITTER_PCT: f64 = 0.2;

/// Facteur de jitter multiplicatif `1 ± pct` — partagé par le backoff d'état
/// et le cooldown de burn ([`crate::agent::random`]) pour éviter que la
/// flotte ne retente/burn en lockstep après un événement commun.
pub fn jitter_factor(pct: f64) -> f64 {
    use rand::Rng;
    rand::rng().random_range(1.0 - pct..=1.0 + pct)
}

/// Backoff exponentiel déclenché uniquement par des erreurs d'ÉTAT.
///
/// Toutes les méthodes sensibles au temps prennent un `now: Instant`
/// explicite pour être testables déterministiquement.
///
/// # Examples
///
/// ```ignore
/// let mut backoff = StateBackoff::new(Duration::from_secs(30), Duration::from_secs(900));
/// let now = Instant::now();
/// assert!(backoff.should_attempt(now));           // pas de backoff actif
/// backoff.on_state_failure(now);                  // échec d'état → ~30s de pause
/// assert!(!backoff.should_attempt(now));          // tentative refusée, comptée
/// ```
pub struct StateBackoff {
    /// Durée du premier backoff après le premier échec d'état.
    base: Duration,
    /// Plafond du backoff (avant jitter ±20 %).
    cap: Duration,
    /// Épisode d'échec en cours : (backoff courant avant jitter, instant
    /// avant lequel toute tentative est refusée). `None` = aucun épisode.
    episode: Option<(Duration, Instant)>,
    /// Tentatives refusées (skippées) depuis le début de l'épisode.
    suppressed: u64,
}

impl StateBackoff {
    /// Crée un backoff inactif. `base` = première pause, `cap` = plafond.
    pub fn new(base: Duration, cap: Duration) -> Self {
        Self {
            base,
            cap,
            episode: None,
            suppressed: 0,
        }
    }

    /// Autorise ou refuse une tentative à l'instant `now`.
    ///
    /// Refuse (et compte l'occurrence dans `suppressed`) si un backoff est
    /// actif et non écoulé. Autorise sinon — y compris à l'expiration du
    /// backoff, où la tentative suivante décidera de la suite (succès →
    /// [`Self::reset`], nouvel échec d'état → backoff doublé).
    pub fn should_attempt(&mut self, now: Instant) -> bool {
        match self.episode {
            Some((_, next_attempt)) if now < next_attempt => {
                self.suppressed += 1;
                false
            }
            _ => true,
        }
    }

    /// Enregistre un échec d'ÉTAT : double le backoff courant (borné à
    /// `cap`), applique un jitter ±20 % (après le cap — le backoff effectif
    /// peut donc atteindre `cap × 1.2`), et programme la prochaine tentative
    /// à `now + backoff`. Retourne la durée effective (jitter inclus).
    pub fn on_state_failure(&mut self, now: Instant) -> Duration {
        let next = match self.episode {
            None => self.base,
            Some((d, _)) => (d * 2).min(self.cap),
        };
        let jittered = next.mul_f64(jitter_factor(JITTER_PCT));
        self.episode = Some((next, now + jittered));
        jittered
    }

    /// Clôt l'épisode (succès, ou récupération observée par un autre canal).
    /// Retourne le nombre de tentatives étouffées pendant l'épisode.
    pub fn reset(&mut self) -> u64 {
        let n = self.suppressed;
        self.episode = None;
        self.suppressed = 0;
        n
    }

    /// Un épisode de backoff est-il en cours ?
    pub fn is_active(&self) -> bool {
        self.episode.is_some()
    }
}

/// Throttle global de logs par clé statique.
///
/// Partagé par tous les agents du process : quand N agents échouent pour la
/// même cause racine, une seule ligne par période est émise, porteuse du
/// nombre d'occurrences étouffées — le volume de logs devient indépendant du
/// nombre d'agents.
pub mod log_throttle {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    struct Entry {
        last_emit: Instant,
        suppressed: u64,
    }

    fn registry() -> &'static Mutex<HashMap<&'static str, Entry>> {
        static REG: OnceLock<Mutex<HashMap<&'static str, Entry>>> = OnceLock::new();
        REG.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Autorise ou étouffe un log pour `key`.
    ///
    /// Retourne `Some(suppressed)` si le log est autorisé (première
    /// occurrence, ou période écoulée depuis la dernière émission) —
    /// `suppressed` est le nombre d'occurrences étouffées entre-temps, à
    /// inclure dans le message. Retourne `None` si le log doit être étouffé.
    pub fn allow(key: &'static str, period: Duration) -> Option<u64> {
        allow_at(key, period, Instant::now())
    }

    /// Variante testable de [`allow`] avec horloge explicite.
    pub fn allow_at(key: &'static str, period: Duration, now: Instant) -> Option<u64> {
        let mut reg = registry().lock().unwrap_or_else(|p| p.into_inner());
        match reg.get_mut(key) {
            None => {
                reg.insert(
                    key,
                    Entry {
                        last_emit: now,
                        suppressed: 0,
                    },
                );
                Some(0)
            }
            Some(e) if now.duration_since(e.last_emit) >= period => {
                let n = e.suppressed;
                e.last_emit = now;
                e.suppressed = 0;
                Some(n)
            }
            Some(e) => {
                e.suppressed += 1;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: Duration = Duration::from_secs(30);
    const CAP: Duration = Duration::from_secs(900);

    /// Bornes de jitter : durée attendue ±20 %.
    fn assert_jittered(actual: Duration, expected: Duration, label: &str) {
        let lo = expected.mul_f64(0.8);
        let hi = expected.mul_f64(1.2);
        println!(
            "  {label}: backoff effectif = {actual:?} (attendu {expected:?} ±20% → [{lo:?}, {hi:?}])"
        );
        assert!(
            actual >= lo && actual <= hi,
            "{label}: {actual:?} hors de [{lo:?}, {hi:?}]"
        );
    }

    #[test]
    fn test_backoff_progression_and_cap() {
        let mut b = StateBackoff::new(BASE, CAP);
        let t0 = Instant::now();

        println!("=== progression exponentielle 30s → cap 900s ===");
        assert!(b.should_attempt(t0), "aucun backoff au départ");
        assert!(!b.is_active());

        // Échecs consécutifs : 30s, 60s, 120s, 240s, 480s, 900s (cap), 900s…
        let expected = [30u64, 60, 120, 240, 480, 900, 900];
        let mut now = t0;
        for (i, exp) in expected.iter().enumerate() {
            let d = b.on_state_failure(now);
            assert_jittered(d, Duration::from_secs(*exp), &format!("échec #{}", i + 1));
            assert!(b.is_active());
            // Pendant le backoff : tentative refusée et comptée
            assert!(!b.should_attempt(now + Duration::from_millis(1)));
            // Après expiration (borne haute du jitter) : tentative autorisée
            now = now + d + Duration::from_millis(1);
            assert!(b.should_attempt(now), "échec #{} : devrait réautoriser après {d:?}", i + 1);
        }

        let suppressed = b.reset();
        println!("reset: {suppressed} tentatives étouffées pendant l'épisode");
        assert_eq!(suppressed, expected.len() as u64);
        assert!(!b.is_active());
        // Après reset : le prochain échec repart de la base
        let d = b.on_state_failure(now);
        assert_jittered(d, BASE, "premier échec post-reset");
    }

    #[test]
    fn test_backoff_counts_suppressed_attempts() {
        let mut b = StateBackoff::new(BASE, CAP);
        let t0 = Instant::now();
        let d = b.on_state_failure(t0);

        // 5 ticks pendant la fenêtre de backoff → 5 refus comptés
        for i in 0..5 {
            let refused = !b.should_attempt(t0 + Duration::from_secs(i));
            println!("tick +{i}s pendant backoff de {d:?} → refusé = {refused}");
            assert!(refused);
        }
        assert_eq!(b.reset(), 5);
        println!("reset → 5 tentatives étouffées confirmées");
    }

    #[test]
    fn test_log_throttle_window() {
        use super::log_throttle::allow_at;
        const PERIOD: Duration = Duration::from_secs(60);
        // Clé unique à ce test : le registre est global au process.
        const KEY: &str = "test_log_throttle_window";
        let t0 = Instant::now();

        let first = allow_at(KEY, PERIOD, t0);
        println!("t+0s   première occurrence → {first:?}");
        assert_eq!(first, Some(0));

        for s in [1u64, 10, 59] {
            let r = allow_at(KEY, PERIOD, t0 + Duration::from_secs(s));
            println!("t+{s}s  dans la période → {r:?}");
            assert_eq!(r, None);
        }

        let after = allow_at(KEY, PERIOD, t0 + Duration::from_secs(60));
        println!("t+60s  période écoulée → {after:?} (3 étouffées)");
        assert_eq!(after, Some(3));

        // Le compteur repart de zéro après émission
        let next = allow_at(KEY, PERIOD, t0 + Duration::from_secs(121));
        println!("t+121s période suivante sans étouffées → {next:?}");
        assert_eq!(next, Some(0));
    }

    #[test]
    fn test_log_throttle_volume_fleet_wide() {
        use super::log_throttle::allow_at;
        const PERIOD: Duration = Duration::from_secs(60);
        const KEY: &str = "test_log_throttle_volume_fleet_wide";
        let t0 = Instant::now();

        // Simule 800 agents × 1 échec toutes les 8s pendant 1h (~360 000
        // occurrences) : le throttle doit émettre ≤ 61 lignes.
        let mut emitted = 0u64;
        let mut occurrences = 0u64;
        for tick in 0..(3600 / 8) {
            for _agent in 0..800 {
                occurrences += 1;
                if allow_at(KEY, PERIOD, t0 + Duration::from_secs(tick * 8)).is_some() {
                    emitted += 1;
                }
            }
        }
        println!("{occurrences} occurrences sur 1h simulée → {emitted} lignes émises (avant: {occurrences})");
        assert!(emitted <= 61, "throttle inefficace : {emitted} lignes émises");
        assert!(emitted >= 55, "throttle trop agressif : {emitted} lignes émises");
    }
}
