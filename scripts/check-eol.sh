#!/usr/bin/env bash
# scripts/check-eol.sh — un file dichiarato eol=lf e' materializzato LF?
#
# Perche' esiste:
#   .gitattributes dichiara 'eol=lf' per i file i cui BYTE contano: le
#   migrazioni SQL sono checksummate byte-per-byte in '_sqlx_migrations', e uno
#   stesso contenuto con fine-riga diversi ha un hash diverso. Ma l'attributo
#   governa cio' che git SCRIVE al checkout, non cio' che c'e' gia' sul disco:
#   un file materializzato prima che la regola esistesse resta CRLF per sempre,
#   perche' nessun 'git checkout' successivo lo ri-materializza se il contenuto
#   non e' cambiato.
#
#   Quel file e' invisibile a tutto il resto del repo. Con core.autocrlf=true
#   git normalizza il contenuto nel confronto, quindi 'git diff' non mostra
#   differenze; sul repo reale nemmeno 'git status' segnalava i due file
#   (misurato: entrambi i comandi rispondono vuoto). Il blob e' corretto,
#   l'albero risulta pulito, e a vedere la differenza e' solo chi legge i byte.
#   Nessun gate esistente la vedeva.
#
#   MISURATO il 05/08/2026 su D:\IDEAI: 2 file su 695 con l'attributo erano
#   CRLF sul disco (le migrazioni 117 e 118). Il database locale era stato
#   migrato da quel checkout, quindi il registro conservava l'hash dei byte
#   CRLF e RIFIUTAVA ogni albero conforme -- cioe' ogni worktree creato dopo
#   l'aggiunta di .gitattributes, e l'avvio di mcp-core da uno di essi. Era la
#   seconda occorrenza: la prima e' l'incidente del 2026-07-02 (migrazione
#   0500) che .gitattributes cita nella propria intestazione.
#
# Cosa NON e' questo gate: un controllo su cio' che si sta committando. Il
# difetto non e' nel commit -- l'indice e' gia' corretto -- ma nel working tree,
# e ci resta finche' qualcuno non lo ri-materializza. Per questo non ha glob e
# guarda l'albero intero: e' l'unico momento in cui qualcuno lo guarda.
set -euo pipefail

cd "$(dirname "$0")/.."

# QUALI file guardare. 'git ls-files --eol' deve APRIRE ogni file per dire cosa
# c'e' sul disco: sull'albero intero costa ~20s (misurato), troppo per stare in
# un pre-commit -- e un gate che costa troppo e' un gate che qualcuno disattiva.
# I candidati si restringono percio' ai pattern che .gitattributes dichiara
# eol=lf: 0,75s a macchina scarica (misurato, 690 file dei ~15000 dell'albero;
# sotto un gate concorrente sale a una decina di secondi, sempre ben sotto la
# scansione totale nelle stesse condizioni). I pattern si LEGGONO da li' invece di
# essere ricopiati qui, altrimenti il giorno in cui la regola coprisse un'altra
# estensione questo gate resterebbe verde senza guardarla (regola O).
_pattern_eol_lf() {
  grep -v '^[[:space:]]*#' .gitattributes 2>/dev/null \
    | grep 'eol=lf' \
    | awk '{print $1}'
}

# I pattern di un .gitattributes annidato sono relativi alla SUA directory:
# tradurli in pathspec sarebbe una seconda implementazione delle regole di git,
# che divergerebbe. Se ne compare uno, si paga la scansione intera invece di
# guardare meno di quanto si dichiara.
annidati="$(git ls-files -- '*/.gitattributes')"

if [ -n "$annidati" ]; then
  candidati=""
else
  candidati="$(_pattern_eol_lf)"
  if [ -z "$candidati" ]; then
    # Nessun file dichiara eol=lf: non c'e' contraddizione possibile. Senza
    # questo ramo il pathspec vuoto significherebbe "tutto", e il gate
    # sembrerebbe veloce solo perche' non ha guardato niente.
    exit 0
  fi
fi

# 'w/' e' cio' che c'e' sul DISCO, 'attr/' cio' che il repo dichiara. Si cercano
# le righe in cui i due si contraddicono. 'mixed' vale quanto 'crlf': anche un
# file a fine-riga misti ha byte diversi da quelli dichiarati.
# shellcheck disable=SC2086  # i pattern sono pathspec distinti, non una stringa
divergenti="$(git ls-files --eol -- $candidati | grep -E '[[:space:]]w/(crlf|mixed)[[:space:]].*eol=lf' || true)"

if [ -z "$divergenti" ]; then
  exit 0
fi

quanti="$(printf '%s\n' "$divergenti" | wc -l | tr -d ' ')"
echo "check-eol: $quanti file dichiarati 'eol=lf' sono materializzati CRLF sul disco." >&2
echo "" >&2
printf '%s\n' "$divergenti" | sed 's/^/  /' >&2
echo "" >&2
echo "Il contenuto e' giusto: 'git diff' non mostra differenze e l'albero puo'" >&2
echo "risultare pulito. A vedere i byte e' il migrator sqlx, che confronta il" >&2
echo "checksum di ogni migrazione applicata e rifiuta di avviare mcp-core." >&2
echo "" >&2
# Il rimedio CANCELLA il file e lo fa riscrivere a git. Sembra brutale e non lo
# e': il contenuto sta nel blob, che e' gia' quello giusto (e' l'unica ragione
# per cui il difetto e' invisibile). Le vie non distruttive NON funzionano qui,
# ed e' misurato, non temuto: 'git checkout-index -f' esce 0 senza scrivere
# nulla quando l'indice considera il file aggiornato -- cioe' sempre, in questo
# caso, visto che per git il file non e' modificato. Sul repo reale il mtime
# restava identico al millisecondo. Suggerire quel comando avrebbe consegnato un
# rimedio che dichiara successo e lascia il difetto dov'e'.
echo "Rimedio (ricrea i file dal blob, applicando .gitattributes):" >&2
printf '%s\n' "$divergenti" | cut -f2 | sed 's|^\(.*\)$|  rm "\1" \&\& git checkout -- "\1"|' >&2
echo "" >&2
echo "Se il database e' gia' stato migrato da questo checkout, il suo registro" >&2
echo "conserva gli hash CRLF e va riallineato DOPO:" >&2
echo "  cargo run -p xtask -- migrate --set meta --repair-checksums" >&2
exit 1
