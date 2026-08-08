#!/usr/bin/env bash
# Esegue scripts/liveness-selftest.ps1 dove PowerShell c'e', e lo DICHIARA dove
# non c'e'. Vive in un file e non nel `run:` di lefthook perche' quel campo
# arriva a `sh -c` e un blocco if/then/fi su piu' righe viene troncato: il gate
# fallirebbe senza aver eseguito nulla, che e' il modo peggiore di fallire.
#
# Il salto e' dichiarato e non silenzioso (regola O): in CI Linux `powershell.exe`
# non esiste, e un gate che li' risultasse verde per assenza direbbe «criterio
# verificato» avendo verificato zero.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if command -v powershell.exe >/dev/null 2>&1; then
  exec powershell.exe -NoProfile -File scripts/liveness-selftest.ps1
fi
echo "SALTATO liveness-selftest: powershell.exe non disponibile su questo host (il criterio NON e' stato verificato qui)"
