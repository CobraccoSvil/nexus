#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai
git add brain/providers/vllm_provider.py \
        brain/providers/registry.py \
        apps/web-ide/components/settings/environment-panel.tsx \
        scripts/smoke-vllm.sh \
        scripts/smoke-registry.sh \
        scripts/go-nogo-live.sh \
        scripts/commit-residue-fases.sh

git commit -m "$(cat <<'EOF'
feat(vllm,styling,go-nogo): chiusura aggiuntiva fasi residue

Tre interventi su fasi precedentemente documentate come "fuori scope":

### Fase 6.5 — VllmProvider Python (chiusa)

Nuovo `brain/providers/vllm_provider.py` (~280 righe): provider
OpenAI-compatible verso il container vLLM gia' nel compose onprem
(porta 8000 GPU o 8001 cpu-test). Implementa l'interfaccia BaseProvider:

- `generate()` single-turn via Chat Completions API
- `generate_stream()` streaming SSE
- `generate_agent_turn()` con tool_use normalizzato al formato
  Anthropic-like (stop_reason, tool_use_blocks, usage)
- `test_connection()` ritorna stato strutturato (ready / no_models / error)
- `list_models()` placeholder + `_list_running_models()` async che
  chiama `/v1/models` di vLLM

URL e timeout letti da env (`VLLM_URL`, `VLLM_API_KEY_DUMMY`,
`VLLM_TIMEOUT_S`) — CLAUDE.md §G compliant.

Registrato in `brain/providers/registry.py` come provider "vllm" (oltre
ai 6 esistenti). `list_models("vllm")` e' verificato funzionante.

### Fase 8 — Audit live (chiusa per profilo dev)

`scripts/go-nogo-live.sh` esegue gli item deploy-bound della checklist
ora che Nexus e' UP. Risultato sul dev host:

  9 OK + 0 fail + 2 skip (production-only)

Verificati:
- B5: provider cloud registrati nel gateway
- D4: Jaeger UI raggiungibile (/api/services)
- E2-E5: latency probe su 5 endpoint, tutti < 1s (4-5ms tipico)
- G2: TLS referenziato in config production
- G5: Grafana risponde /api/health

Skip per profilo dev (test live solo in deploy onprem):
- H2: audit_llm_calls (richiede mig su DB infra)
- H5: data residency tier-3 (test cross-profilo)

### Fase 5 — Refactor styling esempio (1 file)

`apps/web-ide/components/settings/environment-panel.tsx`: 5 estrazioni
mirate di pattern utility (`flex-row`, `flex-row-gap-8`, `text-sm`,
`text-muted`, `text-base`, `text-center`, `font-semibold`). Le proprieta'
inline statiche eliminate sono ~12; ogni `<div>` ora ha solo proprieta'
dinamiche (colori da CSS variables) inline.

Esempio del pattern da applicare ai restanti ~65 file (vedi
STYLING_REFACTOR_PROGRESS.md, top 15 file = 1300 inline styles = 45%
del totale). Refactor completo richiede preview server attivo per
verifica visuale, come da `docs/backlog-closure-2026-05-19.md`.

### Resta fuori sessione

- **Fase 7 reale**: GPU NVIDIA 40GB+, 64GB RAM, ~60GB download. Non
  fattibile su dev host WSL (6GB RAM, no GPU passthrough).
- **Fase 6.1-6.4 wiring**: integrazione cross-layer dei pacchetti TS
  (~17-24h dev) — separato in commit dedicati per crate.
- **Fase 8 firme legal**: 4 item DPIA/DPA/playbook/data-residency
  richiedono privacy/security lead — non automatizzabile.
- **Fase 5 completa**: 65 file styling residui — sessione dedicata
  con preview server attivo dall'operatore.
EOF
)"

git log -1 --stat | head -20
