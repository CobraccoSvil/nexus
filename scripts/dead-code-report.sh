#!/usr/bin/env bash
# scripts/dead-code-report.sh — Punto unico di misura del dead code (regola L).
#
# Due rilevatori + gate "ratchet" (i conteggi possono solo SCENDERE rispetto a
# .dead-code-baseline.json, come il gate jscpd di dup-report.sh):
#   - Rust:   warning dead_code del compilatore (su CARGO_TARGET_DIR separata
#             con RUSTFLAGS="" perche' .cargo/config.toml potrebbe cappare i
#             lint) + dipendenze inutilizzate via cargo-machete (se installato)
#   - TS:     knip su apps/web-ide (file + export inutilizzati)
# La fase Python (vulture su brain/) e' stata rimossa col porting del brain
# in Rust (commit 75a6d62): brain/ non esiste piu' nel repo.
#
# Uso:
#   bash scripts/dead-code-report.sh                    # misura + gate ratchet
#   bash scripts/dead-code-report.sh --update-baseline  # riallinea (mai al rialzo)
#   bash scripts/dead-code-report.sh --report-only      # solo misura
#
# NON e' nel pre-commit (troppo lento): on-demand + CI.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

BASELINE="$ROOT_DIR/.dead-code-baseline.json"
YELLOW="\033[0;33m"; GREEN="\033[0;32m"; RED="\033[0;31m"; NC="\033[0m"

MODE="check"
case "${1:-}" in
  --update-baseline) MODE="update" ;;
  --report-only)     MODE="report" ;;
  "")                MODE="check" ;;
  *) echo "Argomento sconosciuto: $1" >&2; exit 2 ;;
esac

echo -e "${YELLOW}==> dead-code: Rust (compilatore, lint non cappati)${NC}"
RUST_WARN=$(RUSTFLAGS="" CARGO_TARGET_DIR="${DEAD_CODE_TARGET_DIR:-/tmp/nexus-nocap}" \
    cargo check --workspace --message-format=short 2>&1 \
    | grep -cE "warning: .*never (used|read|constructed)" || true)

RUST_DEPS=0
if command -v cargo-machete >/dev/null 2>&1 || [ -x "$HOME/.cargo/bin/cargo-machete" ]; then
    RUST_DEPS=$(PATH="$HOME/.cargo/bin:$PATH" cargo machete 2>/dev/null \
        | grep -cE "^\s+\S+$" || true)
else
    echo "-- cargo-machete non installato: conteggio deps saltato"
fi

echo -e "${YELLOW}==> dead-code: TypeScript (knip su web-ide)${NC}"
TS_ISSUES=$(pnpm dlx knip --directory apps/web-ide --reporter compact --no-progress 2>/dev/null \
    | grep -vcE "^(Unused|Unlisted|$)" || true)

echo -e "${YELLOW}==> dead-code: rust_warnings=${RUST_WARN} rust_deps=${RUST_DEPS} ts_issues=${TS_ISSUES}${NC}"

CURRENT_JSON=$(printf '{"rust_warnings": %d, "rust_deps": %d, "ts_issues": %d}\n' \
    "$RUST_WARN" "$RUST_DEPS" "$TS_ISSUES")

if [[ "$MODE" == "update" ]]; then
    echo "$CURRENT_JSON" > "$BASELINE"
    echo -e "${GREEN}==> dead-code: baseline aggiornata -> .dead-code-baseline.json${NC}"
    exit 0
fi

if [[ "$MODE" == "report" ]]; then
    exit 0
fi

if [[ ! -f "$BASELINE" ]]; then
    echo -e "${RED}!! dead-code: baseline assente; generala con --update-baseline${NC}" >&2
    exit 1
fi

echo "$CURRENT_JSON" | node -e "
const cur = JSON.parse(require('fs').readFileSync(0, 'utf8'));
const base = require('./.dead-code-baseline.json');
const regress = Object.keys(cur).filter(k => cur[k] > (base[k] ?? 0));
if (regress.length) {
  console.error('dead-code AUMENTATO: ' + regress.map(k => k + ' ' + cur[k] + '>' + base[k]).join(', '));
  process.exit(1);
}
console.log('OK dead-code: nessuna regressione vs baseline');
"
