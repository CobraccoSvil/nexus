#!/usr/bin/env bash
# Conta gli stili inline (style={{) nei .tsx di apps/web-ide.
# Parsing via node (jq non e' garantito sull'ambiente; node si'), niente python3.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

node -e '
const fs = require("fs");
const path = require("path");

const root = "apps/web-ide";
const rows = [];

// Scansione ricorsiva dei .tsx, saltando node_modules e .next.
function walk(dir) {
  let entries;
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch {
    return;
  }
  for (const e of entries) {
    const full = path.join(dir, e.name);
    if (e.isDirectory()) {
      if (e.name === "node_modules" || e.name === ".next") continue;
      walk(full);
    } else if (e.isFile() && e.name.endsWith(".tsx")) {
      let text;
      try {
        text = fs.readFileSync(full, "utf8");
      } catch {
        continue;
      }
      const count = (text.match(/style=\{\{/g) || []).length;
      if (count > 0) rows.push([count, full.split(path.sep).join("/")]);
    }
  }
}
walk(root);

// Ordine identico al vecchio sort(reverse=True) su tuple (count, nome):
// count decrescente, a parita di count nome decrescente.
rows.sort((a, b) => (b[0] - a[0]) || (a[1] < b[1] ? 1 : a[1] > b[1] ? -1 : 0));

console.log("count".padStart(5) + " file");
console.log("-".repeat(60));
let total = 0;
for (const [count, name] of rows.slice(0, 30)) {
  total += count;
  console.log(String(count).padStart(5) + " " + name);
}
console.log("-".repeat(60));
console.log("Top 30 sum: " + total);
const all = rows.reduce((s, [c]) => s + c, 0);
console.log("All files:  " + all + " inline styles in " + rows.length + " files");
'
