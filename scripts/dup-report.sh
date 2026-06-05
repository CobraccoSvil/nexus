#!/usr/bin/env bash
# scripts/dup-report.sh — Punto unico di misura della duplicazione del codice.
#
# Esegue jscpd su TS/JS/Rust/Python (config in jscpd.json) e applica il gate
# "ratchet": il numero di cloni puo' solo SCENDERE rispetto a .dup-baseline.json.
# Quando una wave riduce il debito, si riallinea la baseline al ribasso con
# --update-baseline. Vedi ADR 0026 e regola L del CLAUDE.md.
#
# Uso:
#   bash scripts/dup-report.sh                  # misura + gate ratchet vs baseline (CI)
#   bash scripts/dup-report.sh --update-baseline  # riscrive la baseline (mai al rialzo)
#   bash scripts/dup-report.sh --report-only    # solo misura, nessun confronto
#
# Parsing JSON via node (jq non e' garantito sull'ambiente; node si').

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

OUT_DIR="$ROOT_DIR/.dup-report"
REPORT_JSON="$OUT_DIR/jscpd-report.json"
BASELINE="$ROOT_DIR/.dup-baseline.json"

YELLOW="\033[0;33m"; GREEN="\033[0;32m"; RED="\033[0;31m"; NC="\033[0m"

MODE="check"
case "${1:-}" in
  --update-baseline) MODE="update" ;;
  --report-only)     MODE="report" ;;
  "")                MODE="check" ;;
  *) echo "Argomento sconosciuto: $1" >&2; exit 2 ;;
esac

echo -e "${YELLOW}==> dup-report: esecuzione jscpd${NC}"
# Passa i path da scansionare ESPLICITAMENTE: il campo "path" del jscpd.json
# non e' rispettato come filtro restrittivo (jscpd scansiona tutto da cwd se
# non si passano path CLI). Vedi jscpd.json per i path canonici, mantenere
# allineati. Gli "ignore" del config valgono comunque entro questi root.
pnpm exec jscpd --silent --reporters json --output "$OUT_DIR" \
    apps packages crates brain || true

if [[ ! -f "$REPORT_JSON" ]]; then
  echo -e "${RED}!! dup-report: report jscpd non generato ($REPORT_JSON)${NC}" >&2
  exit 1
fi

read_total() {
  node -e "const r=require('$REPORT_JSON').statistics.total; process.stdout.write(String(r['$1']))"
}
PCT="$(read_total percentage)"
CLONES="$(read_total clones)"
DUP_LINES="$(read_total duplicatedLines)"
echo -e "${YELLOW}==> dup-report: percentuale=${PCT}% cloni=${CLONES} righe_duplicate=${DUP_LINES}${NC}"

if [[ "$MODE" == "update" ]]; then
  node -e "const r=require('$REPORT_JSON').statistics.total; \
    require('fs').writeFileSync('$BASELINE', JSON.stringify({percentage:r.percentage, clones:r.clones, duplicatedLines:r.duplicatedLines}, null, 2)+'\n')"
  echo -e "${GREEN}==> dup-report: baseline aggiornata -> .dup-baseline.json${NC}"
  exit 0
fi

if [[ "$MODE" == "report" ]]; then
  exit 0
fi

# MODE=check: gate ratchet (i cloni non possono aumentare).
if [[ ! -f "$BASELINE" ]]; then
  echo -e "${RED}!! dup-report: baseline assente; generala con: bash scripts/dup-report.sh --update-baseline${NC}" >&2
  exit 1
fi

if node -e "const cur=require('$REPORT_JSON').statistics.total; const base=require('$BASELINE'); \
  if (cur.clones > base.clones) { \
    console.error('cloni '+cur.clones+' > baseline '+base.clones); process.exit(1);} \
  process.exit(0);"; then
  echo -e "${GREEN}OK dup-report: duplicazione non aumentata (cloni ${CLONES} <= baseline)${NC}"
else
  echo -e "${RED}!! dup-report: duplicazione AUMENTATA. Consolidare nel punto unico prima del merge (regola L / ADR 0026).${NC}" >&2
  exit 1
fi
