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

# Riusa il binario gia' compilato da `cargo check --workspace` se presente,
# altrimenti compila al volo. La scansione legge path relativi a ROOT_DIR (cwd).
XTASK_BIN="$ROOT_DIR/target/debug/xtask"
if [ -x "$XTASK_BIN" ]; then
  exec "$XTASK_BIN" quality-scan "$@"
else
  exec cargo run --quiet -p xtask -- quality-scan "$@"
fi
