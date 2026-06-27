#!/usr/bin/env bash
# scripts/audit-settings.sh — Audit configurazioni: DB vs lettori codice vs UI.
#
# Wrapper del sottocomando `cargo xtask audit-settings` (punto unico del
# censimento, regola L; porting zero-Python di scripts/audit_settings.py).
# Classi: VIVA / MORTA / FANTASMA / INVISIBILE / RUNTIME-ONLY. Il gate usa una
# baseline ratchet (scripts/audit-settings-baseline.json): morte/fantasma/
# invisibili possono solo SCENDERE, come il gate jscpd (.dup-baseline.json).
#
# Uso:
#   bash scripts/audit-settings.sh --report     # tabella completa su stdout
#   bash scripts/audit-settings.sh --gate       # exit!=0 su regressioni
#   bash scripts/audit-settings.sh --no-db      # CI senza Postgres (A2 vs B)
#   bash scripts/audit-settings.sh --json FILE  # dump completo

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

# Usa il binario gia' compilato se presente (gate CI: `cargo check --workspace`
# lo costruisce prima), altrimenti compila al volo con cargo run. L'audit
# legge i path relativi a ROOT_DIR (cwd), quindi l'invocazione deve restare qui.
XTASK_BIN="$ROOT_DIR/target/debug/xtask"
if [ -x "$XTASK_BIN" ]; then
  exec "$XTASK_BIN" audit-settings "$@"
else
  exec cargo run --quiet -p xtask -- audit-settings "$@"
fi
