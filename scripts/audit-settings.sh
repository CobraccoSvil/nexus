#!/usr/bin/env bash
# scripts/audit-settings.sh — Audit configurazioni: DB vs lettori codice vs UI.
#
# Wrapper di scripts/audit_settings.py (punto unico del censimento, regola L).
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

exec python3 scripts/audit_settings.py "$@"
