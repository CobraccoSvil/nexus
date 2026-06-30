#!/usr/bin/env bash
# Pre-commit veloce: cargo check dell'intero workspace.
# Estratto da lefthook.yml per lo stesso motivo di precommit-turbo.sh: su Windows
# lefthook word-splittava 'bash -lc "cargo check --workspace"' in
# 'bash -lc cargo check --workspace', cosi' bash eseguiva il command string
# "cargo" SENZA subcomando -> stampava l'help ed usciva 0 (falso-verde: il check
# non veniva mai eseguito, la regola B di CLAUDE.md non era enforced).
set -euo pipefail
cd "$(dirname "$0")/.."
exec cargo check --workspace
