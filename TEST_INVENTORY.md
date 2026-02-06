# 🧪 Inventaire Détaillé des Tests PMS

Ce document recense **tous** les tests du projet, classés par importance et par module. Il sert de référence exhaustive pour la validation.

## 🏷️ Légende des Importances

| Code | Niveau | Description |
| :---: | --- | --- |
| 🔴 | **CRITIQUE** | Sécurité, Consensus, Intégrité des données. **Doit passer à 100%.** |
| 🟡 | **IMPORTANT** | Fonctionnalités majeures (Fees, NFT, API). |
| 🟢 | **NORMAL** | Tests unitaires, cas limites (edge cases), utilitaires. |

---

## 📦 1. Core Logic (`crates/pms-core`)

### 🔴 Sécurité & Consensus

| Fichier Test | Objectif & Tests |
|---|---|
| **`single_writer_enforcement.rs`** | **Objectif : Garantir le mode Private DAG (Unique Coordinateur)**<br>• `test_single_parent_accepted`: Bloc avec 1 parent OK<br>• `test_multiple_parents_rejected`: Bloc avec 2+ parents **REJETÉ** (`TooManyParents`)<br>• `test_zero_parents_rejected_if_not_genesis`: Bloc orphelin **REJETÉ**<br>• `test_genesis_zero_parents_accepted`: Genesis sans parent OK<br>• `test_genesis_with_parents_rejected`: Genesis avec parent **REJETÉ**<br>• `test_coordinator_signature_comparison_logic`: Comparaison de clés robuste<br>• `test_error_message_contains_parent_count`: Validation message erreur |
| **`mint_security.rs`** | **Objectif : Empêcher la création frauduleuse de Tokens**<br>• `test_coordinator_can_mint`<br>• `test_random_key_cannot_mint`<br>• `test_empty_signer_cannot_mint`<br>• `test_whitespace_signer_cannot_mint`<br>• `test_dev_mode_allows_mint_without_coordinator_key`<br>• `test_similar_but_different_key_fails`<br>• `test_coordinator_key_case_insensitive` |
| **`mint_security_full.rs`** | **Objectif : Sécurité étendue (Attaques)**<br>• `test_coordinator_full_chain_success`<br>• `test_attacker_valid_signature_but_unauthorized`<br>• `test_forged_signature_rejected`<br>• `test_tampered_message_rejected` |
| **`tx_validation.rs`** | **Objectif : Intégrité des Transactions**<br>• `accept_valid_tx`<br>• `reject_double_spend_intra_block` |

### 🟡 Règles Métier

| Fichier Test | Objectif & Tests |
|---|---|
| **`mint_policy_and_fees.rs`** | **Objectif : Respect des plafonds et frais**<br>• `mint_policy_enforced_on_admin_vs_non_admin`<br>• `tx_utxo_with_fee_above_policy_is_rejected`<br>• `tx_utxo_with_fee_within_policy_is_accepted`<br>• `dev_mode_mint_signed_by_admin_is_accepted`<br>• `mint_policy_rejects_non_admin_in_mainnet` |
| **`nft_validation.rs`** | **Objectif : Gestion des NFTs**<br>• `test_mint_success`<br>• `test_mint_already_exists`<br>• `test_mint_unauthorized_signer`<br>• `test_transfer_success`<br>• `test_transfer_not_owner`<br>• `test_burn_success`<br>• `test_use_success`<br>• `test_token_not_found`<br>• `test_mint_restricted_to_coordinator`<br>• `test_cube_mint_without_authority_rejects_if_configured`<br>• `test_cube_mint_without_authority_succeeds_in_dev_mode`<br>• `test_non_cube_nft_ignores_authority_validation`<br>• `test_cube_mint_with_valid_authority_signature_succeeds` |
| **`tips_select.rs`** | **Objectif : Sélection des parents**<br>• `tips_selection_is_deterministic_and_excludes_finalized` |
| **`node_rewards_wallets.rs`** | **Objectif : Récompenses**<br>• `test_node_rewards_e2e_with_wallets` |

### 🟢 Infrastructure Core

| Fichier Test | Objectif & Tests |
|---|---|
| `stress_create_nodes.rs` | **Stress Tests** : `stress_create_1000_blocks_and_check_parents`, `stress_create_100_encrypted_blocks_and_check_parents` |
| `bootstrap_from_store.rs` | **Bootstrap** : `bootstrap_recovers_all_blocks_and_children_rocks` |
| `adapter_persist_pipeline.rs` | **Pipeline** : `signed_block_goes_through_full_pipeline` |

---

## 🖥️ 2. Serveur & E2E (`crates/pms-server`)

### 🔴 Scénarios Critiques & Sécurité

| Fichier Test | Objectif & Tests |
|---|---|
| **`distributed_tx_e2e.rs`** | **Objectif : Scénario Complet "Full Life"**<br>• `test_fee_pool_calculate_shares`<br>• `test_node_registry`<br>• *(ignored)* `test_distributed_tx_metrics` |
| **`submit_block_auth.rs`** | **Objectif : Auth Ingestion**<br>• `unsigned_block_is_rejected`<br>• `signed_plain_block_is_accepted`<br>• `signed_encrypted_mint_is_accepted` |
| **`security_http.rs`** | **Objectif : Protection HTTP**<br>• `admin_ping_requires_token`<br>• `timeout_layer_returns_408_on_slow_route`<br>• `body_limit_rejects_large_submit_block`<br>• `rate_limit_returns_429_when_spammed` |

### 🟡 Fonctionnalités

| Fichier Test | Objectif & Tests |
|---|---|
| `automated_distribution_test.rs` | **Distribution Auto** : `test_automated_fee_distribution` |
| `wallet_send_fees.rs` | **Wallet Fees** : `wallet_send_tx_injects_fee_and_admin_can_decrypt_fee_utxo` |
| `nft_e2e.rs` | **NFT API** : `nft_mint_and_verify_ownership`, `nft_transfer_changes_ownership`, `nft_burn_removes_token`, `nft_unauthorized_transfer_rejected`, `nft_get_by_owner_after_mint`, `nft_get_by_owner_updates_on_transfer`, `nft_get_by_owner_clears_on_burn` |
| `ip_allowlist.rs` | **IP White/Blacklist** : `test_localhost_always_allowed`, `test_empty_allowlist_permits_all`, `test_cidr_matching`, `test_single_ip_with_32_mask`, `test_ipv6_support`, `test_parse_config_values`, `test_invalid_config_values`, `test_public_endpoints_always_accessible`, `test_blocked_ip_simulation`, `test_attacker_ip_blocked_from_admin`, `test_admin_from_vpn_allowed` |
| `history_core.rs` | **Historique** : `history_core_filters_and_paginates` |
| `history_separation_test.rs` | **Hot/Cold Storage** : `history_separation_test` |
| `metadata_supply_test.rs` | **Metadata** : `test_metadata_persistence_and_supply` |
| `network_batching.rs` | **P2P Batching** : `test_network_batching_enqueue`, `test_network_batching_high_load`, `test_network_batching_empty_tick`, `test_network_batching_concurrent` |
| `dynamic_connection.rs` | **P2P Dyn** : `test_dynamic_p2p_connection` |

### 🟢 Docker & Tests Ignorés (Besoin Cluster)

| Fichier Test | Tests |
|---|---|
| `docker_e2e.rs` | `test_cluster_startup` |
| `docker_stress_sync.rs` | `test_sync_under_load` |
| `docker_scenario.rs` | `test_scenario_split_brain`, `test_scenario_reorg` |
| `fee_distribution_e2e_test.rs` | `test_fee_distribution_ratios` |
| `fee_treasury_test.rs` | `test_treasury_accumulation` |
| `block_rewards_test.rs` | `test_inflation_rewards` |
| `total_supply_test.rs` | `test_total_supply_invariant` |
| `verify_keys_debug.rs` | **Debug** : `verify_coordinator_keys_debug` |

---

## 👛 3. Portefeuille (`crates/pms-wallet`)

### 🔴 Cryptographie & Sécurité

| Fichier Test | Objectif & Tests |
|---|---|
| **`sign_verify.rs`** | **Signature** : `sign_verify_ok` |
| **`history_encrypted_reward.rs`** | **Confidentialité** : `test_history_encrypted_reward` |
| **`x25519_fallback.rs`** | **Fallback ECIES** : `x25519_sk_hex_fallback_without_mnemonic_is_stable`, `x25519_pk_matches_sk_derivation` |
| **`ecdsa_k256.rs`** | **K256** : `test_verify_with_invalid_pubkey_format`, `test_from_mnemonic_sign_verify` |

### 🟡 Logique Wallet

| Fichier Test | Objectif & Tests |
|---|---|
| `history_e2e.rs` | **Scan E2E** : `history_e2e_scan_decrypt_filter_by_address_rocks` |
| `history_pagination.rs` | **Pagination** : `history_pagination_by_time_and_id_rocks` |
| `history_test.rs` | **Filtres** : `decrypt_and_filter_by_address_rocks` |
| `tx_fees_and_change.rs` | **Calcul Tx** : `tx_build_with_fee_policy_happy_path`, `tx_build_insufficient_inputs` |
| `wallet_addr.rs` | **Adresses** : `address_roundtrip_and_lengths`, `bech32_hrp_variant_and_payload_len`, `short_address_is_prefix_of_full`, `tampered_address_fails_decode`, `from_word_list_reconstructs_same_wallet` |

---

## 🔐 4. Crypto & Bases (`crates/pms-crypto`, `pms-types`, `pms-config`)

### 🟡 Primitives

| Fichier Test | Module | Tests |
|---|---|---|
| `ed25519_roundtrip.rs` | `crypto` | `test_ed25519_sign_verify`, `test_ed25519_serialization` |
| `lib.rs` | `crypto` | `test_hash_functions`, `test_signature_wrappers` |
| `general.rs` | `token` | `test_amount_parsing`, `test_decimal_precision` |
| `general.rs` | `payload` | `test_x25519_encryption_decryption`, `test_payload_envelope_serialization` |
| `nft_tests.rs` | `nft` | `test_nft_structure_serialization`, `test_nft_id_generation` |

### 🟢 Configuration

| Fichier Test | Tests |
|---|---|
| `runtime.rs` | `test_apply_single_update`, `test_apply_batch_update` |
| `treasury_wallets.rs` | `test_sign_and_verify_treasury`, `test_verify_fails_with_wrong_wallets` |

---

## 🌐 5. Réseau & Stockage (`crates/pms-network`, `pms-storage`)

### 🟡 Réseau (P2P)

| Fichier Test | Objectif & Tests |
|---|---|
| `anti_abuse.rs` | **Protection** : `ping_pong_still_works`, `oversize_message_is_dropped_connection`, `too_many_parse_errors_kicks_peer`, `rate_limit_drops_or_closes_under_burst` |
| `validate_before_relay.rs` | **Relay** : `validate_before_relay_rocks` |
| `two_nodes_mvp.rs` | **Échange** : `two_nodes_share_blocks_debug_rocks` |
| `rate_limit_and_size.rs` | **Limites** : `rate_limit_and_size_rocks` |
| `bootstrap_tips_then_fetch_chain.rs` | **Sync** : `bootstrap_tips_then_fetch_chain_rocks` |

### 🟡 Stockage (RocksDB)

| Fichier Test | Objectif & Tests |
|---|---|
| `rocks_integrations.rs` | **CRUD** : `can_open_and_write_read`, `migrations_apply_and_version_is_current_rocks`, `put_get_block_and_index_rocks`, `children_counter_and_tips_basic_rocks`, `tips_respect_limit_with_trim_rocks`, `export_then_import_roundtrip_rocks`, `append_is_atomic_and_idempotent_rocks` |
| `rocks_checkpoints.rs` | **Sauvegardes** : `rocks_checkpoints_are_created_and_rotated` |
| `rocks_migration.rs` | **Migrations** : `migrations_apply_and_version_is_current` |
| `index_by_time.rs` | **Index Temps** : `index_by_time_respects_tip_limit_rocks` |
| `rocks_utxo.rs` | **UTXOs** : `apply_tx_atomic_ok_then_conflict_rocks` |
| `node_rewards_test.rs` | **Rewards DB** : `test_node_block_count_increment`, `test_fee_pool_accumulation`, `test_get_all_miners`, `test_reset_pool_and_counts`, `test_distribution_calculation`, `test_runtime_config_node_fee` |

---

## ⚡ 6. Événements & Outils (`crates/pms-event`, `tools-cli`, `pms-ledger`)

### 🟢 Events

| Fichier Test | Tests |
|---|---|
| `event_tests.rs` | `test_nft_event_types`, `test_event_serialization`, `test_bus_emit_and_receive`, `test_bus_multiple_subscribers`, `test_bus_clone_shares_channel` |

### 🟢 Ledger & CLI

| Fichier Test | Tests |
|---|---|
| `picks_test.rs` | **Coin Selection** : `fee_recipient_uniform_is_always_in_admin_list`, `fee_recipient_round_robin_cycles` |
| `keygen_tests.rs` | **CLI Keygen** : `test_generate_and_save_produces_64_char_hex_key` |
| `admin_picker_tests.rs` | **Admin Picker** : `empty_list_errors`, `uniform_balances_are_near_uniform`, `underfunded_gets_higher_prob`, `epsilon_prevents_zero_probability`, `pick_admin_address_weighted_matches_manual_sampling`, `pick_admin_recipient_returns_xpk_and_matches_wallet_encoding` |

---

_Inventaire généré automatiquement avec : grep, classification manuelle des criticités._
