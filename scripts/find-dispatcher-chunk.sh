#!/usr/bin/env bash
cd /home/administrator/ideai/apps/web-ide/.next/static/chunks

echo "=== Chunk con 'IdeShell' (anche minificato) ==="
for f in *.js app/**/*.js; do
  [ -f "$f" ] || continue
  if grep -q 'IdeShell\|ide-shell\|"AI Workspace"\|"Editor Workspace"' "$f" 2>/dev/null; then
    echo "  $f ($(stat -c %s "$f") bytes)"
  fi
done

echo ""
echo "=== Quel chunk contiene useProjectDispatcher? ==="
for f in $(grep -l 'IdeShell\|"AI Workspace"' *.js app/**/*.js 2>/dev/null); do
  echo "--- $f ---"
  for kw in useProjectDispatcher connectDispatcher EventSource project-dispatcher fetchSnapshot openStream applySnapshot; do
    c=$(grep -c "$kw" "$f" 2>/dev/null || echo 0)
    echo "  $kw: $c"
  done
done
