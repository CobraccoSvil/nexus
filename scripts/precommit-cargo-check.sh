#!/usr/bin/env bash
# Pre-commit: clippy dell'intero workspace (stesso comando di `pnpm verify`,
# cosi' i due gate condividono gli artefatti invece di ricompilarsi a vicenda —
# vedi la nota sulla cache in fondo).
# Estratto da lefthook.yml per lo stesso motivo di precommit-turbo.sh: su Windows
# lefthook word-splittava 'bash -lc "cargo check --workspace"' in
# 'bash -lc cargo check --workspace', cosi' bash eseguiva il command string
# "cargo" SENZA subcomando -> stampava l'help ed usciva 0 (falso-verde: il check
# non veniva mai eseguito, la regola B di CLAUDE.md non era enforced).
#
# SQLx: le macro query! sono verificate a compile-time contro un DB reale (non
# esiste cache .sqlx, vedi .github/workflows/verify.yml). Serve quindi
# DATABASE_URL. Lo prendiamo dall'ambiente se gia' presente, altrimenti dal .env
# del repo (stesso pattern di deploy/deploy-local.sh, regola L) -- niente URL
# hardcoded (regola G). Senza, cargo check fallirebbe con un errore SQLx criptico.
#
# --locked: il pre-commit non deve mai aggiornare silenziosamente il lockfile; se
# Cargo.lock e' out-of-sync coi manifest il check fallisce e lo segnala (e' cosi'
# che si scopre un lock disallineato, vedi commit windows-sys su mcp-core).
set -euo pipefail
cd "$(dirname "$0")/.."
# PATH: la shell degli hook lefthook su Windows puo' non avere cargo nel PATH.
export PATH="$HOME/.cargo/bin:$PATH"
# Ambiente comune dei gate (CARGO_INCREMENTAL=0 e simili): punto unico.
# shellcheck source=scripts/gate-env.sh
source scripts/gate-env.sh

# La lettura del .env NON e' piu' qui: la fa gate-env.sh, sorgiato sopra, per
# tutti i gate insieme. DATABASE_URL entra nel fingerprint di Cargo
# (`sqlx-macros` dichiara `rerun-if-env-changed`), quindi due gate che la
# leggessero per conto proprio potrebbero divergere e invalidarsi a vicenda.
# Qui resta il solo fail-closed: chi non puo' procedere senza lo dice, e un file
# sorgiato non puo' uscire per conto del chiamante.
if [ -z "${DATABASE_URL:-}" ]; then
  echo "pre-commit cargo: DATABASE_URL non impostato e non trovato in <repo>/.env." >&2
  echo "  Le macro SQLx verificano le query a compile-time: avvia il DB locale ed" >&2
  echo "  esporta DATABASE_URL, oppure valorizzalo nel .env del repo (vedi" >&2
  echo "  .env.local.example)." >&2
  exit 1
fi

# clippy e NON `cargo check`, e il motivo e' la CACHE, non la severita'.
#
#   `cargo check` e `cargo clippy` non condividono gli artefatti: clippy imposta
#   RUSTC_WORKSPACE_WRAPPER=clippy-driver, che cambia il fingerprint di tutti i
#   crate del workspace. Finche' questo hook faceva `check` e `pnpm verify`
#   faceva `clippy`, il pre-commit NON riusava nulla di un verify appena
#   concluso: pagava un attraversamento completo dei 37 crate ogni volta, anche
#   subito dopo un gate verde. E' la causa che spiega i 647s misurati su un
#   commit di due file — non il target dir, non il .env.
#
#   Con lo stesso comando del gate completo, il flusso reale ("lancio verify,
#   poi committo") paga UNA volta sola: il secondo passaggio riusa il primo.
#
#   Effetto collaterale voluto: l'hook diventa severo quanto il gate. Non e' un
#   costo aggiunto — `main` e' gia' tenuto pulito da `-D warnings` da
#   `pnpm verify` e dalla CI, quindi cio' che passava prima passa anche ora.
#
#   --locked resta: il pre-commit non deve mai aggiornare silenziosamente il
#   lockfile.
exec cargo clippy --workspace --all-targets --locked -- -D warnings
