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
#   4. I gate ratchet (fine-riga, settings, duplicazione, i18n, qualita' Rust)
#
# Utilizzato sia dall'hook pre-commit (lefthook) sia dalla CI.
# Uscita non-zero se qualunque fase fallisce. Stampa il nome delle fasi fallite,
# e — quando si e' fermato prima della fine — quelle che NON ha eseguito.

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

# Piano delle fasi, esecuzione, riepilogo e fail-fast: punto unico, sorgibile e
# provato a parte da scripts/gate-fasi-selftest.sh. Sta fuori di qui perche' un
# gate che dura venti minuti non e' un banco di prova: la meccanica va potuta
# esercitare con fasi finte, senza compilare niente (regola O).
# shellcheck source=scripts/gate-fasi.sh
source "$ROOT_DIR/scripts/gate-fasi.sh"

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
    echo "   albero       : ${ROOT_DIR}"
    echo "   target dir   : ${target} (${stato_cache})"
    echo "   incrementale : CARGO_INCREMENTAL=${CARGO_INCREMENTAL:-<non impostata>}"
    echo "   toolchain    : $(rustc --version 2>/dev/null || echo '<rustc non invocabile>')"
    # La versione di Node e' una premessa quanto la toolchain Rust: le fasi TS e
    # i gate ratchet girano su di lei, e la differenza fra locale e CI e' stata
    # per un mese la sola causa del rosso (vedi gate_pretende_node).
    echo "   node         : $(node --version 2>/dev/null || echo '<node non invocabile>') (minimo: $(node -p "require('./package.json').engines?.node ?? '<non dichiarato>'" 2>/dev/null || echo '<illeggibile>'))"
    if [[ "$FAIL_FAST" == "1" ]]; then
        echo "   alla prima fase rossa: STOP (fail-fast)"
    else
        echo "   alla prima fase rossa: prosegue (riporta tutte le fasi rotte)"
    fi
    # QUALE file ha dato DATABASE_URL, non il suo valore. In un worktree e' il
    # .env del repo COMUNE: senza questa riga "i test sqlx passano" non dice
    # contro quale configurazione, ed e' proprio la differenza fra i due alberi
    # (regola O: un numero senza la sua premessa e' un'opinione).
    if [[ -n "${NEXUS_GATE_ENV_FILE:-}" ]]; then
        echo "   DATABASE_URL : letta da ${NEXUS_GATE_ENV_FILE}"
    elif [[ -n "${DATABASE_URL:-}" ]]; then
        echo "   DATABASE_URL : ereditata dall'ambiente"
    else
        echo "   DATABASE_URL : <non impostata: le fasi sqlx falliranno>"
    fi
    echo
}

SKIP_RUST="${VERIFY_SKIP_RUST:-0}"
SKIP_TS="${VERIFY_SKIP_TS:-0}"

# PREMESSE, TUTTE PRIMA DI QUALUNQUE FASE.
#
# Una premessa non soddisfatta non e' un difetto del codice: il gate non e'
# eseguibile, e lo dichiara col codice dedicato (78) invece di un rosso
# indistinguibile da un test fallito. Punto unico in gate-premesse.sh.
#
# Stanno tutte QUI, in testa, e non sparse fra le fasi: la premessa di nextest
# viveva in mezzo al gate, quindi in CI una sua assenza si sarebbe scoperta dopo
# gli otto minuti delle fasi TypeScript. Chiedere prima costa qualche secondo e
# rende l'esito immediato.
if [[ "$SKIP_TS" != "1" ]]; then
    gate_pretende_turbo
fi
# Anche a TS saltato: i gate ratchet i18n e honeypot-tsc girano su node.
gate_pretende_node
if [[ "$SKIP_RUST" != "1" ]]; then
    gate_pretende_nextest
fi

# Prima di qualunque fase: sotto quali condizioni stanno per nascere i numeri.
# In testa e non solo nel riepilogo, cosi' resta stampata anche se il gate muore
# a meta'.
premessa

if [[ "$SKIP_TS" != "1" ]]; then
    aggiungi_fase "check TypeScript toolchain" node scripts/check-no-honeypot-tsc.mjs
    aggiungi_fase "turbo typecheck+lint+test" pnpm exec turbo run typecheck lint test --continue
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
    aggiungi_fase "cargo clippy --workspace --all-targets -- -D warnings" \
        cargo clippy --workspace --all-targets -- -D warnings
    # Esecutore: nextest invece di `cargo test`. Schedula i test in processi
    # separati, quindi non serializza per binario — sui ~4000 test del workspace,
    # e in particolare sui 106 `#[sqlx::test]` che creano un DB a testa (i
    # "long-pole"), e' dove si guadagna di piu'. La sua presenza e' una premessa,
    # pretesa sopra.
    aggiungi_fase "cargo nextest run --workspace --no-fail-fast" \
        cargo nextest run --workspace --no-fail-fast

    # I DOCTEST vanno eseguiti a parte, e questa riga NON e' ridondante: nextest
    # esegue solo binari di test compilati e non esegue i doctest — nel workspace
    # ce ne sono 90. Senza questa fase la copertura calerebbe in silenzio, che e'
    # il modo peggiore di perdere un pezzo di gate.
    aggiungi_fase "cargo test --doc --workspace" cargo test --doc --workspace
else
    echo "-- verify: Rust saltato (VERIFY_SKIP_RUST=1)"
fi

# Fine-riga dichiarati vs materializzati. Sta anche qui e non solo nel
# pre-commit perche' e' una proprieta' dell'ALBERO, non del commit: un checkout
# fresco puo' introdurla senza che nessuno committi nulla, e chi esegue il gate
# completo e' spesso il primo a toccare quell'albero.
aggiungi_fase "fine-riga dichiarati (eol=lf)" bash scripts/check-eol.sh

# Premesse dei gate, provate DA UN WORKTREE (regola O). Sta in un gate e non fra
# le verifiche a mano per la ragione che rende i suoi due difetti particolari:
# nel repo principale il .env c'e' e node_modules pure, quindi non sono
# osservabili da dove si guarda di solito, e una regressione resterebbe verde
# finche' qualcuno non crea un albero nuovo. Costo: pochi secondi, un repository
# usa-e-getta sotto /tmp.
aggiungi_fase "premesse dei gate da un worktree" bash scripts/gate-premesse-selftest.sh

# La meccanica delle fasi verifica se stessa. Sta nel gate e non fra le prove a
# mano per la lezione che il repo ha gia' pagato altrove: uno strumento che
# nessun gate interroga si e' costruito, non e' entrato in esercizio. Costa
# meno di un secondo e non compila niente.
aggiungi_fase "meccanica delle fasi (self-test)" bash scripts/gate-fasi-selftest.sh

# Gate ratchet configurazioni: settings morte/fantasma/invisibili possono solo
# scendere (baseline in scripts/audit-settings-baseline.json). Se il DB live
# non e' raggiungibile lo script degrada da solo in modalita' --no-db.
aggiungi_fase "audit settings (gate ratchet)" bash scripts/audit-settings.sh --gate

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
aggiungi_fase "duplicazione (gate ratchet jscpd)" bash scripts/dup-report.sh

# Gate ratchet testo non tradotto della web-ide (baseline in
# scripts/i18n-baseline.json). La UI e' bilingue a meta': 391 chiavi passano dal
# traduttore, 797 stringhe visibili sono letterali dentro i componenti, e due
# frasi adiacenti possono parlare lingue diverse. L'estrazione richiede piu'
# ondate; questo gate impedisce che il debito CRESCA nel frattempo, e la
# baseline si riallinea al ribasso dopo ognuna.
aggiungi_fase "testo non tradotto (gate ratchet i18n)" node scripts/i18n-ratchet.mjs

# Gate ratchet qualita codice Rust: findings totali, funzioni >50 righe,
# complessita >20, security possono solo scendere (baseline in
# scripts/quality-baseline.json). Saltabile con VERIFY_SKIP_RUST=1.
if [[ "$SKIP_RUST" != "1" ]]; then
    aggiungi_fase "quality scan (gate ratchet)" bash scripts/quality-scan.sh --gate
else
    echo "-- verify: quality scan saltato (VERIFY_SKIP_RUST=1)"
fi

esegui_piano
riepilogo_fasi

if piano_ha_fasi_rosse; then
    echo
    echo -e "${RED}KO verify: il gate ha fasi rosse (elenco sopra)${NC}" >&2
    exit 1
fi

echo
echo -e "${GREEN}OK verify: tutte le fasi passate${NC}"
