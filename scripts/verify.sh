#!/usr/bin/env bash
# scripts/verify.sh — Gate di build verification unificato per il monorepo.
#
# Esegue, nell'ordine:
#   1. Lint, typecheck e test TypeScript via turbo
#   2. cargo check workspace
#   3. cargo clippy workspace con -D warnings
#   4. cargo test workspace (no fail-fast)
#
# Utilizzato sia dall'hook pre-commit (lefthook) sia dalla CI.
# Uscita non-zero se qualunque fase fallisce. Stampa il nome della fase fallita.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

YELLOW="\033[0;33m"
GREEN="\033[0;32m"
RED="\033[0;31m"
NC="\033[0m"

run_phase() {
    local name="$1"
    shift
    echo -e "${YELLOW}==> verify: ${name}${NC}"
    if ! "$@"; then
        echo -e "${RED}!! verify: fase '${name}' FALLITA${NC}" >&2
        exit 1
    fi
}

SKIP_RUST="${VERIFY_SKIP_RUST:-0}"
SKIP_TS="${VERIFY_SKIP_TS:-0}"

if [[ "$SKIP_TS" != "1" ]]; then
    run_phase "turbo typecheck+lint+test" pnpm exec turbo run typecheck lint test --continue
else
    echo "-- verify: TS saltato (VERIFY_SKIP_TS=1)"
fi

if [[ "$SKIP_RUST" != "1" ]]; then
    run_phase "cargo check --workspace" cargo check --workspace
    run_phase "cargo clippy --workspace --all-targets -- -D warnings" \
        cargo clippy --workspace --all-targets -- -D warnings
    run_phase "cargo test --workspace --no-fail-fast" cargo test --workspace --no-fail-fast
else
    echo "-- verify: Rust saltato (VERIFY_SKIP_RUST=1)"
fi

# Gate ratchet configurazioni: settings morte/fantasma/invisibili possono solo
# scendere (baseline in scripts/audit-settings-baseline.json). Se il DB live
# non e' raggiungibile lo script degrada da solo in modalita' --no-db.
run_phase "audit settings (gate ratchet)" bash scripts/audit-settings.sh --gate

echo -e "${GREEN}OK verify: tutte le fasi passate${NC}"
