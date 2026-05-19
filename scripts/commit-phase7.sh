#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai
git add infra/docker/docker-compose.onprem.yml \
        scripts/onprem-preflight.sh \
        docs/onprem-dry-run-2026-05-19.md \
        scripts/commit-phase7.sh

git commit -m "$(cat <<'EOF'
chore(onprem): pre-flight script + dry-run report (Fase 7)

L'esecuzione completa del runbook `docs/migration-to-onprem.md` richiede
hardware production-grade (GPU NVIDIA >=40GB VRAM, 64GB RAM, 100GB SSD,
download ~60GB del modello Qwen2.5-Coder-32B). Non eseguibile sul dev
host WSL di questo branch.

Fix e tool aggiunti per supportare l'esecuzione futura:

1. **`infra/docker/docker-compose.onprem.yml`**: rimosso `version: "3.8"`
   obsoleto. `docker compose config --quiet` ora passa senza warning.

2. **`scripts/onprem-preflight.sh`**: validazione prerequisiti del sistema
   target PRIMA del deploy. Controlla:
   - CLI tool richiesti (docker, curl, python3, pg_isready)
   - docker daemon + docker compose v2
   - GPU NVIDIA + nvidia-container-toolkit (con test `docker --gpus all`)
   - RAM (>= 64GB raccomandato, >= 32GB warn)
   - Disco /var/lib/docker (>= 100GB)
   - File richiesti dal runbook
   - Sintassi compose
   - Chiavi presenti in .env.onprem (se file esiste)
   Exit 0 se 0 fail; permette di evitare il download di 60GB prima di
   scoprire un FAIL banale.

3. **`docs/onprem-dry-run-2026-05-19.md`**: report dell'esecuzione
   pre-flight sul WSL dev host. Documenta i FAIL attesi (RAM, pg_isready)
   e fornisce la roadmap per l'esecuzione end-to-end reale su target
   production:
   - 5 azioni operatore (preflight, .env.onprem, compose up, smoke,
     go-live checklist 9 item)
   - Tempo stimato: 1-2h compreso download modello
EOF
)"

git log -1 --stat
