use pms_config::LedgerDef;

/// Logique d'autorisation pour les opérations de pont.
pub struct BridgeAuth;

impl BridgeAuth {
    /// Un des deux ledgers est-il "main" (admin-owned) ?
    /// Si oui, seul l'admin peut gérer le pont.
    pub fn requires_admin(ledger_a: &LedgerDef, ledger_b: &LedgerDef) -> bool {
        ledger_a.owner_pubkey.is_none() || ledger_b.owner_pubkey.is_none()
    }

    /// Vérifie si l'appelant est autorisé à activer un pont entre deux ledgers.
    ///
    /// - Si l'un des ledgers est admin-owned → admin_token requis
    /// - Sinon → les deux owners doivent signer (ou admin override)
    pub fn can_enable(
        ledger_a: &LedgerDef,
        ledger_b: &LedgerDef,
        is_admin: bool,
        signer_pubkey: Option<&str>,
    ) -> bool {
        // Admin peut toujours tout faire
        if is_admin {
            return true;
        }
        // Si un des ledgers est admin-owned, seul l'admin peut
        if Self::requires_admin(ledger_a, ledger_b) {
            return false;
        }
        // Custom ↔ Custom : le signataire doit être owner des deux
        // (ou on pourrait accepter si owner d'un seul — mais enable nécessite accord mutuel)
        if let Some(signer) = signer_pubkey {
            let is_owner_a = ledger_a.owner_pubkey.as_deref() == Some(signer);
            let is_owner_b = ledger_b.owner_pubkey.as_deref() == Some(signer);
            // Pour l'instant on demande que le signataire soit owner d'au moins un des deux
            // TODO: two-party consent (signature des deux owners)
            is_owner_a || is_owner_b
        } else {
            false
        }
    }

    /// Vérifie si l'appelant est autorisé à désactiver un pont.
    ///
    /// - Admin → toujours OK
    /// - Owner d'un des deux ledgers → OK (un seul suffit pour couper)
    pub fn can_disable(
        ledger_a: &LedgerDef,
        ledger_b: &LedgerDef,
        is_admin: bool,
        signer_pubkey: Option<&str>,
    ) -> bool {
        if is_admin {
            return true;
        }
        if Self::requires_admin(ledger_a, ledger_b) {
            return false;
        }
        if let Some(signer) = signer_pubkey {
            let is_owner_a = ledger_a.owner_pubkey.as_deref() == Some(signer);
            let is_owner_b = ledger_b.owner_pubkey.as_deref() == Some(signer);
            is_owner_a || is_owner_b
        } else {
            false
        }
    }

    /// Autorise l'INITIATION d'un transfert via ce pont **au niveau ledger**.
    ///
    /// **SÉCURITÉ (durcissement v0.30.1)** : renvoie `true` UNIQUEMENT pour
    /// l'admin. Auparavant l'owner du ledger source était accepté — mais un
    /// `BridgeLock` détruit les UTXOs d'un `from_address` ARBITRAIRE (pas
    /// forcément celui de l'owner), donc autoriser sur la seule propriété du
    /// ledger laissait un owner **drainer n'importe quel utilisateur de son
    /// ledger**. La propriété du ledger n'est PAS une preuve de contrôle des
    /// fonds de `from_address`.
    ///
    /// Un appelant NON-admin doit prouver le contrôle de `from_address` lui-même
    /// (signature de son propriétaire, cf. `bridge_transfer_signing_message` +
    /// `from_address_control_proven` côté serveur), pas via cette fonction. Ce
    /// n'est donc plus qu'un gate opérateur au niveau ledger.
    pub fn can_transfer(
        _source_ledger: &LedgerDef,
        _dest_ledger: &LedgerDef,
        is_admin: bool,
        _signer_pubkey: Option<&str>,
    ) -> bool {
        is_admin
    }
}
