use pms_core::{Dag, PARENTS_MAX, PARENTS_MIN}; // adapte aux noms exacts
use pms_types::{Block, EncryptedPayload, PayloadEnvelope, PlainPayload, TxOutput};
use pms_utils::compute_block_id;
use rand::RngCore;
use x25519_dalek::{PublicKey, StaticSecret};

#[test]
fn stress_create_1000_blocks_and_check_parents() {
    let genesis = Block::genesis(pms_utils::compute_block_id_sorted);
    let mut dag = Dag::new_with_genesis(genesis);

    let initial_count = dag.blocks.len();

    for _i in 0..1000 {
        let payload = Some(PayloadEnvelope::Plain(PlainPayload::Mint {
            outputs: vec![TxOutput {
                address: "8xtest".into(),
                amount: "10.00000000".into(),
            }],
        }));
        let _b = dag
            .add_payload_auto_parents_mined(
                payload,
                0, // pas de PoW pour les tests
                pms_utils::compute_block_id_sorted,
            )
            .unwrap();

        //println!("Itération {}, parents={:?}", i, &_b.parents);
    }

    // total
    let expected = initial_count + 1000;
    assert_eq!(
        dag.blocks.len(),
        expected,
        "len={} expected={}",
        dag.blocks.len(),
        expected
    );

    // Comptages par cardinalité de parents (hors genesis)
    let mut count_parents_eq1 = 0usize;
    let mut count_viol_min = 0usize;
    let mut count_viol_max = 0usize;

    for (_id, b) in &dag.blocks {
        if b.parents.is_empty() {
            continue;
        } // genesis
        let n = b.parents.len();
        if n == 1 {
            count_parents_eq1 += 1;
        } else {
            if n < PARENTS_MIN {
                count_viol_min += 1;
            }
            if n > PARENTS_MAX {
                count_viol_max += 1;
            }
        }
        // Tous les parents existent
        for p in &b.parents {
            assert!(dag.blocks.contains_key(p), "parent {p} manquant");
        }
    }

    // On autorise au plus UN bloc avec 1 parent (le tout premier après genesis)
    assert!(
        count_parents_eq1 <= 1,
        "trop de blocs à 1 parent: {count_parents_eq1}"
    );
    // Tous les autres respectent min/max
    assert_eq!(count_viol_min, 0, "des blocs ont < PARENTS_MIN parents");
    assert_eq!(count_viol_max, 0, "des blocs ont > PARENTS_MAX parents");

    // Au moins un tip restant
    assert!(!dag.find_tips().is_empty(), "aucun tip restant");
}

fn gen_keypair_hex() -> (String, String) {
    let mut sk_bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut sk_bytes);
    let sk = StaticSecret::from(sk_bytes);
    let pk = PublicKey::from(&sk);
    (hex::encode(sk.to_bytes()), hex::encode(pk.to_bytes()))
}

#[test]
fn stress_create_100_encrypted_blocks_and_check_parents() {
    // --- clés de lecture pour le test (1 lecteur)
    let (reader_sk_hex, reader_pk_hex) = gen_keypair_hex();

    // 1) Genesis
    let genesis = Block::genesis(compute_block_id);
    let mut dag = Dag::new_with_genesis(genesis);
    let initial_count = dag.blocks.len();

    // 2) 100 blocks Mint (CHIFFRÉS)
    for i in 0..100 {
        // a) payload clair (objet applicatif)
        let mint_block = PlainPayload::Mint {
            outputs: vec![TxOutput {
                address: "8xtestaddr".to_string(),
                amount: "10.00000000".to_string(),
            }],
        };
        let pt = serde_json::to_vec(&mint_block).expect("serde mint_block");

        // b) enveloppe chiffrée pour le lecteur
        let enc = EncryptedPayload::encrypt_for(&pt, &vec![reader_pk_hex.clone()], pt.len() as u32)
            .expect("encrypt_for");

        // 👉 Affiche le JSON de l’enveloppe chiffrée
        println!(
            "Itération {i}, EncryptedPayload = {}",
            serde_json::to_string_pretty(&enc).unwrap()
        );

        let payload = Some(PayloadEnvelope::Encrypted(enc));

        // c) ajout bloc avec parents auto
        let block = dag
            .add_payload_auto_parents_mined(payload, 1, compute_block_id)
            .expect("add_payload_auto_parents");

        // d) vérif parents existent
        for pid in &block.parents {
            assert!(
                dag.blocks.contains_key(pid),
                "Parent {} manquant à l'itération {}",
                pid,
                i
            );
        }

        // (optionnel) e) spot-check : on essaie de déchiffrer 1 bloc / N
        if i % 20 == 0 {
            // 👈 j’ai mis 20 au lieu de 200 pour en voir plus
            if let Some(PayloadEnvelope::Encrypted(ep)) = &block.payload {
                let recovered = ep.decrypt_with(&reader_sk_hex).expect("decrypt");
                let back: PlainPayload = serde_json::from_slice(&recovered).expect("serde back");
                println!("Itération {i}, Payload déchiffré = {:?}", back);
                match back {
                    PlainPayload::Mint { .. } => {}
                    _ => panic!("payload inattendu après decrypt"),
                }
            }
        }
    }

    // 4) 100 nouveaux blocks (en plus de genesis)
    assert_eq!(dag.blocks.len(), initial_count + 100);

    // 5) Chaque bloc (hors genesis) a le bon nombre de parents
    for (_id, b) in &dag.blocks {
        if b.parents.is_empty() {
            continue;
        } // genesis
        let min_parents = if initial_count <= 1 { 1 } else { 2 };
        assert!(
            b.parents.len() >= min_parents,
            "block {_id} parents={}",
            b.parents.len()
        );
    }

    // 6) Tous les parents existent
    for (_id, b) in &dag.blocks {
        for p in &b.parents {
            assert!(dag.blocks.contains_key(p), "parent {p} manquant pour {_id}");
        }
    }
}
