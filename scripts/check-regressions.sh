#!/usr/bin/env bash
# Diagnosi regressioni: cerca errori nei log dei servizi runtime.

echo "═══ Branch corrente ═══"
cd /home/administrator/ideai
echo "branch:  $(git branch --show-current)"
echo "HEAD:    $(git rev-parse --short HEAD)"
echo "commit:  $(git log -1 --pretty=format:'%s')"
echo ""

echo "═══ Health endpoint ═══"
for p in 3000 4000 4010 4020 4030 4040 4050 4055 4060; do
  code=$(curl -sS -o /dev/null -m 5 -w "%{http_code}" "http://localhost:$p/health" 2>/dev/null || echo "TIMEOUT")
  printf "  :%-5s  %s\n" "$p" "$code"
done
echo ""

echo "═══ Errori recenti nei log servizi (ultimi 5 min) ═══"
for log in /tmp/nexus-*.log; do
  if [ -f "$log" ]; then
    # Cerca ERROR/panic/Error: nelle ultime 200 righe
    errors=$(tail -200 "$log" 2>/dev/null | grep -iE 'ERROR|panic|Error:|FATAL|traceback|exception' | head -3)
    if [ -n "$errors" ]; then
      echo ""
      echo "── $log ──"
      echo "$errors"
    fi
  fi
done
echo ""

echo "═══ Diff sul file environment-panel (ultima modifica Fase 5) ═══"
git diff HEAD~1 HEAD -- apps/web-ide/components/settings/environment-panel.tsx | head -40
