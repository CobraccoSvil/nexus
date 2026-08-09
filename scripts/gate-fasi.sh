#!/usr/bin/env bash
# scripts/gate-fasi.sh — La sequenza di fasi di un gate: piano dichiarato,
# esecuzione, riepilogo (punto unico, regola L).
#
# Da sorgere (`source`), non da eseguire. Lo sorge verify.sh.
#
# IL DIFETTO CHE CHIUDE (misurato l'08/08/2026).
#
#   verify.sh eseguiva le fasi come una sequenza di chiamate, e alla prima rossa
#   usciva. Il riepilogo elencava percio' le sole fasi CONCLUSE — tutte verdi,
#   per costruzione — e chi leggeva il log di un gate rosso non poteva sapere
#   quanta parte del gate non avesse nemmeno provato a misurare.
#
#   Non e' teoria: `.github/workflows/verify.yml` e' stato rosso su OGNI run dal
#   2026-07-03, sempre nella prima fase (`@ai-orchestrator/web-ide#test`, per una
#   versione di Node insufficiente). clippy e nextest, che vengono dopo, non
#   sono mai stati eseguiti — e nel log non comparivano ne' come verdi ne' come
#   rossi: semplicemente non c'erano. Per cinque settimane "la CI e' rossa" non
#   ha mai voluto dire "Rust e' rotto", e nessuno poteva dedurlo dall'output.
#
# LE DUE PROPRIETA' CHE NE DERIVANO.
#
#   1. Il piano e' DICHIARATO prima e percorso da un ciclo solo: "previste" ed
#      "eseguite" sono lo stesso oggetto e non possono divergere. La differenza
#      fra i due insiemi e' esattamente cio' che il riepilogo deve dire.
#
#   2. Una fase non eseguita e' un ESITO, non un'assenza (regole O e Q): il suo
#      stato e' IGNOTO, che non e' verde. Dichiararlo e' l'unico modo perche' un
#      gate interrotto non venga letto come un gate che ha misurato tutto.
#
# FAIL-FAST: giusto in locale, sbagliato in CI.
#
#   In locale il gate serve a chi sta per committare: il rosso deve arrivare
#   presto, e l'ordine delle fasi (la piu' rapida per prima) e' scelto apposta.
#   In CI serve a dire QUANTE cose sono rotte: fermarsi alla prima trasforma un
#   difetto TypeScript in un silenzio su tutto il resto.
#
#   Discriminante: la variabile CI, che ogni runner GitHub imposta. Segnale
#   strutturato (regola M), come per CARGO_INCREMENTAL in gate-env.sh.
#   Override esplicito: VERIFY_FAIL_FAST=0|1.
#
#   IL COSTO, dichiarato: un run che prima moriva in 8 minuti ora arriva in
#   fondo, quindi ne consuma 25-30. Su un account Free con 2000 minuti/mese non
#   e' trascurabile — ma il confronto giusto non e' "8 minuti contro 30": e'
#   "otto minuti che non dicono niente" contro "trenta che dicono cosa e' rotto".
#   Un rosso che non si puo' leggere e' speso comunque, e per cinque settimane
#   e' stato speso 300 volte.

# Colori: definiti solo se il chiamante non li ha gia'.
: "${YELLOW:=$'\033[0;33m'}"
: "${RED:=$'\033[0;31m'}"
: "${NC:=$'\033[0m'}"

# Ogni elemento del piano e' "nome<US>arg1<US>arg2...". Il separatore e' US
# (0x1F) e non uno spazio: cosi' un argomento con spazi resta un argomento solo.
FASI_US=$'\x1f'
FASI_PIANO=()

# Esito per fase, riempiti da esegui_piano. Indici allineati fra loro.
FASI_NOMI=()
FASI_SECONDI=()
FASI_ESITO=()
FASI_NON_ESEGUITE=()

if [[ -n "${CI:-}" ]]; then
    FAIL_FAST="${VERIFY_FAIL_FAST:-0}"
else
    FAIL_FAST="${VERIFY_FAIL_FAST:-1}"
fi

aggiungi_fase() { # aggiungi_fase <nome> <comando> [arg...]
    local riga="$1"
    shift
    local arg
    for arg in "$@"; do
        riga+="${FASI_US}${arg}"
    done
    FASI_PIANO+=("$riga")
}

esegui_piano() {
    local i nome inizio esito
    local fermato=0
    local -a campi
    [[ ${#FASI_PIANO[@]} -gt 0 ]] || return 0
    for i in "${!FASI_PIANO[@]}"; do
        IFS="$FASI_US" read -r -a campi <<< "${FASI_PIANO[$i]}"
        nome="${campi[0]}"
        if [[ "$fermato" == "1" ]]; then
            FASI_NON_ESEGUITE+=("$nome")
            continue
        fi
        echo -e "${YELLOW}==> verify: ${nome}${NC}"
        inizio=$SECONDS
        esito=0
        # `|| esito=$?` invece di `if ! ...`: la durata va registrata anche
        # quando la fase fallisce, e con `set -e` un comando fallito senza guard
        # uscirebbe qui.
        "${campi[@]:1}" || esito=$?
        FASI_NOMI+=("$nome")
        FASI_SECONDI+=("$((SECONDS - inizio))")
        FASI_ESITO+=("$esito")
        if [[ $esito -ne 0 ]]; then
            echo -e "${RED}!! verify: fase '${nome}' FALLITA${NC}" >&2
            [[ "$FAIL_FAST" == "1" ]] && fermato=1
        fi
    done
}

# Riepilogo ordinato per durata decrescente. Stampato anche quando una fase
# fallisce: le fasi gia' concluse restano una misura valida, e chi debugga un
# gate lento ha bisogno proprio di quelle.
#
# Dichiara TRE insiemi, non uno: eseguite (con durata ed esito), fallite, e non
# eseguite.
riepilogo_fasi() {
    [[ ${#FASI_NOMI[@]} -eq 0 && ${#FASI_NON_ESEGUITE[@]} -eq 0 ]] && return 0
    local i totale=0
    for i in "${!FASI_NOMI[@]}"; do
        totale=$((totale + ${FASI_SECONDI[$i]}))
    done
    echo
    echo -e "${YELLOW}== verify: durata per fase (decrescente) ==${NC}"
    if [[ ${#FASI_NOMI[@]} -gt 0 ]]; then
        for i in "${!FASI_NOMI[@]}"; do
            printf '%s\t%s\t%s\n' "${FASI_SECONDI[$i]}" "${FASI_ESITO[$i]}" "${FASI_NOMI[$i]}"
        done | sort -rn | while IFS=$'\t' read -r sec esito nome; do
            if [[ "$esito" == "0" ]]; then
                printf '   %3dm%02ds  ok     %s\n' $((sec / 60)) $((sec % 60)) "$nome"
            else
                printf '   %3dm%02ds  ROSSA  %s\n' $((sec / 60)) $((sec % 60)) "$nome"
            fi
        done
    fi
    printf '   %3dm%02ds  TOTALE\n' $((totale / 60)) $((totale % 60))

    local fallite=()
    for i in "${!FASI_NOMI[@]}"; do
        [[ "${FASI_ESITO[$i]}" == "0" ]] || fallite+=("${FASI_NOMI[$i]}")
    done
    if [[ ${#fallite[@]} -gt 0 ]]; then
        echo
        echo -e "${RED}== verify: fasi FALLITE (${#fallite[@]}) ==${NC}"
        printf '   - %s\n' "${fallite[@]}"
    fi
    if [[ ${#FASI_NON_ESEGUITE[@]} -gt 0 ]]; then
        echo
        echo -e "${YELLOW}== verify: fasi NON ESEGUITE (${#FASI_NON_ESEGUITE[@]}) ==${NC}"
        echo "   Il gate si e' fermato prima (fail-fast). Su queste non ha misurato"
        echo "   niente: il loro stato e' IGNOTO, non verde."
        printf '   - %s\n' "${FASI_NON_ESEGUITE[@]}"
    fi
}

# 0 = nessuna fase eseguita e' rossa. NB: non dice niente sulle non eseguite,
# ed e' voluto — un gate fermato a meta' esce comunque non-zero perche' la fase
# che l'ha fermato E' fra le eseguite.
piano_ha_fasi_rosse() {
    local esito
    [[ ${#FASI_ESITO[@]} -gt 0 ]] || return 1
    for esito in "${FASI_ESITO[@]}"; do
        [[ "$esito" == "0" ]] || return 0
    done
    return 1
}
