#!/usr/bin/env bash
cd /home/administrator/ideai

echo "=== 1. Cancello tsconfig.tsbuildinfo (auto-gen) ==="
rm -f apps/web-ide/tsconfig.tsbuildinfo
git rm -f --cached apps/web-ide/tsconfig.tsbuildinfo 2>&1 | tail -3

echo ""
echo "=== 2. Conflitti in message-list.tsx (line ranges) ==="
grep -n '<<<<<<<\|=======\|>>>>>>>' apps/web-ide/components/chat/message-list.tsx | head -30

echo ""
echo "=== 3. Git status ==="
git status --short | head -20
