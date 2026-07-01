#!/usr/bin/env bash
# Hook pre-commit lefthook (cargo_check_quick) estratto in script.
#
# Perche' uno script e non un comando inline: su Windows lefthook PERDE le
# virgolette annidate di `run: bash -lc "cargo check --workspace"`, che diventa
# `bash -lc cargo check --workspace` -> bash esegue solo `cargo` (senza
# subcomando) con `check --workspace` come parametri posizionali, e cargo stampa
# l'usage con exit 1 (falso fallimento). Un path di script singolo, senza quote
# annidate, e' robusto (stesso motivo di check-no-emoji.sh).
set -euo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
exec cargo check --workspace
