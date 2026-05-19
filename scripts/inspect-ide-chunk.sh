#!/usr/bin/env bash
cd /home/administrator/ideai/apps/web-ide/.next/static/chunks

echo "=== Chunks principali ==="
ls -la | grep -E '\.(js)$' | sort -k5 -n -r | head -10

echo ""
echo "=== Chunk app/ide/page contiene? ==="
F=$(ls app/ide/page-*.js 2>/dev/null | head -1)
echo "file: $F"
if [ -n "$F" ]; then
  for kw in useProjectDispatcher project-dispatcher connectDispatcher hooks/connection EventSource snapshot; do
    if grep -q "$kw" "$F" 2>/dev/null; then
      echo "  ✓ $kw presente"
    else
      echo "  ✗ $kw MANCANTE"
    fi
  done
fi

echo ""
echo "=== Cerca in TUTTI i chunks ==="
for kw in useProjectDispatcher connectDispatcher disconnectDispatcher openStream; do
  echo "--- $kw ---"
  grep -l "$kw" *.js app/**/*.js 2>/dev/null | head -3
done

echo ""
echo "=== ide-shell? ==="
grep -l "ide-shell\|IdeShell" *.js app/**/*.js 2>/dev/null | head -5
