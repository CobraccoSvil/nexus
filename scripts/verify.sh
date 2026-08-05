#!/usr/bin/env bash
# scripts/verify.sh — Gate di build verification unificato per il monorepo.
#
# Esegue, nell'ordine:
#   1. Lint, typecheck e test TypeScript via turbo
#   2. cargo clippy workspace --all-targets con -D warnings
#      (superset stretto di `cargo check --workspace`, che era una fase a se'
#      fino al 2026-08-05: stessa copertura, un attraversamento in meno)
#   3. cargo nextest run workspace (no fail-fast) + cargo test --doc workspace
#      (i doctest vanno a parte: nextest non li esegue)
#
# Utilizzato sia dall'hook pre-commit (lefthook) sia dalla CI.
# Uscita non-zero se qualunque fase fallisce. Stampa il nome della fase fallita.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

# Ambiente comune dei gate (CARGO_INCREMENTAL=0 e simili): punto unico.
# shellcheck source=scripts/gate-env.sh
source "$ROOT_DIR/scripts/gate-env.sh"

YELLOW="\033[0;33m"
GREEN="\033[0;32m"
RED="\033[0;31m"
NC="\033[0m"

# Durata di ogni fase, per il riepilogo finale. Sta QUI e non nei chiamanti
# perche' run_phase e' il punto unico da cui passa ogni fase (regola L): una
# misura aggiunta a ogni call site divergerebbe alla prima fase nuova.
FASI_NOMI=()
FASI_SECONDI=()

# La premessa che accompagna i numeri: due run con premesse diverse non sono
# confrontabili, e senza questa riga non si vede (regola O).
#
# NON stampa DATABASE_URL: redigerne la password richiederebbe una seconda copia
# di `xtask::premessa::db_dichiarato`, che da bash non e' richiamabile senza
# compilare xtask. Non e' una perdita — l'URL del DB non e' una premessa dei
# TEMPI, che dipendono da target dir, incrementale e toolchain.
premessa() {
    local target="${CARGO_TARGET_DIR:-${ROOT_DIR}/target}"
    local stato_cache="freddo (nessun artefatto)"
    if [[ -d "${target}/debug/deps" ]] && [[ -n "$(ls -A "${target}/debug/deps" 2>/dev/null | head -1)" ]]; then
        stato_cache="caldo"
    fi
    echo -e "${YELLOW}== verify: da dove sto guardando ==${NC}"
    echo "   target dir   : ${target} (${stato_cache})"
    echo "   incrementale : CARGO_INCREMENTAL=${CARGO_INCREMENTAL:-<non impostata>}"
    echo "   toolchain    : $(rustc --version 2>/dev/null || echo '<rustc non invocabile>')"
    echo
}

# Riepilogo ordinato per durata decrescente. Stampato anche quando una fase
# fallisce: le fasi gia' concluse restano una misura valida, e chi debugga un
# gate lento ha bisogno proprio di quelle.
riepilogo_fasi() {
    [[ ${#FASI_NOMI[@]} -eq 0 ]] && return 0
    local i totale=0
    for i in "${!FASI_NOMI[@]}"; do
        totale=$((totale + ${FASI_SECONDI[$i]}))
    done
    echo
    echo -e "${YELLOW}== verify: durata per fase (decrescente) ==${NC}"
    for i in "${!FASI_NOMI[@]}"; do
        printf '%s\t%s\n' "${FASI_SECONDI[$i]}" "${FASI_NOMI[$i]}"
    done | sort -rn | while IFS=$'\t' read -r sec nome; do
        printf '   %3dm%02ds  %s\n' $((sec / 60)) $((sec % 60)) "$nome"
    done
    printf '   %3dm%02ds  TOTALE\n' $((totale / 60)) $((totale % 60))
}

run_phase() {
    local name="$1"
    shift
    echo -e "${YELLOW}==> verify: ${name}${NC}"
    local inizio=$SECONDS
    local esito=0
    # `|| esito=$?` invece di `if ! ...`: la durata va registrata anche quando la
    # fase fallisce, e con `set -e` un comando fallito senza guard uscirebbe qui.
    "$@" || esito=$?
    FASI_NOMI+=("$name")
    FASI_SECONDI+=("$((SECONDS - inizio))")
    if [[ $esito -ne 0 ]]; then
        echo -e "${RED}!! verify: fase '${name}' FALLITA${NC}" >&2
        riepilogo_fasi
        exit 1
    fi
}

SKIP_RUST="${VERIFY_SKIP_RUST:-0}"
SKIP_TS="${VERIFY_SKIP_TS:-0}"

# Prima di qualunque fase: sotto quali condizioni stanno per nascere i numeri.
# In testa e non solo nel riepilogo, cosi' resta stampata anche se il gate muore
# a meta'.
premessa


if [[ "$SKIP_TS" != "1" ]]; then
    run_phase "check TypeScript toolchain" node scripts/check-no-honeypot-tsc.mjs
    run_phase "turbo typecheck+lint+test" pnpm exec turbo run typecheck lint test --continue
else
    echo "-- verify: TS saltato (VERIFY_SKIP_TS=1)"
fi

if [[ "$SKIP_RUST" != "1" ]]; then
    # NB: qui NON c'e' un `cargo check --workspace`, e non e' una dimenticanza.
    # `clippy --all-targets` e' un suo superset STRETTO: clippy *e'* rustc con
    # lint aggiuntive, quindi ogni errore che check rileverebbe lo rileva anche
    # lui, e `--all-targets` copre lib+bin+test+bench+example contro i soli
    # lib+bin di `check --workspace`.
    #
    # Non condividono la cache: clippy imposta RUSTC_WORKSPACE_WRAPPER=
    # clippy-driver, che cambia il fingerprint dei crate del workspace (le
    # dipendenze esterne restano condivise). Il check era percio' un
    # attraversamento completo dei 37 crate il cui risultato veniva buttato via.
    #
    # Cio' che si perde: il rosso su un errore di sintassi arriva qualche decina
    # di secondi piu' tardi. Nessuna copertura.
    run_phase "cargo clippy --workspace --all-targets -- -D warnings" \
        cargo clippy --workspace --all-targets -- -D warnings
    # Esecutore: nextest invece di `cargo test`. Schedula i test in processi
    # separati, quindi non serializza per binario — sui ~4000 test del workspace,
    # e in particolare sui 106 `#[sqlx::test]` che creano un DB a testa (i
    # "long-pole"), e' dove si guadagna di piu'.
    #
    # Obbligatorio, non opzionale: un gate che ripiegasse in silenzio su
    # `cargo test` misurerebbe una cosa diversa da quella che misura la CI, e la
    # differenza non si vedrebbe finche' non conta (regola O).
    if ! cargo nextest --version >/dev/null 2>&1; then
        echo -e "${RED}!! verify: cargo-nextest non e' installato.${NC}" >&2
        echo "   Installalo con: cargo install cargo-nextest --locked" >&2
        echo "   (e' l'esecutore dei test del gate: senza, questo gate non e' eseguibile)" >&2
        exit 1
    fi
    run_phase "cargo nextest run --workspace --no-fail-fast" \
        cargo nextest run --workspace --no-fail-fast

    # I DOCTEST vanno eseguiti a parte, e questa riga NON e' ridondante: nextest
    # esegue solo binari di test compilati e non esegue i doctest — nel workspace
    # ce ne sono 90. Senza questa fase la copertura calerebbe in silenzio, che e'
    # il modo peggiore di perdere un pezzo di gate.
    run_phase "cargo test --doc --workspace" cargo test --doc --workspace
else
    echo "-- verify: Rust saltato (VERIFY_SKIP_RUST=1)"
fi

# Fine-riga dichiarati vs materializzati. Sta anche qui e non solo nel
# pre-commit perche' e' una proprieta' dell'ALBERO, non del commit: un checkout
# fresco puo' introdurla senza che nessuno committi nulla, e chi esegue il gate
# completo e' spesso il primo a toccare quell'albero.
run_phase "fine-riga dichiarati (eol=lf)" bash scripts/check-eol.sh

# Gate ratchet configurazioni: settings morte/fantasma/invisibili possono solo
# scendere (baseline in scripts/audit-settings-baseline.json). Se il DB live
# non e' raggiungibile lo script degrada da solo in modalita' --no-db.
run_phase "audit settings (gate ratchet)" bash scripts/audit-settings.sh --gate

# Gate ratchet duplicazione (jscpd, baseline in .dup-baseline.json).
#
# Fino al 2026-08-05 girava SOLO nel workflow CI, e il risultato era il difetto
# che il repo aveva gia' imparato per quality_scan: "confinato alla verifica
# completa, il drift si accumulava invisibile per decine di commit e ricadeva
# sul primo che eseguiva pnpm verify". Misurato quel giorno: 11 cloni contro una
# baseline di 9, senza che nessuna esecuzione locale potesse accorgersene.
#
# Costa 19s (misurati) su un gate che ne dura oltre 1300: non e' il motivo per
# cui sta fuori dal pre-commit — li' resta escluso perche' scansiona l'albero
# intero, e un pre-commit deve guardare cio' che si sta committando.
run_phase "duplicazione (gate ratchet jscpd)" bash scripts/dup-report.sh

# Gate ratchet qualita codice Rust: findings totali, funzioni >50 righe,
# complessita >20, security possono solo scendere (baseline in
# scripts/quality-baseline.json). Saltabile con VERIFY_SKIP_RUST=1.
if [[ "$SKIP_RUST" != "1" ]]; then
    run_phase "quality scan (gate ratchet)" bash scripts/quality-scan.sh --gate
else
    echo "-- verify: quality scan saltato (VERIFY_SKIP_RUST=1)"
fi

riepilogo_fasi
echo
echo -e "${GREEN}OK verify: tutte le fasi passate${NC}"
