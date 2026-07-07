---
id: 0028-user-manager-guaranteed
kind: adr
title: "Garanzia permanente del systemd --user manager (Nexus admin su WSL)"
slug: 0028-user-manager-guaranteed
tags:
  - architecture
  - systemd
  - wsl
  - reliability
  - sudo-manager
auto_generated: false
created_at: 2026-06-08T00:00:00Z
updated_at: 2026-06-08T00:00:00Z
nexus_meta_version: 1
---

# ADR 0028 — Garanzia permanente del systemd --user manager

> **Relativizzato da** [[0038-modello-servizi-progetto-multipiattaforma]]: la garanzia del manager `user@<uid>.service` resta valida come dettaglio del backend systemd/Linux. Su Windows non esiste un manager (`ManagerStatus::NotApplicable`) e questo modulo e' gated `#[cfg(not(windows))]` (inerte). Contenuto storico invariato.

> **Status**: accettato (implementato)
> **Decisori**: team Nexus
> **Trigger**: il manager `user@<UID>.service` risulta `inactive` dopo `wsl --shutdown` nonostante `Linger=yes`. Conseguenza: `systemctl --user` da "Connection refused", il `service_observer` diventa cieco e i servizi dei progetti non sono gestibili. Supera la decisione "non avviamo automaticamente il manager" di [ADR 0022](0022-systemd-user-bus-down-vs-zero-servizi.md).

## Contesto

I servizi dei progetti utente girano come unit `systemd --user` in `~/.config/systemd/user/` (scelta corretta, isolamento per-utente). Dipendono pero' dal manager `user@<UID>.service`. In WSL questo manager **non riparte deterministicamente al boot** anche con linger: manca il trigger di login PAM che su un sistema normale avvia `user@.service`. `systemd-logind` e' attivo, il linger persiste su disco (`/var/lib/systemd/linger/<user>`), ma il manager resta `dead` finche' non c'e' un login interattivo o un'azione esplicita.

ADR 0022 ha reso il sintomo VISIBILE (campo `manager_unavailable` nel pannello) e ha deliberatamente **scartato** l'auto-start, temendo una toppa (regola H): un auto-start ad ogni poll sarebbe stato "combattere il sintomo". Ha pero' previsto un "Livello 2 futuro" col Sudo Manager. Questo ADR realizza quel livello, aggiungendo la garanzia di boot che lo rende un fix di causa radice, non una toppa.

L'utente ha chiesto esplicitamente: "Nexus deve operare sulla sua macchina come admin e gestire i suoi processi in modo affidabile in maniera permanente".

## Decisione

Architettura a **2 livelli** (il watchdog periodico in-poll resta scartato).

### Livello 1 — Garanzia di boot (unit --system, autoritativa)

Nuova unit `/etc/systemd/system/nexus-user-manager.service` (`deploy/systemd/nexus-user-manager.service`), installata one-time da `deploy/install-user-manager.sh`:

- `Type=oneshot`, `RemainAfterExit=yes`, `After=systemd-logind.service`, `WantedBy=multi-user.target`.
- `ExecStartPre`: ricrea difensivamente `/run/user/<UID>` (bug noto user-runtime-dir).
- `ExecStart`: `loginctl enable-linger <user>` + `systemctl start user@<UID>.service`.

Gira come root ad **ogni boot** della distro (incluso dopo `wsl --shutdown`), PRIMA che mcp-core parta. Fornisce il trigger che in WSL manca. **Non dipende da DB, sudo-runner o mcp-core**: e' la garanzia reale, isolata e testabile a DB spento. L'UID non e' hardcoded nel repo: i placeholder `__NEXUS_ADMIN_USER__` / `__NEXUS_ADMIN_UID__` sono risolti dall'installer via `id -u`.

### Livello 2 — Cintura race-window (codice Nexus, niente loop)

`crates/mcp-core/src/project_workspace/user_manager.rs::ensure_user_manager(db)`, chiamata UNA volta all'avvio di mcp-core (`main.rs`, vicino a `port_gc`):

1. Probe via `wizard::systemd_user_available()` (punto unico, regola L — reso `pub(crate)`).
2. Se il bus e' giu' e `agent.user_manager.autostart_enabled=true`, risuscita il manager via `sudo_manager::execute(db, "user-manager-start")` (Sudo Manager, [ADR 0017](#)/mig 0289 — unico canale privilegiato).
3. Cooldown con **FLOOR hardcoded 60s non bypassabile** (anche se il setting DB e' 0), per non martellare root se il manager e' in crash-loop.
4. Re-probe + log; **mai panic**. Se ancora giu', il fallback detached del wizard copre (degradazione, non crash).

Copre la finestra in cui mcp-core parte prima che la oneshot completi e ogni restart di mcp-core. **Nessun loop tokio periodico**: la morte del bus a runtime e' rara e gia' segnalata dal `service_observer` (regola L, niente rate-limiting duplicato).

## Perche' questo NON e' la toppa temuta da ADR 0022

ADR 0022 temeva "un auto-start in loop ad ogni poll". Qui:
- la **garanzia** e' la unit --system al boot (causa radice: fornisce il trigger mancante), non un loop;
- il Livello 2 e' **una sola** invocazione all'avvio, con floor anti-martellamento;
- nessun privilegio nuovo: si riusa il Sudo Manager esistente con audit immutabile e allowlist (`systemctl` gia' in `PATH_ALLOWLIST`, `user@<UID>.service` valido per `ARG_SAFE_PATTERN`).

Sopravvive a `wsl --shutdown`, a deploy e a wipe + re-migrazione del DB.

## Sicurezza e isolamento (regola E)

- Il guard `has_system_systemctl` (`agent_tools/safety.rs`) resta **intatto**: l'agente non acquisisce alcun potere su systemd di sistema. I servizi di progetto restano `systemctl --user`.
- Il purpose `user-manager-start` agisce sul manager dell'UTENTE (infrastruttura Nexus), una sola unit `user@<UID>.service`, non su risorse di un progetto. Non tocca container `ideai-*` ne esegue docker.
- Estensione raccomandata (non bloccante): nel runner, per `category='service'`, validare l'unit contro `^user@[0-9]+\.service$` per blindare la superficie di `nexus_sudo_purposes`.

## Config (regola G)

- `settings.agent.user_manager.autostart_enabled` (default `true`) — governa solo il Livello 2; la unit --system resta attiva comunque.
- `settings.agent.user_manager.resurrection_cooldown_seconds` (default `120`, floor 60 nel codice).
- Purpose `user-manager-start` in `nexus_sudo_purposes` (mig 0369), command_template allineato all'UID reale dall'installer.

## Limiti onesti

- systemd in WSL non tiene viva la VM: la ripartenza avviene alla riapertura della distro (apertura terminale o task Windows al logon `wsl -d Ubuntu --exec /bin/true`, opzionale, fuori scope codice).
- Questo ADR garantisce il MANAGER UTENTE (che ospita i servizi di progetto). I servizi CORE Nexus (mcp-core, brain, gateway, web-ide) restano avviati dal loro meccanismo attuale; possono in futuro avere proprie unit --system con `After=docker.service`.

## Rollback

- Runtime senza root: `UPDATE settings SET value='false' WHERE key='agent.user_manager.autostart_enabled'` → Livello 2 no-op.
- Unit --system: `sudo systemctl disable --now nexus-user-manager.service && sudo rm /etc/systemd/system/nexus-user-manager.service && sudo systemctl daemon-reload`.
- Codice: revert del commit (modulo + chiamata main.rs + `pub(crate)`).
- DB: mig 0369 additiva (`DELETE FROM nexus_sudo_purposes WHERE name='user-manager-start'` + i due settings).

## Metriche di Done

- ✅ Migrazione 0369 (purpose + settings)
- ✅ `user_manager.rs` + chiamata in `main.rs` + `systemd_user_available` pub(crate)
- ✅ Unit `--system` + installer `install-user-manager.sh`
- ✅ Test cooldown-floor (3/3), `cargo check` + `clippy -D warnings` verdi
- ⬜ Esecuzione one-time `bash deploy/install-user-manager.sh` (root, lato utente)
- ⬜ Verifica post `wsl --shutdown`: `user@<UID>` active senza intervento

## Riferimenti

- [ADR 0022](0022-systemd-user-bus-down-vs-zero-servizi.md) — diagnosi bus-down (questo ADR ne realizza il "Livello 2")
- Sudo Manager: `db/migrations/0289_sudo_manager.sql`, `crates/nexus-sudo-runner/`, `crates/mcp-core/src/sudo_manager.rs`
- `crates/mcp-core/src/project_workspace/wizard.rs` (`systemd_user_available`, fallback detached)
- [ADR 0026](0026-punto-unico-de-duplicazione.md) — catalogo punti unici (gate systemd, canale sudo)
