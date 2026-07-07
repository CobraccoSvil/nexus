---
id: 0022-systemd-user-bus-down-vs-zero-servizi
kind: adr
title: "Distinguere bus systemd utente irraggiungibile da zero servizi configurati"
slug: 0022-systemd-user-bus-down-vs-zero-servizi
tags:
  - architecture
  - systemd
  - wsl
  - diagnostics
  - ux
auto_generated: false
created_at: 2026-06-04T20:30:00Z
updated_at: 2026-07-02T00:00:00Z
nexus_meta_version: 1
---

# ADR 0022 — Bus systemd utente irraggiungibile vs zero servizi

> **Relativizzato da** [[0038-modello-servizi-progetto-multipiattaforma]]: la distinzione "bus down vs zero servizi" resta valida ma diventa un dettaglio interno del backend systemd/Linux, modellata dal segnale strutturato `ManagerStatus::Unavailable { hint }` invece dell'euristica `contains()` sullo stderr. Contenuto storico invariato.

> **Status**: implementato (verificato 2026-07-02)
> **Aggiornamento 2026-07-02 (as-built)**: backend `manager_unavailable` in `crates/mcp-core/src/project_workspace/services.rs` + frontend `systemd-services-section.tsx`; adattato a Windows (`manager_mode` = "windows").
> **Decisori**: team Nexus
> **Trigger**: incident del 04/06/2026 su Beauty-Book. Il pannello "Run & Debug" mostra "Nessun servizio trovato con prefisso beauty-book-" anche se il file `~/.config/systemd/user/beauty-book-dev.service` esiste con `Linger=yes`.

## Contesto

Il pannello "Servizi systemd persistenti" elenca i servizi via `systemctl --user list-units '{slug}-*'` (`crates/mcp-core/src/project_workspace/services.rs`). La scelta dello scope `--user` e' corretta: i servizi dei progetti vivono in `~/.config/systemd/user/`.

Il problema: quando il manager systemd utente (`user@<uid>.service`) e' **inactive** — comune in WSL e nei container — `systemctl --user` esce con codice != 0 e stderr "Failed to connect to bus: Connection refused", con stdout vuoto. Il codice precedente non controllava `status.success()` ne lo stderr: trattava stdout vuoto come "0 servizi" e mostrava "Nessun servizio trovato".

### Diagnosi dell'incident

- File servizio: `~/.config/systemd/user/beauty-book-dev.service` → **esiste**
- `loginctl` linger administrator → **yes**
- systemd PID 1 → attivo
- `user@1000.service` → **inactive** (era partito al boot 09:26, uscito 09:44 perche' il linger fu abilitato dopo)
- `/run/user/1000/bus` → "Connection refused"
- Risultato: `systemctl --user` fallisce, Nexus mostra "Nessun servizio trovato"

Due stati opposti collassati in un unico messaggio fuorviante:

| Stato reale | Messaggio mostrato (bug) | Messaggio corretto |
|---|---|---|
| Zero file `.service` configurati | "Nessun servizio trovato" | "Nessun servizio trovato" (giusto) |
| Bus systemd utente giu' | "Nessun servizio trovato" (**sbagliato**) | "Manager systemd utente non attivo: avvialo o riavvia WSL" |

## Decisione

Distinguere i due casi in backend e propagarli al frontend con un campo esplicito.

### Backend (`project_workspace/services.rs`) — implementato

1. Helper `user_manager_unavailable(output)` che ritorna `true` quando `systemctl --user` fallisce con stderr contenente `failed to connect to bus` / `connection refused` / `failed to get d-bus connection` / `refusing to operate`.
2. In `get_project_services_status`, se il manager e' giu', ritornare:
   ```json
   { "services": [], "slug": "...", "manager_unavailable": true, "manager_hint": "..." }
   ```
3. Nel caso normale, ritornare sempre `"manager_unavailable": false` (contratto stabile per il frontend).

`manager_hint` (costante `USER_MANAGER_HINT`): suggerimento operativo con `sudo systemctl start user@$(id -u)` (niente uid hardcoded, la shell risolve `$(id -u)`) o `wsl --shutdown`.

### Frontend (`systemd-services-section.tsx`) — da implementare

Quando `managerUnavailable === true`, il blocco `services.length === 0` mostra un banner diagnostico (warning, non "vuoto"):

> ⚠️ Manager systemd utente non attivo — impossibile elencare i servizi. {manager_hint}

invece di "Nessun servizio trovato con prefisso ...". Il pulsante "+ Configura" resta, ma il messaggio chiarisce che il problema non e' l'assenza di servizi.

## Perche' non avviamo automaticamente il manager

Tentazione: far avviare a Nexus `systemctl start user@<uid>` quando rileva il bus giu'. Scartato per ora:
- richiede privilegi root (Sudo Manager, ADR 0017) — overhead per un problema ambientale
- in WSL il manager puo' ri-uscire subito (race con linger): un auto-start in loop sarebbe una toppa (regola H)
- la causa radice e' ambientale (WSL non persiste il manager): il fix corretto e' che l'utente abiliti linger + riavvii WSL una volta, non che Nexus combatta il sintomo ad ogni poll

Possibile evoluzione futura (Livello 2): pulsante "Avvia manager utente" che usa il Sudo Manager per un singolo `systemctl start user@<uid>`, mostrato solo quando `manager_unavailable=true`. Fuori scope qui.

## Metriche di Done

- ✅ Backend: `user_manager_unavailable` + campo `manager_unavailable`/`manager_hint` (fatto, `cargo check` verde)
- ⬜ Frontend: banner diagnostico quando `manager_unavailable=true`
- ⬜ `api-client.ts`: tipo risposta esteso con `manager_unavailable?`, `manager_hint?`
- ⬜ Test: simulare risposta `manager_unavailable=true` → UI mostra banner, non "vuoto"
- ⬜ `pnpm verify` verde

## Riferimenti

- `crates/mcp-core/src/project_workspace/services.rs` (`get_project_services_status`)
- `apps/web-ide/components/panels/run/systemd-services-section.tsx`
- Incident Beauty-Book 04/06/2026 — `user@1000.service` inactive in WSL
