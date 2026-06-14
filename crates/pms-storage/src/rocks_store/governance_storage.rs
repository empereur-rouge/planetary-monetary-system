//! Persistance des propositions de gouvernance timelock (plan §4).
//!
//! CF `governance_proposals` : `proposal_id` → `GovernanceProposalRecord` (JSON).
//! Source de vérité requêtable pour `GET /v1/governance/{pending,history}` et
//! pour la validation de l'enact (timelock écoulé + statut `Pending`).
//!
//! Le bloc DAG `GovernanceProposal` reste la preuve immuable (signée Coordinator,
//! horodatée) ; ce CF est l'index d'état matérialisé (avec le statut courant).

use crate::governance_store::GovernanceStorage;
use crate::rocks_store::store::RocksStore;
use anyhow::Result;
use pms_config::{GovernanceProposalRecord, GovernanceStatus};

impl GovernanceStorage for RocksStore {
    /// Enregistre (ou écrase) une proposition de gouvernance.
    fn put_governance_proposal(&self, record: &GovernanceProposalRecord) -> Result<()> {
        let cf = self.cf("governance_proposals");
        let json = serde_json::to_vec(record)?;
        self.db.put_cf(&cf, record.proposal_id.as_bytes(), &json)?;
        Ok(())
    }

    /// Récupère une proposition par son id, `None` si inconnue.
    fn get_governance_proposal(
        &self,
        proposal_id: &str,
    ) -> Result<Option<GovernanceProposalRecord>> {
        let cf = self.cf("governance_proposals");
        match self.db.get_cf(&cf, proposal_id.as_bytes())? {
            Some(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            None => Ok(None),
        }
    }

    /// Mute le statut d'une proposition existante (`Pending` → `Enacted` /
    /// `Cancelled`). Erreur si la proposition est inconnue.
    fn set_governance_status(
        &self,
        proposal_id: &str,
        status: GovernanceStatus,
        action_block_id: &str,
    ) -> Result<()> {
        let mut record = self
            .get_governance_proposal(proposal_id)?
            .ok_or_else(|| anyhow::anyhow!("governance proposal not found: {proposal_id}"))?;
        record.status = status;
        // Mémorise le bloc qui a provoqué la transition (audit + index des blocs de
        // gouvernance) — l'enact ou le cancel selon le statut cible.
        match status {
            GovernanceStatus::Enacted => record.enact_block_id = Some(action_block_id.to_string()),
            GovernanceStatus::Cancelled => {
                record.cancel_block_id = Some(action_block_id.to_string())
            }
            GovernanceStatus::Pending => {}
        }
        self.put_governance_proposal(&record)
    }

    /// Liste toutes les propositions (tous statuts). L'appelant filtre par
    /// statut pour `/pending` vs `/history`.
    fn list_governance_proposals(&self) -> Result<Vec<GovernanceProposalRecord>> {
        let cf = self.cf("governance_proposals");
        let mut out = Vec::new();
        for kv in self.db.iterator_cf(&cf, rocksdb::IteratorMode::Start) {
            let (_k, v) = kv?;
            // Tolère le marqueur __init__ de la migration (valeur vide).
            if let Ok(record) = serde_json::from_slice::<GovernanceProposalRecord>(&v) {
                out.push(record);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rocks_store::store::RocksMemoryConfig;
    use pms_config::{ConfigUpdate, GovernanceTier};
    use tempfile::TempDir;

    async fn fresh_store() -> (RocksStore, TempDir) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().to_string_lossy().into_owned();
        let store = RocksStore::new(&path, 16, "main", None, &RocksMemoryConfig::default())
            .await
            .expect("rocks");
        (store, dir)
    }

    fn rec(id: &str, tier: GovernanceTier, status: GovernanceStatus) -> GovernanceProposalRecord {
        GovernanceProposalRecord {
            proposal_id: id.into(),
            update: ConfigUpdate::SetFeeRate { bps: 300 },
            tier,
            reason: "test".into(),
            announced_at_ms: 1000,
            enact_after_ms: 1000 + tier.default_duration_ms(),
            status,
            proposal_block_id: format!("proposal-block-{id}"),
            enact_block_id: None,
            cancel_block_id: None,
        }
    }

    #[tokio::test]
    async fn governance_proposal_roundtrip_and_status() {
        let (store, _d) = fresh_store().await;

        assert!(
            store.list_governance_proposals().unwrap().is_empty(),
            "fresh store has no proposals"
        );

        // put + get (round-trip exact)
        let r = rec("p1", GovernanceTier::Policy, GovernanceStatus::Pending);
        store.put_governance_proposal(&r).unwrap();
        let got = store.get_governance_proposal("p1").unwrap().unwrap();
        println!("stored proposal: {got:?}");
        assert_eq!(got, r, "round-trip exact");
        assert_eq!(got.status, GovernanceStatus::Pending);
        // enact_after = announced + 15 jours (Policy) — golden hardcodé
        assert_eq!(got.enact_after_ms, 1000 + 15 * 86_400_000);

        // transition de statut + mémorisation du bloc d'enact
        store
            .set_governance_status("p1", GovernanceStatus::Enacted, "enact-block-p1")
            .unwrap();
        let enacted = store.get_governance_proposal("p1").unwrap().unwrap();
        assert_eq!(enacted.status, GovernanceStatus::Enacted);
        assert_eq!(
            enacted.enact_block_id.as_deref(),
            Some("enact-block-p1"),
            "le bloc d'enact est mémorisé dans le record"
        );
        assert_eq!(enacted.cancel_block_id, None);

        // inconnu → None / erreur
        assert!(store.get_governance_proposal("nope").unwrap().is_none());
        assert!(
            store
                .set_governance_status("nope", GovernanceStatus::Cancelled, "x")
                .is_err(),
            "status update on unknown proposal must error"
        );

        // list
        store
            .put_governance_proposal(&rec(
                "p2",
                GovernanceTier::Operator,
                GovernanceStatus::Pending,
            ))
            .unwrap();
        assert_eq!(store.list_governance_proposals().unwrap().len(), 2);
        println!("list len = 2 OK");
    }

    #[test]
    fn tier_durations_golden() {
        // Paliers 7 / 15 / 45 jours (plan §4.2) — golden hardcodé.
        assert_eq!(
            GovernanceTier::Operator.default_duration_ms(),
            7 * 86_400_000
        );
        assert_eq!(
            GovernanceTier::Policy.default_duration_ms(),
            15 * 86_400_000
        );
        assert_eq!(
            GovernanceTier::Constitution.default_duration_ms(),
            45 * 86_400_000
        );
        println!("tier durations 7/15/45j: OK");
    }
}
