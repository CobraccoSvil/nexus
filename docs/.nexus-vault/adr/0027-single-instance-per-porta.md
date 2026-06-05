---
id: adr-0027-single-instance-per-porta
kind: adr
title: "ADR 0027 - Single-instance per porta (no istanze fantasma su restart)"
slug: 0027-single-instance-per-porta
tags:
  - adr
  - deploy
  - reliability
  - single-instance
  - so-reuseport
nexus_meta_version: 1
---

# ADR 0027 - Single-instance per porta (no istanze fantasma su restart)

## Stato

Implementato.

## Contesto

Incidente ricorrente (vedi memoria `reference_mcp_core_shutdown_hang`,
`reference_brain_multi_processi_reuseport`): dopo i restart restavano istanze
VECCHIE in ascolto sulla stessa porta dei servizi nuovi, e le richieste venivano
servite a INTERMITTENZA dal binario vecchio o nuovo (stesso URL -> a volte route
corretta, a volte route deprecata). Sintomo subdolo: bug che "ogni tanto
ricompaiono" dopo un fix gia' deployato, perche' una frazione del traffico
colpisce il processo vecchio.

Tre cause concomitanti:

1. **brain gRPC :50051** — `grpc.so_reuseport` e' ATTIVO DI DEFAULT (=1) in
   Python gRPC. Due processi brain potevano bindare la STESSA porta e il kernel
   distribuiva le connessioni a caso tra loro.
2. **Deploy con race** — `stop` faceva `pkill + sleep 1` e poi avviava subito il
   nuovo, senza attendere che il vecchio fosse morto e la porta libera; un
   eventuale `EADDRINUSE` allo start era silenzioso.
3. **Nessun single-instance guard** nel codice: niente impediva strutturalmente
   la coesistenza di due processi sullo stesso servizio.

## Decisione

Sistema a 3 leve complementari (regola L: un solo punto di verita' per "istanza
attiva" = un lock per porta).

1. **SO_REUSEPORT disattivato sul gRPC** (`neural_service.py`):
   `options=[("grpc.so_reuseport", 0), ...]`. Un secondo processo che tenta il
   bind di :50051 fallisce invece di coesistere.

2. **Single-instance lock (flock esclusivo) all'avvio**, indipendente da deploy
   e systemd:
   - mcp-core (`main.rs`, prima del bind): `flock(LOCK_EX|LOCK_NB)` su
     `/tmp/nexus-mcp-core-<port>.lock`; se occupato, `exit(1)` con messaggio
     chiaro. Il File viene `mem::forget` per tenere il lock per tutta la vita
     del processo.
   - brain (`grpc_server/main.py`, prima di bindare le porte):
     `fcntl.flock` su `/tmp/nexus-brain-<grpc_port>.lock`; se occupato,
     `sys.exit(1)`. Il fd resta aperto a livello modulo.
   - Path override via env `NEXUS_MCP_CORE_LOCK` / `NEXUS_BRAIN_LOCK`.

3. **Deploy stop affidabile** (`deploy-local.sh`, helper `_stop_pattern`):
   SIGTERM -> poll d'uscita (~15s) -> SIGKILL se non muore. Usato da
   `stop_service` e `stop_brain`. Elimina la race "nuovo avviato mentre il
   vecchio e' ancora sulla porta".

## Conseguenze

Positive: e' strutturalmente impossibile avere due istanze sulla stessa porta;
il secondo avvio esce subito con un messaggio diagnostico invece di servire
traffico dal codice vecchio. La fonte dell'intermittenza dei "bug che
riappaiono" e' rimossa. Verificato: `flock -n` sul lock attivo fallisce; una
sola istanza per :4000/:50051/:8001 dopo il deploy.

Neutre: i lock file vivono in `/tmp` (rimossi al boot della macchina; il flock e'
comunque rilasciato dal kernel alla morte del processo, quindi un file stantio
non blocca un avvio legittimo).

## Riferimenti

- `brain/grpc_server/neural_service.py` (so_reuseport=0),
  `brain/grpc_server/main.py` (`_acquire_single_instance_lock`),
  `crates/mcp-core/src/main.rs` (flock pre-bind),
  `deploy/deploy-local.sh` (`_stop_pattern`).
- Memoria: `reference_mcp_core_shutdown_hang`,
  `reference_brain_multi_processi_reuseport`.
