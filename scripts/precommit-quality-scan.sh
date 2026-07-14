#!/usr/bin/env bash
# Pre-commit: gate ratchet delle metriche di qualita Rust.
#
# Delega a scripts/quality-scan.sh, che resta l'unico punto che sa COME si
# invoca il gate (riuso del binario xtask gia' compilato, fallback su cargo
# run): qui si aggiunge solo cio' che e' specifico del pre-commit, cioe' il
# PATH di cargo (regola L: nessuna seconda copia dell'invocazione).
#
# Perche' nel pre-commit e non solo in `pnpm verify`/CI: il gate confronta il
# workspace INTERO con scripts/quality-baseline.json, quindi chi lo esegue paga
# anche il debito introdotto da altri. Girando solo nella verifica completa, il
# drift si accumulava in silenzio per decine di commit e veniva scoperto — e
# ripulito — dal primo malcapitato che eseguiva `pnpm verify` (misurato una
# volta: 92 commit di drift, +812 finding). Qui lo vede chi lo introduce,
# quando lo introduce, che e' l'unico momento in cui costa poco correggerlo.
#
# Costo: ~2-8s a target caldo (la scansione pura e' ~2s per ~900 file). Il primo
# build di xtask dopo un clean richiede minuti, come qualunque comando cargo;
# chi lavora in Rust ha gia' il target caldo da cargo check/test. Non serve
# DATABASE_URL: xtask non usa le macro sqlx `query!` (verificato).
set -euo pipefail
cd "$(dirname "$0")/.."
# PATH: la shell degli hook lefthook su Windows puo' non avere cargo nel PATH.
export PATH="$HOME/.cargo/bin:$PATH"

exec bash scripts/quality-scan.sh --gate
