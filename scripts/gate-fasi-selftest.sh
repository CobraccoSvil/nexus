#!/usr/bin/env bash
# Test della meccanica delle fasi di un gate (scripts/gate-fasi.sh).
#
# Perche' esiste: la proprieta' da verificare e' "un gate interrotto DICHIARA
# cio' che non ha eseguito", e provarla facendo girare `pnpm verify` vero
# richiederebbe venti minuti e una fase rotta apposta. Qui si sorge lo script
# REALE e si eseguono fasi finte (`true`, `false`): la meccanica attraversata e'
# quella della produzione, l'unica cosa sostituita e' cio' che le fasi fanno
# (regola O — la misura raggiunge il suo oggetto per la strada della
# produzione).
#
# Test di mutazione (regola O), da rieseguire dopo ogni modifica a gate-fasi.sh:
#
#   1) togli la registrazione delle fasi saltate:
#        in esegui_piano, sostituisci `FASI_NON_ESEGUITE+=("$nome")` con `:`
#      -> devono ROSSEGGIARE i casi 2 e 3 (era il difetto: una fase non
#         eseguita non lasciava traccia, e il log di un gate rosso non diceva
#         quanta parte del gate non aveva misurato niente)
#
#   2) riporta il fail-fast a incondizionato:
#        in gate-fasi.sh, `FAIL_FAST="${VERIFY_FAIL_FAST:-1}"` in entrambi i rami
#      -> deve ROSSEGGIARE il caso 4 (era il difetto: in CI un rosso TypeScript
#         nascondeva lo stato di Rust)
#
#   3) fai concludere il gate verde quando una fase e' rossa:
#        in piano_ha_fasi_rosse, `return 1` incondizionato
#      -> deve ROSSEGGIARE il caso 5
#
# Uso: bash scripts/gate-fasi-selftest.sh
# Exit 0 = tutti i casi superati.
set -uo pipefail

SCRIPTS="$(cd "$(dirname "$0")" && pwd)"

falliti=0
esito() { # esito <descrizione> <atteso> <ottenuto>
  if [ "$2" = "$3" ]; then
    printf '  OK       %s\n' "$1"
  else
    printf '  FALLITO  %s\n     atteso  : %s\n     ottenuto: %s\n' "$1" "$2" "$3"
    falliti=$((falliti + 1))
  fi
}

# Esegue un frammento sorgendo lo script reale, in un processo pulito.
# L'ambiente e' ripulito da CI e VERIFY_FAIL_FAST: il caso li dichiara da se',
# altrimenti il risultato dipenderebbe da dove gira il selftest (in CI, dove
# questo script gira dentro pnpm verify, CI e' impostata).
fasi() { # fasi <assegnazioni env> <frammento>
  env -u CI -u VERIFY_FAIL_FAST bash -c "
    set -uo pipefail
    $1
    source '$SCRIPTS/gate-fasi.sh'
    $2
  " 2>&1
}

echo "### 1. tutte verdi: nessuna fallita, nessuna non eseguita"
out="$(fasi '' '
  aggiungi_fase prima true
  aggiungi_fase seconda true
  esegui_piano
  echo "eseguite=${#FASI_NOMI[@]} nonEseguite=${#FASI_NON_ESEGUITE[@]}"
  piano_ha_fasi_rosse && echo "rosse=si" || echo "rosse=no"
' | tail -2)"
esito "due fasi eseguite, zero saltate" "eseguite=2 nonEseguite=0" "$(echo "$out" | head -1)"
esito "il piano non ha fasi rosse" "rosse=no" "$(echo "$out" | tail -1)"

echo
echo "### 2. fail-fast: le fasi dopo la rossa sono NON ESEGUITE, non verdi"
out="$(fasi '' '
  aggiungi_fase prima true
  aggiungi_fase rotta false
  aggiungi_fase terza true
  aggiungi_fase quarta true
  esegui_piano
  echo "eseguite=${#FASI_NOMI[@]} nonEseguite=${#FASI_NON_ESEGUITE[@]}"
  echo "saltate=${FASI_NON_ESEGUITE[*]}"
' | tail -2)"
esito "si ferma alla rossa: 2 eseguite, 2 saltate" \
  "eseguite=2 nonEseguite=2" "$(echo "$out" | head -1)"
esito "le saltate sono nominate, non contate e basta" \
  "saltate=terza quarta" "$(echo "$out" | tail -1)"

echo
echo "### 3. il riepilogo DICHIARA le non eseguite (non le omette)"
out="$(fasi '' '
  aggiungi_fase prima true
  aggiungi_fase rotta false
  aggiungi_fase terza true
  esegui_piano
  riepilogo_fasi
')"
case "$out" in
  *"NON ESEGUITE (1)"*) dichiarate=si ;;
  *) dichiarate="no -- il riepilogo non nomina le fasi saltate" ;;
esac
esito "il riepilogo elenca le fasi non eseguite" "si" "$dichiarate"
case "$out" in
  *"IGNOTO, non verde"*) ignoto=si ;;
  *) ignoto="no -- il riepilogo non dice che il loro stato e' ignoto" ;;
esac
esito "dice che il loro stato e' ignoto, non verde" "si" "$ignoto"
case "$out" in
  *"FALLITE (1)"*) fallite=si ;;
  *) fallite="no -- il riepilogo non elenca le fasi fallite" ;;
esac
esito "elenca anche le fasi fallite" "si" "$fallite"

echo
echo "### 4. in CI il gate prosegue: dice QUANTE cose sono rotte"
# Il difetto che questo caso presidia: dal 2026-07-03 la CI moriva sempre nella
# prima fase, e clippy/nextest non venivano eseguiti ne' riportati.
out="$(fasi 'export CI=true' '
  aggiungi_fase prima false
  aggiungi_fase seconda false
  aggiungi_fase terza true
  esegui_piano
  echo "eseguite=${#FASI_NOMI[@]} nonEseguite=${#FASI_NON_ESEGUITE[@]}"
' | tail -1)"
esito "CI impostata: nessuna fase resta al buio" "eseguite=3 nonEseguite=0" "$out"

out="$(fasi 'export CI=true; export VERIFY_FAIL_FAST=1' '
  aggiungi_fase prima false
  aggiungi_fase seconda true
  esegui_piano
  echo "eseguite=${#FASI_NOMI[@]} nonEseguite=${#FASI_NON_ESEGUITE[@]}"
' | tail -1)"
esito "l'override esplicito vince sul discriminante CI" \
  "eseguite=1 nonEseguite=1" "$out"

echo
echo "### 5. l'esito del piano e' rosso se una fase eseguita e' rossa"
out="$(fasi 'export CI=true' '
  aggiungi_fase prima true
  aggiungi_fase rotta false
  esegui_piano
  piano_ha_fasi_rosse && echo "rosse=si" || echo "rosse=no"
' | tail -1)"
esito "una fase rossa fra le eseguite -> piano rosso" "rosse=si" "$out"

out="$(fasi '' '
  esegui_piano
  piano_ha_fasi_rosse && echo "rosse=si" || echo "rosse=no"
' | tail -1)"
esito "piano vuoto: non e' rosso (e non esplode sotto set -u)" "rosse=no" "$out"

echo
echo "### 6. un argomento con spazi resta UN argomento"
# Il piano serializza le fasi in una stringa: senza un separatore che non
# compaia negli argomenti, un comando con uno spazio si spezzerebbe in due.
out="$(fasi '' '
  aggiungi_fase "eco" bash -c "printf %s \"\$0\"" "due parole"
  esegui_piano
' | grep -c "due parole")"
esito "l'argomento arriva intero al comando" "1" "$out"

echo
if [ "$falliti" -eq 0 ]; then
  echo "OK gate-fasi-selftest: tutti i casi superati."
else
  echo "FALLITO gate-fasi-selftest: $falliti caso/i non superato/i." >&2
fi
exit "$([ "$falliti" -eq 0 ] && echo 0 || echo 1)"
