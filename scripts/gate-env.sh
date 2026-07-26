#!/usr/bin/env bash
# scripts/gate-env.sh — Ambiente comune dei gate (punto unico, regola L).
#
# Da sorgere (`source`) all'inizio di ogni script che invoca cargo PER VERIFICARE
# (verify.sh, quality-scan.sh, precommit-cargo-check.sh), non da eseguire.
# La stessa decisione in piu' script sarebbe destinata a divergere: qui c'e'
# scritta una volta, col perche'.
#
# CARGO_INCREMENTAL=0
#   La compilazione incrementale serve al dev-loop: ricompilare in fretta lo
#   STESSO crate mentre lo si modifica. Un gate compila, verifica e finisce —
#   quella cache non viene mai riusata, ma viene scritta lo stesso, e pesa.
#
#   Misurato il 2026-07-26 su questa macchina:
#     D:\IDEAI\target-verify   98,0 GB totali
#       debug/incremental      80,3 GB  (82%, 107.112 file)  <- mai riusata
#       debug/deps             16,8 GB  (gli artefatti veri)
#     D:\IDEAI\target          186,1 GB totali
#       debug/incremental     148,0 GB  (80%, 183.826 file)
#
#   Non e' una pulizia da rifare ogni mese: senza questa riga lo spazio torna
#   da solo al primo gate. Il dev-loop NON e' toccato — chi lancia `cargo build`
#   a mano nel proprio target continua ad avere l'incrementale attivo, perche'
#   questo file lo sorgono solo i gate.
export CARGO_INCREMENTAL=0
