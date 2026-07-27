#!/usr/bin/env bash
# Pre-commit veloce: cargo check dell'intero workspace.
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

if [ -z "${DATABASE_URL:-}" ]; then
  env_file="$(git rev-parse --show-toplevel 2>/dev/null)/.env"
  if [ -f "$env_file" ]; then
    # Estrae il valore senza eseguire il file; strip di un eventuale CR finale (.env editato su Windows).
    DATABASE_URL="$(grep -m1 '^DATABASE_URL=' "$env_file" 2>/dev/null | cut -d= -f2-)"
    DATABASE_URL="${DATABASE_URL%$'\r'}"
    export DATABASE_URL
  fi
fi

if [ -z "${DATABASE_URL:-}" ]; then
  echo "pre-commit cargo: DATABASE_URL non impostato e non trovato in <repo>/.env." >&2
  echo "  Le macro SQLx verificano le query a compile-time: avvia il DB locale ed" >&2
  echo "  esporta DATABASE_URL, oppure valorizzalo nel .env del repo (vedi" >&2
  echo "  .env.local.example)." >&2
  exit 1
fi

exec cargo check --workspace --locked
