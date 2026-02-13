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
            let is_owner_a = ledger_a
                .owner_pubkey
                .as_deref()
                .map_or(false, |pk| pk == signer);
            let is_owner_b = ledger_b
                .owner_pubkey
                .as_deref()
                .map_or(false, |pk| pk == signer);
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
            let is_owner_a = ledger_a
                .owner_pubkey
                .as_deref()
                .map_or(false, |pk| pk == signer);
            let is_owner_b = ledger_b
                .owner_pubkey
                .as_deref()
                .map_or(false, |pk| pk == signer);
            is_owner_a || is_owner_b
        } else {
            false
        }
    }

    /// Vérifie si l'appelant est autorisé à faire un transfert via ce pont.
    ///
    /// - Admin → toujours OK
    /// - Owner du ledger source → OK
    pub fn can_transfer(
        source_ledger: &LedgerDef,
        dest_ledger: &LedgerDef,
        is_admin: bool,
        signer_pubkey: Option<&str>,
    ) -> bool {
        if is_admin {
            return true;
        }
        if Self::requires_admin(source_ledger, dest_ledger) {
            return false;
        }
        if let Some(signer) = signer_pubkey {
            source_ledger
                .owner_pubkey
                .as_deref()
                .map_or(false, |pk| pk == signer)
        } else {
            false
        }
    }
}
