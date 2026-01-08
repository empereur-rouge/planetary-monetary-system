use pms_config::Settings;
use pms_errors::ValidationError;
use pms_types::TxOutput;
use pms_wire::WireBlock;
use rust_decimal::Decimal;
use std::str::FromStr;

/// Pour l'instant : plafond "en dur" avec TODO pour la config.
/// Ex: 1_000_000 unités max par bloc de mint (string décimale).
const MAX_REWARD_PER_BLOCK_STR: &str = "1000000"; // TODO: déplacer en Settings

/// Vérifie la politique de mint pour un PlainPayload::Reward.
///
/// - `outputs` : les sorties du reward (PlainPayload::Reward { outputs }).
/// - `wb` : le WireBlock d'origine (pour id + signer_pk_hex).
/// - `settings` : config globale (on y lit les admins).
pub fn validate_mint_policy(
    outputs: &[TxOutput],
    wb: &WireBlock,
    settings: &Settings,
) -> Result<(), ValidationError> {
    let signer = wb.signer_pk_hex.trim();

    // ============================================================
    // 0) MODE TEST — override uniquement si prefix = "pms:dev"
    // ============================================================

    if let Ok(test_pk) = std::env::var("PMS_TEST_ADMIN_PUBKEY") {
        let test_pk = test_pk.trim();

        // On ne déclenche le mode test que si on est vraiment en réseau dev
        if matches!(settings.network.mode, pms_config::NetworkMode::Dev) && !test_pk.is_empty() {
            if !test_pk.eq_ignore_ascii_case(signer) {
                return Err(ValidationError::UnauthorizedMint {
                    id: wb.id.clone(),
                    signer_pk_hex: signer.to_string(),
                });
            }

            // OK → admin de test valide, on vérifie uniquement le montant
            return check_mint_amount(outputs, &wb.id);
        }
    }

    // ============================================================
    // 1) MODE NORMAL — prod/dev sans override
    // ============================================================

    let admin_pubkeys = &settings.admin.signer_pubkeys;

    // Si une liste explicite existe → on force le check strict
    if !admin_pubkeys.is_empty() {
        let ok = admin_pubkeys
            .iter()
            .any(|pk| pk.eq_ignore_ascii_case(signer));

        if !ok {
            return Err(ValidationError::UnauthorizedMint {
                id: wb.id.clone(),
                signer_pk_hex: signer.to_string(),
            });
        }
    } else {
        // Dev local sans liste admin → on laisse passer mais on log
        eprintln!("[policy] validate_mint_policy: signer_pubkeys vide, bypass admin check (dev?)");
    }

    // ============================================================
    // 2) Validation du montant total Mint
    // ============================================================
    check_mint_amount(outputs, &wb.id)
}

fn check_mint_amount(outputs: &[TxOutput], block_id: &str) -> Result<(), ValidationError> {
    let mut total = Decimal::ZERO;

    for out in outputs {
        let v = Decimal::from_str(&out.amount).map_err(|_| ValidationError::InvalidAmount {
            reason: format!("reward output amount not a valid decimal: '{}'", out.amount),
        })?;

        if v < Decimal::ZERO {
            return Err(ValidationError::InvalidAmount {
                reason: "reward output amount is negative".into(),
            });
        }

        total += v;
    }

    let max_reward =
        Decimal::from_str(MAX_REWARD_PER_BLOCK_STR).expect("MAX_REWARD_PER_BLOCK_STR constant");

    if total > max_reward {
        return Err(ValidationError::MintAmountTooHigh {
            id: block_id.to_string(),
            max_allowed: MAX_REWARD_PER_BLOCK_STR.to_string(),
        });
    }

    Ok(())
}
