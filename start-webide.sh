#!/bin/bash
cd /home/administrator/ideai/apps/web-ide
export PORT=3000
export NODE_ENV=development
export BRAIN_URL=http://localhost:8001

# Avvia il server con restart automatico se crasha
while true; do
  echo "[$(date)] Avvia web-ide sulla porta 3000..."
  # In dev usiamo next dev (non richiede build .next).
  ./node_modules/.bin/next dev -H 0.0.0.0 -p 3000
  echo "[$(date)] Server terminato. Riavvio tra 3 secondi..."
  sleep 3
done
