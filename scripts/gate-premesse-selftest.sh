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
#   4) riporta gate_pretende_node a un confronto inventato:
#        in gate-premesse.sh, sostituisci il corpo con `return 0`
#      -> deve ROSSEGGIARE il caso 7 (era il difetto: un Node insufficiente
#         usciva come "Could not find lib/**/*.test.ts", cioe' accusava un file)
#
#   5) togli l'isolamento dall'ambiente git dell'hook:
#        in questo file, cancella la riga `unset GIT_DIR ...` in testa
#      -> deve ROSSEGGIARE il caso 8 (era il difetto: eseguito come hook
#         pre-commit, questo script committava sul repo REALE e vi registrava
#         un worktree, perche' GIT_DIR batte `git -C`)
#
# Uso: bash scripts/gate-premesse-selftest.sh
# Exit 0 = tutti i casi superati.
set -uo pipefail

# L'AMBIENTE GIT EREDITATO VA VIA PRIMA DI QUALUNQUE COMANDO GIT.
#
# Questo script crea un repository usa-e-getta e ci lavora con `git -C <tmp>`.
# `-C` cambia la directory, ma NON batte le variabili d'ambiente: se GIT_DIR e
# GIT_INDEX_FILE sono impostate, ogni comando git le usa e lavora sul repo che
# le ha esportate — dovunque punti `-C`.
#
# MISURATO l'09/08/2026, la prima volta che questo selftest e' stato agganciato
# a lefthook: durante un `git commit`, git esporta GIT_DIR e GIT_INDEX_FILE agli
# hook. Il `git -C "$COMUNE" commit -m "selftest: script dei gate"` di riga ~90
# ha percio' committato l'INDICE REALE sul branch reale — un commit col
# contenuto giusto e il messaggio del selftest — e il `worktree add ... main`
# successivo ha registrato nel repo reale un worktree sotto /tmp, poi cancellato
# da `cleanup`, lasciando un worktree orfano e l'indice pieno del contenuto di
# `main`. Niente e' andato perso, ma solo perche' il working tree non viene
# toccato da nessuno dei due comandi.
#
# Fuori da un hook il difetto non esiste (nessuna delle due variabili e'
# impostata), ed e' il motivo per cui lo script e' vissuto finora dentro
# `pnpm verify` senza mai mostrarlo. Lo stesso vale per qualunque script che
# maneggi un repo temporaneo: l'isolamento non lo da' `-C`, lo da' questo unset.
unset GIT_DIR GIT_INDEX_FILE GIT_WORK_TREE GIT_COMMON_DIR GIT_PREFIX \
  GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_NAMESPACE \
  GIT_INDEX_VERSION GIT_REFLOG_ACTION

# L'identita' dei commit usa-e-getta viaggia PER COMANDO (`-c`), mai scritta con
# `git config`, ed e' difesa in profondita' rispetto all'unset qui sopra.
#
# MISURATO l'09/08/2026, dopo il primo incidente: `git -C "$COMUNE" config
# user.name ...` con GIT_DIR ereditato non aveva solo committato sul repo reale
# — aveva RISCRITTO `D:/IDEAI/.git/config`, che i worktree condividono. Da quel
# momento ogni commit di OGNI sessione e' uscito firmato
# `gate selftest <selftest@nexus.local>` invece di `Nexus Dev <dev@ideai.local>`:
# 18 commit su 40, di sessioni diverse, prima che qualcuno se ne accorgesse.
# Quelli restano come sono (riscrivere una storia gia' pubblicata costa piu' del
# danno); il config e' stato ripristinato.
#
# `-c` non puo' fare questo danno per costruzione: vale per la singola
# invocazione e non tocca nessun file di configurazione, nemmeno se un domani
# qualcuno reintroducesse una variabile d'ambiente che scavalca `-C`.
IDENTITA_USA_E_GETTA=(-c user.name="gate selftest" -c user.email=selftest@nexus.local)

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
mkdir -p "$COMUNE/scripts"
for s in gate-env.sh gate-premesse.sh precommit-cargo-check.sh precommit-turbo.sh; do
  cp "$SCRIPTS/$s" "$COMUNE/scripts/$s"
done
git -C "$COMUNE" add -A
git -C "$COMUNE" "${IDENTITA_USA_E_GETTA[@]}" -c core.hooksPath="$NOHOOKS" \
  -c commit.gpgsign=false commit -q -m "selftest: script dei gate"
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
echo "### 7. Node insufficiente: la causa e' la versione, non il codice"
# La strada e' quella della produzione: verify.sh sorge gate-env.sh (che sorge
# gate-premesse.sh) e invoca la premessa. Il minimo lo legge da package.json
# dell'albero, che qui si scrive apposta — nel repo reale c'e' sempre, e col
# valore giusto, quindi il caso non sarebbe osservabile (regola O).
pretende_node() { # pretende_node <albero>  -> stampa il messaggio
  (cd "$1" && bash -c 'source scripts/gate-env.sh 2>/dev/null; gate_pretende_node' 2>&1)
}
codice_node() { # codice_node <albero>  -> stampa il solo codice d'uscita
  (cd "$1" && bash -c 'source scripts/gate-env.sh 2>/dev/null; gate_pretende_node' \
    >/dev/null 2>&1; echo $?)
}

printf '{"engines":{"node":">=999.0.0"}}\n' > "$WT/package.json"
esito "Node sotto il minimo: esce 78 (EX_CONFIG), non 1" "78" "$(codice_node "$WT")"
msg_node="$(pretende_node "$WT")"
case "$msg_node" in
  *"non soddisfa il minimo dichiarato"*) causa_node=si ;;
  *) causa_node="no -- $msg_node" ;;
esac
esito "il messaggio nomina la versione di node, non un file mancante" "si" "$causa_node"

printf '{"engines":{"node":">=0.0.1"}}\n' > "$WT/package.json"
esito "minimo soddisfatto: la premessa non intralcia" "0" "$(codice_node "$WT")"

# Un range che questo gate non sa confrontare non viene indovinato: si ferma
# dichiarandolo. Un confronto approssimato renderebbe la premessa piu'
# permissiva senza che nessuno lo sappia.
printf '{"engines":{"node":"^22 || >=24"}}\n' > "$WT/package.json"
esito "range non interpretabile: 78, non un confronto inventato" "78" "$(codice_node "$WT")"

# Nessun minimo dichiarato: niente da pretendere, nessun valore di ripiego.
printf '{"name":"selftest"}\n' > "$WT/package.json"
esito "engines.node assente: la premessa passa senza inventare un minimo" \
  "0" "$(codice_node "$WT")"
rm -f "$WT/package.json"

# Il caso 8 rilancia questo stesso script: la guardia impedisce la ricorsione.
if [ "${NEXUS_SELFTEST_ANNIDATO:-}" != "1" ]; then
  echo
  echo "### 8. eseguito come hook: non tocca il repo che lo ha invocato"
  # Il difetto misurato l'09/08 (vedi l'unset in testa). La misura riproduce
  # l'ambiente vero di un hook — GIT_DIR e GIT_INDEX_FILE esportate da git — e
  # guarda la CONSEGUENZA su un repo vittima: il suo HEAD e la sua lista di
  # worktree. Non verifica che le variabili siano state unsettate: quello e' il
  # come, e un domani il rimedio potrebbe essere un altro (regola O).
  VITTIMA="$TMP/vittima"
  git init -q -b main "$VITTIMA"
  printf 'segnaposto\n' > "$VITTIMA/file.txt"
  git -C "$VITTIMA" add -A
  git -C "$VITTIMA" "${IDENTITA_USA_E_GETTA[@]}" -c core.hooksPath="$NOHOOKS" \
    -c commit.gpgsign=false commit -q -m "vittima: stato iniziale"
  head_prima="$(git -C "$VITTIMA" rev-parse HEAD)"
  wt_prima="$(git -C "$VITTIMA" worktree list | wc -l)"
  # L'IDENTITA' della vittima: il danno peggiore del 09/08 non fu il commit di
  # straforo ma il `git config user.name` finito nel config CONDIVISO dei
  # worktree — da li' in poi ogni commit di ogni sessione usciva firmato
  # "gate selftest". Si guarda il config, non il commit.
  git -C "$VITTIMA" config user.name "Proprietario Vero"
  git -C "$VITTIMA" config user.email vero@esempio.local
  nome_prima="$(git -C "$VITTIMA" config user.name)"

  # Una modifica NON committata nell'indice: e' cio' che il difetto committava.
  printf 'modifica in staging\n' > "$VITTIMA/file.txt"
  git -C "$VITTIMA" add -A

  GIT_DIR="$VITTIMA/.git" GIT_INDEX_FILE="$VITTIMA/.git/index" \
    NEXUS_SELFTEST_ANNIDATO=1 bash "$SCRIPTS/gate-premesse-selftest.sh" >/dev/null 2>&1
  annidato=$?

  esito "l'esecuzione annidata supera i propri casi" "0" "$annidato"
  esito "HEAD della vittima invariato (nessun commit di straforo)" \
    "$head_prima" "$(git -C "$VITTIMA" rev-parse HEAD)"
  esito "nessun worktree registrato nella vittima" \
    "$wt_prima" "$(git -C "$VITTIMA" worktree list | wc -l)"
  esito "l'identita' della vittima non viene riscritta" \
    "$nome_prima" "$(git -C "$VITTIMA" config user.name)"
fi

echo
if [ "$falliti" -eq 0 ]; then
  echo "OK gate-premesse-selftest: tutti i casi superati."
else
  echo "FALLITO gate-premesse-selftest: $falliti caso/i non superato/i." >&2
fi
exit "$([ "$falliti" -eq 0 ] && echo 0 || echo 1)"
