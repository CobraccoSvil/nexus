#!/usr/bin/env bash
# Test end-to-end delle premesse dei gate DA UN WORKTREE, che e' il caso in cui i
# due difetti dell'08/08/2026 esistono e nel repo principale no.
#
# Perche' esiste come test e non come verifica una-tantum: entrambi i difetti
# erano invisibili da dove si guarda di solito. Nel repo principale il `.env` c'e'
# e `node_modules` pure, quindi ogni prova fatta li' e' verde per costruzione — e
# lo resterebbe anche dopo aver rotto di nuovo la risoluzione. La misura deve
# raggiungere l'oggetto per la stessa strada della produzione (regola O), e qui
# la strada e' un worktree collegato.
#
# Cosa attraversa: gli script REALI (`gate-env.sh`, `gate-premesse.sh`,
# `precommit-cargo-check.sh`, `precommit-turbo.sh`), copiati byte per byte in un
# repository usa-e-getta. Il repo e' temporaneo, non gli script: serve poter
# TOGLIERE il `.env`, che nel repo reale c'e' sempre e senza il quale il caso
# "manca ovunque" non e' osservabile.
#
# Test di mutazione (regola O), da rieseguire dopo ogni modifica agli script:
#
#   1) riporta la ricerca del .env al solo albero corrente:
#        in gate-env.sh, cancella il ramo che aggiunge "${_gate_common_root}/.env"
#      -> deve ROSSEGGIARE il caso 1 (era il difetto: worktree senza .env)
#
#   2) riporta il fail-closed all'exit 1 generico:
#        in gate-premesse.sh, `exit "$NEXUS_GATE_EXIT_CONFIG"` -> `exit 1`
#      -> deve ROSSEGGIARE i casi 3 e 5 (era il difetto: "non eseguito" e
#         "codice bocciato" indistinguibili, quindi fail_text bugiardo)
#
#   3) togli la pretesa su turbo:
#        in precommit-turbo.sh, cancella la riga `gate_pretende_turbo`
#      -> deve ROSSEGGIARE il caso 5
#
# Uso: bash scripts/gate-premesse-selftest.sh
# Exit 0 = tutti i casi superati.
set -uo pipefail

SCRIPTS="$(cd "$(dirname "$0")" && pwd)"
TMP="$(mktemp -d)"
COMUNE="$TMP/comune"
WT="$TMP/wt"
NOHOOKS="$TMP/nohooks"

cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

falliti=0
esito() { # esito <descrizione> <atteso> <ottenuto>
  if [ "$2" = "$3" ]; then
    printf '  OK       %s\n' "$1"
  else
    printf '  FALLITO  %s\n     atteso  : %s\n     ottenuto: %s\n' "$1" "$2" "$3"
    falliti=$((falliti + 1))
  fi
}

# Repository usa-e-getta. Hook e firma disattivati con -c sul singolo comando e
# non con --no-verify: gli hook che girerebbero qui sono quelli del repo REALE
# (core.hooksPath e' spesso globale), e farli partire dentro un test dei gate
# significherebbe misurare l'albero sbagliato — lo stesso difetto che si sta
# verificando.
mkdir -p "$NOHOOKS"
git init -q -b main "$COMUNE"
git -C "$COMUNE" config user.email selftest@nexus.local
git -C "$COMUNE" config user.name "gate selftest"
mkdir -p "$COMUNE/scripts"
for s in gate-env.sh gate-premesse.sh precommit-cargo-check.sh precommit-turbo.sh; do
  cp "$SCRIPTS/$s" "$COMUNE/scripts/$s"
done
git -C "$COMUNE" add -A
git -C "$COMUNE" -c core.hooksPath="$NOHOOKS" -c commit.gpgsign=false \
  commit -q -m "selftest: script dei gate"
git -C "$COMUNE" worktree add -q -b selftest-wt "$WT" main

echo "repo comune : $COMUNE"
echo "worktree    : $WT"
echo

# Sorge gate-env.sh dall'albero indicato, con l'ambiente ripulito da cio' che il
# test non vuole ereditare, e stampa cio' che il file ha risolto.
sorgi() { # sorgi <albero> <variabile>
  (cd "$1" && env -u DATABASE_URL -u NEXUS_GATE_ENV_FILE -u CARGO_TARGET_DIR \
    bash -c 'source scripts/gate-env.sh 2>/dev/null; echo "${'"$2"':-<vuota>}"')
}

# Esegue uno script di gate dall'albero indicato e stampa il solo codice d'uscita.
codice() { # codice <albero> <script>
  (cd "$1" && env -u DATABASE_URL bash "scripts/$2" >/dev/null 2>&1; echo $?)
}

echo "### 1. worktree senza .env: la fonte e' il repo COMUNE"
printf 'DATABASE_URL=postgres://prova/comune\n' > "$COMUNE/.env"
esito "DATABASE_URL risolta dal comune" \
  "postgres://prova/comune" "$(sorgi "$WT" DATABASE_URL)"
esito "il file letto e' dichiarato (premessa, regola O)" \
  "$COMUNE/.env" "$(sorgi "$WT" NEXUS_GATE_ENV_FILE)"

echo
echo "### 2. .env NEL worktree: decisione locale, vince sul comune"
printf 'DATABASE_URL=postgres://prova/locale\n' > "$WT/.env"
esito "DATABASE_URL risolta dal worktree" \
  "postgres://prova/locale" "$(sorgi "$WT" DATABASE_URL)"
rm -f "$WT/.env"

echo
echo "### 3. .env da nessuna parte: il gate NON e' fallito, non e' partito"
rm -f "$COMUNE/.env"
esito "DATABASE_URL non risolta" "<vuota>" "$(sorgi "$WT" DATABASE_URL)"
uscita="$(codice "$WT" precommit-cargo-check.sh)"
esito "precommit-cargo-check esce 78 (EX_CONFIG), non 1" "78" "$uscita"
messaggio="$(cd "$WT" && env -u DATABASE_URL bash scripts/precommit-cargo-check.sh 2>&1)"
case "$messaggio" in
  *"GATE NON ESEGUITO"*) trovato=si ;;
  *) trovato="no -- $messaggio" ;;
esac
esito "il messaggio dichiara che non ha misurato niente" "si" "$trovato"
case "$messaggio" in
  *"$COMUNE/.env"*) cercato=si ;;
  *) cercato="no -- il messaggio non dice dove ha cercato" ;;
esac
esito "il messaggio elenca i percorsi cercati, comune incluso" "si" "$cercato"

echo
echo "### 4. repo principale: il caso normale non cambia"
printf 'DATABASE_URL=postgres://prova/principale\n' > "$COMUNE/.env"
esito "DATABASE_URL dal proprio .env" \
  "postgres://prova/principale" "$(sorgi "$COMUNE" DATABASE_URL)"
esito "un solo candidato, nessun doppione" \
  "$COMUNE/.env" "$(sorgi "$COMUNE" NEXUS_GATE_ENV_CANDIDATI)"

echo
echo "### 5. albero senza node_modules: turbo non invocabile"
# Il repo usa-e-getta non ha node_modules: e' esattamente lo stato di un worktree
# appena creato, cioe' il caso 2 dell'08/08.
esito "precommit-turbo esce 78 (EX_CONFIG), non 1" \
  "78" "$(codice "$WT" precommit-turbo.sh)"
msg_turbo="$(cd "$WT" && bash scripts/precommit-turbo.sh 2>&1)"
case "$msg_turbo" in
  *"turbo non e' invocabile"*) causa=si ;;
  *) causa="no -- $msg_turbo" ;;
esac
esito "il messaggio nomina turbo, non typecheck/lint" "si" "$causa"

echo
echo "### 6. il target dir resta dell'albero, non del comune"
# Invariante che il fix non deve rompere: i .env si ereditano, gli artefatti no.
# Un target condiviso farebbe girare i test di un albero sul binario di un altro.
esito "CARGO_TARGET_DIR del worktree" \
  "$WT/target" "$(sorgi "$WT" CARGO_TARGET_DIR)"

echo
if [ "$falliti" -eq 0 ]; then
  echo "OK gate-premesse-selftest: tutti i casi superati."
else
  echo "FALLITO gate-premesse-selftest: $falliti caso/i non superato/i." >&2
fi
exit "$([ "$falliti" -eq 0 ] && echo 0 || echo 1)"
