#!/usr/bin/env bash
# scripts/quality-scan.sh — Gate ratchet qualita codice Rust del workspace.
#
# Wrapper del sottocomando `cargo xtask quality-scan` (punto unico, regola L;
# basato su mcp_quality::scan). Metriche sottoposte a gate: findings totali,
# funzioni >50 righe, complessita ciclomatica >20, security. Baseline ratchet
# in scripts/quality-baseline.json: i valori possono solo SCENDERE, come il
# gate jscpd (.dup-baseline.json) e audit-settings.
#
# Uso:
#   bash scripts/quality-scan.sh --gate     # exit!=0 su regressioni (default)
#   bash scripts/quality-scan.sh --update   # riallinea la baseline al ribasso

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

# Ambiente comune dei gate (CARGO_INCREMENTAL=0 e simili): punto unico.
# shellcheck source=scripts/gate-env.sh
source "$ROOT_DIR/scripts/gate-env.sh"

# SEMPRE via cargo (regola O: lo strumento misura il codice CORRENTE).
# Il vecchio shortcut `target/debug/xtask` riusava un binario possibilmente
# STANTIO: dopo una modifica ai detector (mcp-quality) misurava con la logica
# vecchia contro la baseline nuova -- il gate mentiva in entrambe le direzioni.
# cargo garantisce la freschezza da se' (a target caldo il costo e' ~1s) e
# rispetta CARGO_TARGET_DIR (ogni albero il suo target, mai cache incrociate).
exec cargo run --quiet -p xtask -- quality-scan "$@"
