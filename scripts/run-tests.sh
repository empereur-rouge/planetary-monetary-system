#!/bin/bash
# =============================================================================
# run-tests.sh — Lance la suite de tests de façon FIABLE, crate par crate.
# =============================================================================
#
# POURQUOI par-crate et pas `cargo test --workspace` ?
#   Sur le FS externe (/Volumes/Crutial X9 ...) + parallélisme maximal, le
#   `--workspace` global a des flakes I/O transitoires : la lecture du fichier
#   de config est silencieusement droppée par config-rs (source required(false))
#   → `missing field rocks` → des binaires entiers tombent au hasard. En lançant
#   crate par crate, la contention I/O est bornée et le résultat est déterministe.
#
# CE SCRIPT EST LA RÉPONSE À « est-ce que la suite est verte ? ».
#   Le laisser pourrir (tests morts non-compilants, tests rouges pré-existants)
#   est ce qui a permis l'accumulation de rot découverte à l'audit v0.9.3.
#   Relancer en UNE commande :  bash scripts/run-tests.sh
#
# Options:
#   --release        compile/run en release (plus lent, masque moins de races)
#   --ignored        inclut aussi les tests #[ignore] (sandbox, bench, docker)
#   -p <crate>       ne teste qu'un crate (répétable)
#
# Sortie: un résumé PASS/FAIL par crate + code de sortie non-nul si un échec.
# =============================================================================
set -u

cd "$(dirname "$0")/.." || exit 2
REPO_ROOT="$(pwd)"

# Force une config explicite : évite le flake de découverte de fichier de config
# (cf. footgun load_config / FS externe documenté dans CLAUDE.md § Anti-faux-tests).
export PMS_CONFIG="${PMS_CONFIG:-$REPO_ROOT/etc/config/config.dev.toml}"

# Cible de build dédiée (ne pas polluer ./target, accélère les itérations).
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/dag-pms-target}"

PROFILE=""
EXTRA_TEST_ARGS=()
ONLY_CRATES=()
while [ $# -gt 0 ]; do
  case "$1" in
    --release) PROFILE="--release"; shift ;;
    --ignored) EXTRA_TEST_ARGS+=("--ignored"); shift ;;
    -p) ONLY_CRATES+=("$2"); shift 2 ;;
    *) echo "argument inconnu: $1"; exit 2 ;;
  esac
done

# Liste des crates : tous les membres du workspace + le simulateur (hors workspace).
if [ "${#ONLY_CRATES[@]}" -gt 0 ]; then
  CRATES=("${ONLY_CRATES[@]}")
else
  CRATES=()
  for d in crates/*/; do
    name="$(basename "$d")"
    CRATES+=("$name")
  done
  # tools/simulator (pms-simulator) n'est PAS dans le workspace → testé à part.
  CRATES+=("pms-simulator")
fi

PASS=()
FAIL=()

echo "════════════════════════════════════════════════════════════════"
echo " run-tests.sh — PMS_CONFIG=$PMS_CONFIG"
echo "  profil: ${PROFILE:-debug}   ignored: ${EXTRA_TEST_ARGS[*]:-non}"
echo "════════════════════════════════════════════════════════════════"

for crate in "${CRATES[@]}"; do
  printf '── %-22s ' "$crate"
  # bash 3.2 (macOS) : `"${arr[@]}"` sur tableau vide + `set -u` lève "unbound".
  # On ne passe `-- <args>` que s'il y a des args ignored/etc.
  if [ "$crate" = "pms-simulator" ]; then
    # Crate binaire hors workspace : on teste depuis son dossier.
    if [ "${#EXTRA_TEST_ARGS[@]}" -gt 0 ]; then
      out="$(cd tools/simulator && cargo test $PROFILE -- "${EXTRA_TEST_ARGS[@]}" 2>&1)"
    else
      out="$(cd tools/simulator && cargo test $PROFILE 2>&1)"
    fi
  else
    if [ "${#EXTRA_TEST_ARGS[@]}" -gt 0 ]; then
      out="$(cargo test $PROFILE -p "$crate" -- "${EXTRA_TEST_ARGS[@]}" 2>&1)"
    else
      out="$(cargo test $PROFILE -p "$crate" 2>&1)"
    fi
  fi
  rc=$?
  # Compte les résultats agrégés.
  passed="$(printf '%s\n' "$out" | grep -cE 'test result: ok')"
  failed_lines="$(printf '%s\n' "$out" | grep -E 'test result: FAILED')"
  if [ $rc -eq 0 ] && [ -z "$failed_lines" ]; then
    echo "✓ ($passed binaires ok)"
    PASS+=("$crate")
  else
    echo "✗ FAIL"
    printf '%s\n' "$out" | grep -E '^test .* FAILED$|error\[|could not compile|panicked at' | head -10 | sed 's/^/      /'
    FAIL+=("$crate")
  fi
done

echo "════════════════════════════════════════════════════════════════"
echo " RÉSUMÉ : ${#PASS[@]} crate(s) verts, ${#FAIL[@]} en échec"
if [ "${#FAIL[@]}" -gt 0 ]; then
  echo " ÉCHECS : ${FAIL[*]}"
  exit 1
fi
echo " ✅ Toute la suite (par-crate) est verte."
exit 0
