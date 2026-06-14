//! Persistance de l'état du budget d'émission par période (plan §3.1).
//!
//! Stocke un **singleton** `EmissionEpochState` (JSON) qui survit aux restarts :
//! l'epoch courant, la supply de référence figée, le budget de la période et le
//! cumul déjà émis. C'est l'**ancre de recovery** du gate d'émission — au boot,
//! le miroir RAM du gate est ré-initialisé depuis cette valeur (jamais re-sommé
//! depuis les blocs : le DAG RAM ne garde que les N plus récents, et un scan du
//! CF `blocks` serait O(tous-les-blocs)).
//!
//! ## Sécurité (propriété P2 — exactement-une-fois)
//!
//! Le gate écrit ce compteur **avant** de forger le bloc de mint
//! (ordonnancement « counter-first ») : à tout instant, la valeur durable est
//! `≥` ce qui a réellement été émis. Un crash entre l'écriture du compteur et la
//! persistance durable du bloc ne peut donc causer qu'une **sous-émission**
//! conservatrice (jamais de dépassement du couloir), auto-réparée au prochain
//! rollover d'epoch. Voir `crates/pms-server/src/emission/`.
//!
//! ## Stockage
//!
//! Réutilise le CF `node_fee_pool` sous une clé dédiée — pas de nouveau column
//! family ni de migration `CURRENT_VER` (même approche que
//! [`reserve_snapshot`](super::reserve_snapshot) et `total_burned`).

use crate::rocks_store::store::RocksStore;
use anyhow::Result;

/// Clé du singleton dans le CF `node_fee_pool`.
const EMISSION_EPOCH_STATE_KEY: &[u8] = b"emission_epoch_state";

impl RocksStore {
    /// Enregistre l'état du budget d'émission de la période courante.
    ///
    /// `state_json` = `{ epoch_id, supply_ref, budget, emitted }` sérialisé par
    /// le gate (`crates/pms-server/src/emission/EmissionEpochState`). Écriture
    /// synchrone d'une seule clé — appelée sous le mutex du gate, donc jamais en
    /// concurrence avec elle-même.
    pub fn record_emission_epoch_state(&self, state_json: &str) -> Result<()> {
        let cf = self.cf("node_fee_pool");
        self.db
            .put_cf(&cf, EMISSION_EPOCH_STATE_KEY, state_json.as_bytes())?;
        Ok(())
    }

    /// Dernier état du budget d'émission ancré (JSON), `None` si jamais émis.
    pub fn latest_emission_epoch_state(&self) -> Result<Option<String>> {
        let cf = self.cf("node_fee_pool");
        Ok(self
            .db
            .get_cf(&cf, EMISSION_EPOCH_STATE_KEY)?
            .map(|v| String::from_utf8_lossy(&v).into_owned()))
    }
}
