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

# Ambiente comune dei gate (target dir, incrementale, DATABASE_URL): punto
# unico. Serve perche' la riga sotto invoca cargo, e un gate che compilasse in
# un target diverso dagli altri non riuserebbe nulla.
# shellcheck source=scripts/gate-env.sh
source "$ROOT_DIR/scripts/gate-env.sh"

# SEMPRE via cargo, come scripts/quality-scan.sh (regola O: lo strumento misura
# il codice CORRENTE).
#
# Fino al 2026-08-05 qui c'era uno shortcut su `$ROOT_DIR/target/debug/xtask`,
# con due difetti:
#   1. il path era hardcoded e ignorava CARGO_TARGET_DIR — con un target diverso
#      l'audit girava con un binario STANTIO, misurando con la logica vecchia
#      contro la baseline nuova (lo stesso difetto che quality-scan.sh dichiara
#      di aver gia' corretto: "il gate mentiva in entrambe le direzioni");
#   2. la motivazione era falsa: diceva che `cargo check --workspace` costruisce
#      quel binario, ma `cargo check` non linka — non lo ha mai prodotto.
# A target caldo cargo costa ~1s e garantisce la freschezza da se'.
#
# L'audit legge i path relativi a ROOT_DIR (cwd), quindi l'invocazione resta qui.
exec cargo run --quiet -p xtask -- audit-settings "$@"
