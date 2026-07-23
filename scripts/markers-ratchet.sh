#!/usr/bin/env bash
# markers-ratchet.sh — gate ratchet sui marker di debito e sulle frasi di inerzia.
#
# Perche' esiste (W9 del piano di pulizia, regola O + H). W8 ha corretto i 35
# commenti fossili PRINCIPALI (quelli che dichiaravano MORTO codice VIVO), ma il
# codebase conserva molti riferimenti al porting Python->Rust: alcuni sono tracce
# d'origine legittime, altri sono fossili non ancora ripuliti. Distinguerli
# testualmente, caso per caso, e' impossibile senza falsi positivi. Il ratchet
# non giudica il singolo commento: conta due famiglie di marker e impone che il
# totale possa solo SCENDERE. Cosi' nessun commit ne aggiunge di netti, e il
# numero cala man mano che le wave successive puliscono.
#
# Due metriche, ognuna col suo tetto:
#   debt    = righe con un marker di debito esplicito
#             (TODO, FIXME, HACK, XXX, WORKAROUND, DEBITO)
#   inertia = righe con una dichiarazione di INERZIA — le trappole di lettura che
#             inducono (umano o agente) a credere morto un percorso vivo:
#             INERTE, "mai raggiunt*", "non ancora (cablat|portat|instradat)*"
#
# Il conteggio e' quello ESATTO che genera la baseline (`--update`): lo strumento
# misura il suo oggetto come lo misurera' il gate (regola O). Path relativi alla
# repo root, mai assoluti (difetto gia' occorso col gate quality-scan).
#
# Uso:
#   scripts/markers-ratchet.sh            # gate: fallisce se una metrica sale
#   scripts/markers-ratchet.sh --update   # riscrive la baseline al valore corrente
#
# Innestato in lefthook.yml (pre-commit) e .github/workflows/verify.yml (CI).

set -euo pipefail

cd "$(dirname "$0")/.."

BASELINE="scripts/markers-baseline.json"

# Scope: codice sorgente di produzione e non. Le directory generate/vendored sono
# escluse; i file di test NON sono esclusi (un ratchet tollera i marker esistenti
# nella baseline, e un marker nuovo in un test e' comunque debito da giustificare).
SCOPE_ARGS=(
  crates apps
  --include='*.rs' --include='*.ts' --include='*.tsx'
  --exclude-dir=node_modules --exclude-dir=.next
  --exclude-dir=dist --exclude-dir=target --exclude-dir=target-verify
)

DEBT_RE='\b(TODO|FIXME|HACK|XXX|WORKAROUND|DEBITO)\b'
# Inerzia: le frasi che dichiarano non-esecuzione. `INERTE` e' maiuscolo di
# proposito (convenzione dei commenti di porting); le altre due sono
# case-insensitive perche' compaiono in prosa.
INERTIA_RE='INERTE|[Mm]ai raggiunt|[Nn]on ancora (cablat|portat|instradat)'

count() {
  # Conta le RIGHE che matchano (non le occorrenze): stabile fra Windows e Linux,
  # indipendente da CRLF. 2>/dev/null per ignorare eventuali path non leggibili.
  grep -rnE "$1" "${SCOPE_ARGS[@]}" 2>/dev/null | wc -l | tr -d '[:space:]'
}

debt="$(count "$DEBT_RE")"
inertia="$(count "$INERTIA_RE")"

if [[ "${1:-}" == "--update" ]]; then
  cat > "$BASELINE" <<JSON
{
  "debt": $debt,
  "inertia": $inertia
}
JSON
  echo "markers-ratchet: baseline aggiornata -> debt=$debt inertia=$inertia"
  exit 0
fi

if [[ ! -f "$BASELINE" ]]; then
  echo "!! markers-ratchet: baseline assente ($BASELINE). Esegui: scripts/markers-ratchet.sh --update" >&2
  exit 1
fi

# Legge i due interi dalla baseline senza dipendere da jq (non garantito in CI).
base_debt="$(grep -oE '"debt"[[:space:]]*:[[:space:]]*[0-9]+' "$BASELINE" | grep -oE '[0-9]+$')"
base_inertia="$(grep -oE '"inertia"[[:space:]]*:[[:space:]]*[0-9]+' "$BASELINE" | grep -oE '[0-9]+$')"

fail=0
report() {
  local name="$1" cur="$2" base="$3"
  if (( cur > base )); then
    echo "!! markers-ratchet: $name PEGGIORA: $base -> $cur (+$(( cur - base )))" >&2
    fail=1
  elif (( cur < base )); then
    echo "markers-ratchet: $name migliora: $base -> $cur (riallinea con --update)"
  else
    echo "OK markers-ratchet: $name invariato ($cur)"
  fi
}

report "debt"    "$debt"    "$base_debt"
report "inertia" "$inertia" "$base_inertia"

if (( fail != 0 )); then
  echo "" >&2
  echo "Un marker di debito o una frase di inerzia in piu' rispetto alla baseline." >&2
  echo "Rimuovi il marker/la frase introdotta, oppure — se il debito e' giustificato" >&2
  echo "e tracciato altrove — aggiorna la baseline con scripts/markers-ratchet.sh --update" >&2
  echo "dichiarandolo nel commit. La baseline NON deve mai salire senza motivazione." >&2
  exit 1
fi

echo "OK markers-ratchet: nessuna regressione (debt=$debt, inertia=$inertia)."
