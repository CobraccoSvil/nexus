#!/usr/bin/env bash
# Classifica i warning di lint web-ide per file, leggendo un log di verify.
# Parsing via node (jq non e' garantito sull'ambiente; node si'), niente python3.
# Il log di default e' quello prodotto dalla campagna backlog-closure; si puo'
# sovrascrivere passando un percorso come primo argomento.
set -uo pipefail

LOG="${1:-/tmp/backlog-closure/verify.log}"

if [ ! -r "$LOG" ]; then
  echo "log non leggibile: $LOG" >&2
  exit 1
fi

LOG="$LOG" node -e '
const fs = require("fs");

const log = process.env.LOG;
const fileRe = /@ai-orchestrator\/web-ide:lint:\s*(\/home\/administrator\/ideai\/apps\/web-ide\/\S+\.tsx?)$/;
const warnRe = /warning\s+(.+?)\s+(@?[\w-]+\/[\w-]+)\s*$/;

let current = null;
const warningsByFile = new Map();

for (const raw of fs.readFileSync(log, "utf8").split("\n")) {
  const line = raw.replace(/\r$/, "");
  const m = line.match(fileRe);
  if (m) {
    current = m[1].replace("/home/administrator/ideai/", "");
    continue;
  }
  // Il vecchio predicato python si riduceva a: current && line.includes("warning").
  if (current && line.includes("warning")) {
    const mw = line.match(warnRe);
    if (mw) {
      if (!warningsByFile.has(current)) warningsByFile.set(current, []);
      warningsByFile.get(current).push([mw[2], mw[1].trim()]);
    }
  }
}

// sorted(key=-len) in python e stabile: a parita di conteggio resta l ordine
// di prima apparizione, come la Map di JS con sort stabile.
const ranked = [...warningsByFile.entries()].sort((a, b) => b[1].length - a[1].length);
let total = 0;
for (const [f, ws] of ranked) {
  total += ws.length;
  console.log(f + ": " + ws.length + " warnings");
}
console.log("\nTOTALE: " + total);
'
