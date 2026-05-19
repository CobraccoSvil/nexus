# On-prem migration — dry-run su host di sviluppo (2026-05-19)

Esecuzione del pre-flight check (`scripts/onprem-preflight.sh`) sul WSL dev
host del branch `chore/backlog-closure`. Lo script valida i prerequisiti del
sistema PRIMA di scaricare il modello vLLM (~60GB).

## Output dry-run

  [ OK ]   docker disponibile (/usr/bin/docker)
  [ OK ]   curl disponibile (/usr/bin/curl)
  [ OK ]   python3 disponibile (/usr/bin/python3)
  [FAIL]  pg_isready mancante — il runbook on-prem lo richiede
  [ OK ]   docker daemon raggiungibile
  [ OK ]   docker compose v2 disponibile (5.1.3)
  [WARN]  nvidia-smi mancante — usa profilo cpu-test (vllm-cpu) per validare senza GPU
  [FAIL]  RAM: 6GB — insufficiente per vLLM 32B
  [ OK ]   Disco /var/lib/docker: 860GB liberi (OK per cache vLLM)
  [ OK ]   file presente: .env.example
  [ OK ]   file presente: infra/docker/docker-compose.onprem.yml
  [ OK ]   file presente: infra/sql/init-schemas.sql
  [ OK ]   file presente: infra/sql/rls-policies.sql
  [ OK ]   file presente: scripts/onprem-smoke.sh
  [ OK ]   docker-compose.onprem.yml sintatticamente valido
  [WARN]  .env.onprem non trovato — creare da .env.example prima del deploy

  Pre-flight: 12 OK, 2 warning, 2 fail

## Interpretazione

I 2 FAIL su WSL dev host sono attesi e accettabili:

1. **`pg_isready` mancante**: il dev host non ha postgres-client installato
   (i container li portano dentro). Su un server production target serve
   `apt install postgresql-client` per usare lo smoke test esterno.
2. **RAM 6GB**: WSL dev ha allocazione conservativa. Production target
   richiede 64GB (vLLM 32B + workload concorrente).

I 2 WARN sono informativi:

3. **nvidia-smi mancante**: WSL non ha GPU passthrough configurato. Per
   testare senza GPU usare profile `cpu-test` (`vllm-cpu` service con
   modello 7B su CPU, port 8001).
4. **`.env.onprem` non creato**: normale — viene creato dall'operatore al
   momento del deploy reale (vedi §1.2 del runbook).

## Fix introdotti

- Rimosso `version: "3.8"` obsoleto dal `docker-compose.onprem.yml`
  (Compose Spec v2 lo segnala come ignorato).
- Validazione `docker compose ... config --quiet`: passa senza warning.

## Cosa serve per esecuzione end-to-end reale

Lista delle azioni che il branch NON puo' completare in dev WSL (richiedono
target environment production-grade):

1. **Server fisico/cloud con GPU NVIDIA ≥ 40GB VRAM** (A100, H100, RTX 6000 Ada).
2. **`apt install postgresql-client nvidia-container-toolkit`**.
3. **Token HuggingFace** con accesso al modello Qwen2.5-Coder-32B-Instruct.
4. **Banda + storage**: ~60GB download iniziale per il modello.
5. **Esecuzione completa**: docker compose up + scarica modello + healthcheck +
   smoke test + verifica RLS + verifica `/providers` solo `vllm`.

Tempo stimato dall'operatore (su hardware adeguato): ~1-2h includendo
download del modello.

## Roadmap follow-up

Quando l'utente avra' un target on-prem disponibile:

1. Lanciare `./scripts/onprem-preflight.sh` (validazione prerequisiti — deve
   passare 16/16 OK su production target).
2. Creare `.env.onprem` da `.env.example` (vedi `docs/migration-to-onprem.md §1.2`).
3. `docker compose -f infra/docker/docker-compose.onprem.yml --env-file .env.onprem up -d`.
4. Attendere ~30-60min per download vLLM.
5. `./scripts/onprem-smoke.sh` deve chiudere "Smoke test SUPERATO (0 failure)".
6. Spuntare i 9 item della "Checklist go-live onprem" in fondo al runbook.
