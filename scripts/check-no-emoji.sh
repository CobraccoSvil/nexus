#!/usr/bin/env bash
# Blocca emoji nel messaggio di commit (direttiva editoriale: niente emoji nei
# commit message). Estratto da lefthook.yml perche' la regex con \x{...} inline
# veniva corrotta dall'interpolazione dei template lefthook su Windows e il grep -P
# di Git Bash non supporta PCRE. Il check usa node (sempre presente nel toolchain),
# che decodifica UTF-8 in modo affidabile e indipendente dal locale.
# $1 = path del file con il messaggio di commit (lefthook lo passa come {1}).
set -euo pipefail

msg_file="${1:?uso: check-no-emoji.sh <path-commit-msg>}"

node -e '
  const fs = require("fs");
  const text = fs.readFileSync(process.argv[1], "utf8");
  // Stessi range della direttiva storica: simboli/pittogrammi e supplementari.
  if (/[\u{1F300}-\u{1FAFF}\u{2600}-\u{27BF}]/u.test(text)) {
    console.error("Commit message contiene emoji: rimuovile - direttiva editoriale");
    process.exit(1);
  }
' "$msg_file"
