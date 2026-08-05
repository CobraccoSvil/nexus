#!/usr/bin/env bash
# scripts/gate-env.sh — Ambiente comune dei gate (punto unico, regola L).
#
# Da sorgere (`source`) all'inizio di ogni script che invoca cargo PER VERIFICARE
# (verify.sh, quality-scan.sh, precommit-cargo-check.sh), non da eseguire.
# La stessa decisione in piu' script sarebbe destinata a divergere: qui c'e'
# scritta una volta, col perche'.
#
# COSA APPARTIENE A QUESTO FILE: tutto cio' che entra nel FINGERPRINT di Cargo,
# cioe' tutto cio' che, se differisse fra due gate, li farebbe ricompilare a
# vicenda. Non e' una collezione di comodita': e' l'insieme delle variabili per
# cui "i due gate concordano" deve valere per COSTRUZIONE e non per coincidenza
# di quale shell li ha lanciati (regola L).
#
# CARGO_INCREMENTAL — dipende dal REGIME in cui il gate gira, non dalla macchina.
#
#   Fino al 2026-08-05 qui c'era un `export CARGO_INCREMENTAL=0` incondizionato,
#   motivato cosi': "un gate compila, verifica e finisce — quella cache non
#   viene mai riusata, ma viene scritta lo stesso, e pesa". La misura era vera:
#
#     D:\IDEAI\target-verify   98,0 GB totali
#       debug/incremental      80,3 GB  (82%, 107.112 file)
#       debug/deps             16,8 GB  (gli artefatti veri)
#     D:\IDEAI\target          186,1 GB totali
#       debug/incremental     148,0 GB  (80%, 183.826 file)
#
#   Ma quella premessa vale per UN regime solo.
#
#   CI: il runner nasce e muore dentro un job. La cache incrementale non verra'
#   MAI riletta, e scriverla e' puro costo. Qui lo 0 e' giusto.
#
#   LOCALE: chi sviluppa rilancia il gate sulla STESSA macchina, sullo STESSO
#   target, dopo una modifica piccola. Li' la cache viene riusata eccome, ed e'
#   l'unica cosa che eviti di ricompilare `mcp-core` intero — 215.421 righe,
#   meta' del workspace — per una riga cambiata. In Rust l'unita' di
#   compilazione e' il crate: sotto quel livello, l'incrementale (che riusa le
#   codegen unit non toccate) e' il solo meccanismo che si avvicini a
#   "ricompila solo cio' che ho toccato".
#
#   Spegnerlo incondizionatamente non ha tolto uno spreco: ha tolto lo spreco in
#   CI e, insieme, il riuso in locale. Il file esprimeva una decisione sola per
#   due regimi diversi — mancava la distinzione, non la misura.
#
#   MISURATO il 2026-08-05 sul caso reale (una riga aggiunta a
#   `crates/mcp-core/src/run_lineage.rs`, poi `cargo clippy --workspace
#   --all-targets`, stesso target scaldato in ciascun regime):
#     senza incrementale : 81s
#     con incrementale   : 23s
#   Cioe' 3,5x, ~58s risparmiati a ogni ciclo edit-verify sulla sola fase
#   clippy. Meno del "fino al 50%" che si legge in giro sul parallel frontend, e
#   piu' del previsto per una modifica che e' un `if`: il numero vale piu' della
#   stima, ed e' per questo che sta scritto qui.
#
#   Il confronto ha richiesto di scaldare il target DUE volte, una per regime:
#   i due non sono confrontabili sullo stesso target caldo, perche' cambiare
#   questo valore cambia il fingerprint e invalida tutto.
#
#   Discriminante: la variabile CI, che ogni runner GitHub imposta. E' un
#   segnale strutturato (regola M), non un hostname o un percorso da indovinare.
#
#   NB: il primo gate dopo un cambio di questo valore ricompila tutto — il
#   fingerprint di Cargo cambia. Il guadagno e' dal secondo in poi.
#
#   `.github/workflows/verify.yml` continua a dichiarare CARGO_INCREMENTAL: 0
#   nell'env del job, e non e' una ridondanza da togliere: un YAML non puo'
#   sorgere uno script shell, e quella riga copre anche gli step che invocano
#   cargo SENZA passare da qui (`xtask migrate`, `service-manifests`). I due
#   percorsi concordano — in CI la variabile CI c'e' sempre — quindi non esiste
#   un caso in cui l'uno contraddica l'altro.
if [[ -n "${CI:-}" ]]; then
    export CARGO_INCREMENTAL=0
else
    export CARGO_INCREMENTAL=1
fi

# La radice del repo/worktree, risolta da QUESTO file e non ereditata dal
# chiamante: cosi' il valore non dipende da chi ci sorge (precommit-cargo-check.sh
# non definisce ROOT_DIR, verify.sh si).
_gate_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# CARGO_TARGET_DIR — dove i gate compilano.
#
#   Il difetto che chiude: due gate che compilano in target DIVERSI non
#   condividono niente, quindi ognuno paga un cold build completo. E' successo
#   davvero — `CARGO_TARGET_DIR=target-verify` esportata a mano prima di un
#   `git commit` per riusare la cache, mentre il pre-commit di suo non la
#   imposta: il verify scaldava un target e il commit ne trovava un altro
#   freddo. Il workaround era diventato un gesto quotidiano, cioe' esattamente
#   il "restart manuale che diventa abitudine" che la regola H vieta.
#
#   Fissarlo QUI rende i due gate coerenti per costruzione: non dipendono piu'
#   da quale shell li ha lanciati ne' da cosa c'era nel suo ambiente.
#
#   Il valore e' `<radice>/target`, che e' anche il default di Cargo. Due
#   conseguenze volute: (a) oggi e' un NO-OP — nessuno ci perde una cache calda
#   applicando questa riga; (b) essendo relativo alla radice, ogni worktree ha
#   il proprio, che e' la separazione richiesta per non far girare i test di un
#   albero sul binario di un altro.
#
#   Override: `NEXUS_GATE_TARGET_DIR`, non `CARGO_TARGET_DIR`. Una variabile
#   dedicata rende l'intento esplicito e impedisce a una `CARGO_TARGET_DIR`
#   residua nell'ambiente di desincronizzare i gate senza che nessuno l'abbia
#   deciso. Il valore effettivo lo dichiara la premessa di verify.sh.
export CARGO_TARGET_DIR="${NEXUS_GATE_TARGET_DIR:-${_gate_root}/target}"

# DATABASE_URL — le macro SQLx `query!` verificano le query a compile-time
#   contro un DB reale (il repo NON usa la cache offline `.sqlx`: scelta
#   dichiarata, vedi precommit-cargo-check.sh).
#
#   Sta qui perche' `sqlx-macros` dichiara `rerun-if-env-changed=DATABASE_URL`:
#   se due gate la vedono diversa — o uno si' e uno no — Cargo invalida ogni
#   crate che usa `query!` E tutti i suoi dipendenti, cioe' `mcp-core` e mezzo
#   workspace. E' fingerprint a tutti gli effetti, quindi appartiene a questo
#   file.
#
#   Finora funzionava per una proprieta' non dichiarata: sqlx carica il `.env`
#   per conto suo. Reggeva, ma nessuno l'aveva scritto, e bastava un gate che
#   esportasse la variabile per far divergere gli altri. Ora la lettura e' una
#   sola, esplicita, e vale per tutti i gate.
#
#   Qui si LEGGE e si esporta; non si impone. Chi non puo' procedere senza
#   (precommit-cargo-check.sh) tiene il proprio fail-closed: un file sorgiato
#   non puo' uscire senza terminare il chiamante.
if [[ -z "${DATABASE_URL:-}" ]] && [[ -f "${_gate_root}/.env" ]]; then
    # Estrae il valore senza eseguire il file; strip del CR finale (.env
    # editato su Windows).
    _gate_db_url="$(grep -m1 '^DATABASE_URL=' "${_gate_root}/.env" 2>/dev/null | cut -d= -f2-)"
    _gate_db_url="${_gate_db_url%$'\r'}"
    if [[ -n "$_gate_db_url" ]]; then
        export DATABASE_URL="$_gate_db_url"
    fi
    unset _gate_db_url
fi

unset _gate_root
