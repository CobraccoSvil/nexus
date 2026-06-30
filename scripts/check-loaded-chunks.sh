#!/usr/bin/env bash
cd /home/administrator/ideai/apps/web-ide/.next/static/chunks
echo "=== Chunks caricati dalla pagina ==="
for f in 2882-3e7206b3a2f48bf7.js aaeded28-8350d8c0103546b4.js 8245-40fef82699ac5759.js 3636-78049e4e8ab3a1e1.js 2520-85fb78108a80b84d.js main-app-5413af280620220b.js webpack-777d5514e57d3a2b.js app/ide/page-*.js app/layout-*.js; do
  if [ -f "$f" ]; then
    has_es=$(grep -c 'event-stream' "$f" 2>/dev/null)
    has_ds=$(grep -c 'connectDispatcher\|useProjectDispatcher\|fetchSnapshot' "$f" 2>/dev/null)
    has_apply=$(grep -c 'applySnapshot' "$f" 2>/dev/null)
    sz=$(stat -c %s "$f")
    echo "$f: ${sz}b event-stream=$has_es dispatcher=$has_ds applySnap=$has_apply"
  fi
done

echo ""
echo "=== App routes generati ==="
ls -la app/ide/ 2>/dev/null
ls -la app/ 2>/dev/null | head -10
