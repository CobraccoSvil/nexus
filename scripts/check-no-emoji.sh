#!/usr/bin/env bash
# scripts/check-no-emoji.sh — guard commit-msg: blocca emoji nel messaggio di
# commit (direttiva editoriale Nexus, CLAUDE.md sezione A).
#
# Punto unico (regola L) della verifica emoji lato commit-msg, invocato da
# lefthook. Portabile Linux/WSL/Windows git-bash:
#   - `grep -P` (range Unicode BMP + supplementari) richiede un locale UTF-8;
#     git-bash su Windows usa per default un locale non-UTF-8 e fallirebbe con
#     "grep: -P supports only unibyte and UTF-8 locales". LC_ALL=C.UTF-8 lo forza.
#   - Forma a singolo comando: il blocco `run: |` multi-line di lefthook non
#     sopravvive all'esecuzione su git-bash Windows (syntax error), per questo
#     la logica vive qui e l'hook chiama lo script in una riga sola.
#
# Uso: bash scripts/check-no-emoji.sh <path-file-messaggio>
# Exit 1 se il messaggio contiene emoji, 0 altrimenti.

set -euo pipefail

msg_file="${1:?path del file messaggio mancante}"

if LC_ALL=C.UTF-8 grep -Pq "[\x{1F300}-\x{1FAFF}\x{2600}-\x{27BF}]" "$msg_file"; then
  echo "Commit message contiene emoji: rimuovile - direttiva editoriale" >&2
  exit 1
fi
