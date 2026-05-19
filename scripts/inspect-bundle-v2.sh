#!/usr/bin/env bash
cd /home/administrator/ideai/apps/web-ide/.next/static/chunks

echo "=== Search keywords nel bundle (vedere quali sono presenti) ==="
for kw in useProjectDispatcher connectDispatcher openStream applySnapshot ProjectStore project-dispatcher snapshot fetchSnapshot EventSource; do
  count=$(grep -l "$kw" *.js 2>/dev/null | wc -l)
  echo "$kw: $count chunks"
done

echo ""
echo "=== Mtime chunks (verificare se freschi post-build) ==="
ls -lt *.js 2>/dev/null | head -10 | awk '{print $6,$7,$8,$9}'

echo ""
echo "=== File webpack manifest (richiede project-dispatcher?) ==="
grep -lE 'project-dispatcher' . *.js 2>/dev/null | head -5
