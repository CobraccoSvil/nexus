---
id: adr-0038-modello-servizi-progetto-multipiattaforma
kind: adr
title: "ADR 0038 - Modello unico multipiattaforma dei servizi di progetto (ServiceManager)"
slug: 0038-modello-servizi-progetto-multipiattaforma
tags:
  - adr
  - single-source-of-truth
  - regola-L
  - regola-M
  - servizi-progetto
  - windows
  - systemd
  - cross-platform
auto_generated: false
created_at: 2026-07-07T00:00:00Z
updated_at: 2026-07-07T00:00:00Z
nexus_meta_version: 1
---

# ADR 0038 - Modello unico multipiattaforma dei servizi di progetto (ServiceManager)

## Stato

Accepted - 2026-07-07 (implementato sul branch `claude/service-manager-punto-unico`, non ancora mergiato in `main`).

## Contesto

Il ciclo di vita dei "servizi di progetto" (dev server, api, `docker-compose`
registrati in un progetto Nexus) aveva DUE modelli di piattaforma incompatibili e
un dispatch sparso a macchia di leopardo.

- **Linux (produzione)**: i servizi sono unit `systemd --user`
  (`systemctl --user` / `journalctl --user` / file in `~/.config/systemd/user/`),
  con manager `user@<uid>.service` come dipendenza (vedi
  [[0022-systemd-user-bus-down-vs-zero-servizi]] e
  [[0028-user-manager-guaranteed]]).
- **Windows (ambiente di sviluppo canonico)**: non esiste `systemd`; i servizi sono
  righe in `agent_processes` con `kind='service'`, gestiti tramite spawn di
  processi, `taskkill` e ispezione delle porte via `Get-NetTCPConnection`.

Il dispatch di piattaforma `#[cfg(windows)]` / `#[cfg(not(windows))]` era duplicato
in oltre 8 file. Molte funzioni non erano gated affatto: su Windows facevano
silenziosamente no-op o, peggio, **mentivano** — emettevano gli eventi
`ServiceStarted` / `ServiceRestarted` senza aver realmente agito su alcun processo
(caso `restart_project_unit` nell'auto-remediation). Un audit ha verificato 22 bug
riconducibili a questa dispersione.

Questo violava due regole autoritative del `CLAUDE.md`:

- **Regola L** (punto unico di controllo): la stessa decisione — "com'e' fatto e
  come si governa un servizio di progetto su questa piattaforma" — era
  re-implementata in ogni call site invece di essere delegata a un unico modulo
  autoritativo.
- **Regola M** (stato tecnico da segnale strutturato, mai dal testo/assenza):
  l'esito di start/stop/restart veniva dedotto dall'assenza di errore o dal parsing
  di stderr (`contains("failed to connect to bus")`), non da un segnale strutturato
  codificato alla fonte. Da qui il "mentire": senza un flag esplicito che dicesse
  "ho agito davvero", l'evento veniva emesso comunque.

## Decisione

Introdurre un punto unico per il ciclo di vita dei servizi di progetto:
`crates/mcp-core/src/project_workspace/service_manager.rs`. Il modulo definisce un
vocabolario platform-neutral e istrada l'intera logica di piattaforma attraverso un
solo trait con due implementazioni per **composizione** (regola L, "composition over
inheritance").

### Trait `ServiceBackend` + tipi neutri

Un solo `trait ServiceBackend` (async via `#[async_trait]`) con operazioni
platform-neutral: `list`, `start`, `stop`, `restart`, `listening_ports`,
`manager_status`. I metodi ricevono un `ServiceContext<'_>` minimale (pool DB,
port registry opzionale, project id, slug di servizio, root del progetto), evitando
di dipendere dallo stato HTTP degli handler.

I tipi di dominio non contengono vocabolario systemd nei nomi pubblici:

- `ServiceState` — enum `running | starting | stopped | failed | unknown`. La sola
  conversione dagli stati testuali stabili di systemd (`ActiveState` / `SubState`)
  avviene in `ServiceState::normalize_from`, usata dal solo backend systemd; i
  chiamanti non vedono mai le stringhe systemd. Questo non e' parsing di prosa
  (regola M): mappa gli enum testuali stabili dell'API systemd.
- `ServiceEntry { id, label, state, main_pid, managed_by }` — una voce del pannello.
  `managed_by` distingue chi gestisce concretamente il servizio
  (`"windows"` / `"systemd"` / `"detached"` / `"docker-compose"`).
- `ServiceActionOutcome { acted: bool, message }` — esito di un'azione.
- `ManagerStatus` — `Available | Unavailable { hint } | NotApplicable`, che
  sostituisce l'euristica booleana `user_manager_unavailable` basata su
  `contains()` sullo stderr.
- `PortListener { port, pid, program }` — terna "chi ascolta su quale porta",
  condivisa con port_enforcer/cleanup.

### `acted: bool` come segnale strutturato (regola M)

`ServiceActionOutcome.acted` e' `true` SOLO se l'operazione ha realmente agito su un
processo o una unit reale (spawn / kill / restart effettivo). E' il campo su cui i
call site decidono se emettere l'evento `ServiceStarted / Stopped / Restarted`.
Questo chiude alla radice il "mentire" di `restart_project_unit`: l'evento non e'
piu' dedotto dall'assenza di errore, ma da un segnale esplicito codificato dal
backend che ha eseguito (o non eseguito) l'azione.

### Due backend per composizione, selezione unica `active()`

Due implementazioni del trait **avvolgono** i primitivi gia' esistenti (non li
riscrivono):

- `WindowsProcessBackend` — servizi come processi in `agent_processes`
  (`kind='service'`), avvolge i primitivi Windows di `services.rs` /
  `agent_processes.rs` / le porte in ascolto Windows.
- `SystemdUserBackend` — `systemctl --user` / `journalctl --user`, con la directory
  delle unit risolta da `user_systemd_dir()` (unica fonte del percorso
  `$HOME/.config/systemd/user`, chiude i default HOME divergenti `/home/...` vs
  `/root` sparsi nei vecchi call site).

La selezione del backend attivo avviene in **un solo punto**, `active()`, con un
type alias `ActiveBackend` risolto a compile-time via `#[cfg]`. Niente `dyn` /
`Box`: la piattaforma e' nota a compile-time e non serve dispatch dinamico.

### Convergenza dei call site

I call site che prima duplicavano il dispatch di piattaforma ora DELEGANO ad
`active()`:

- `restart_project_unit` (auto-remediation) — emette l'evento solo se `acted`;
- `detect_all_port_bindings` — riabilita il port_enforcer su Windows tramite il
  ramo `Get-NetTCPConnection` + risalita dell'albero dei processi Win32;
- `cleanup_project_ports`, `restart_all_project_services`;
- i tool agente `nexus_service_status` / `nexus_service_control`;
- `system_channel_events` (Console Debug);
- `mark_existing_services` (wizard) — chiude il doppio spawn;
- `cleanup_systemd_units`.

Il frontend e' stato de-systemd-izzato: il componente pannello dei servizi di
progetto usa copy neutro, senza terminologia specifica di systemd.

## Confine (solo servizi di progetto)

Il ServiceManager copre **esclusivamente** i servizi DI PROGETTO — quelli
registrati e governati dentro un progetto utente. I microservizi di
**infrastruttura Nexus** (mcp-core, gateway, admin-service, brain, ecc.) restano
gestiti dal `services_watchdog` e dal flusso di deploy (unit `systemd` in
produzione; WinSW / processi dev su Windows). Sono un dominio separato, fuori dallo
scope di questo ADR e di questo modulo (vedi
[[settings-keys#agent.watchdog.services]] per l'elenco dei microservizi monitorati
dal watchdog).

## Conseguenze

### Positive

- Un solo posto in cui rispondere alla domanda "com'e' un servizio di progetto su
  questa piattaforma": aggiungere una terza piattaforma o cambiare la mappatura
  degli stati si fa una volta sola, dentro il modulo.
- Fine del "mentire": nessun evento `Started/Stopped/Restarted` senza un'azione
  reale, perche' l'emissione e' gating su `acted` (regola M).
- Windows diventa un backend di prima classe: le funzioni che prima erano no-op
  ora agiscono davvero; il port_enforcer torna operativo su Windows.
- Nessun vocabolario systemd trapela nei tipi pubblici o nel frontend.

### Negative / costi

- Il modulo avvolge primitivi esistenti tuttora presenti in `services.rs` /
  `agent_processes.rs`: finche' la convergenza non e' totale, i primitivi
  sottostanti convivono col nuovo punto unico (costo tipico del pattern regola L,
  gia' descritto in [[0026-punto-unico-de-duplicazione]]).
- Il branch non e' ancora mergiato in `main`: fino al merge, lo stato di
  produzione (Linux/systemd) resta quello descritto da 0022/0028.

### Alternative considerate

- **Mantenere il dispatch `#[cfg]` per-funzione** e limitarsi ad aggiungere i rami
  Windows mancanti. Scartata: replica la macchia di leopardo e la regola M
  continuerebbe a essere violata (nessun segnale `acted` centralizzato). Aggiungere
  un requisito significherebbe di nuovo toccare "lo stesso `if` in piu' file"
  (trigger imperativo regola L: fermarsi e creare il punto unico).
- **Dispatch dinamico `Box<dyn ServiceBackend>`**. Scartata: la piattaforma e' nota
  a compile-time, `dyn`/`Box` aggiungerebbero allocazione e indirezione senza alcun
  guadagno; il type alias risolto da `#[cfg]` e' la scelta a costo zero.
- **Ereditarieta' di una base class comune ai due backend**. Non applicabile (Rust
  non ha ereditarieta' di classi) e comunque anti-pattern per riuso di codice: la
  scelta corretta e' `trait` + composizione (vedi tabella meccanismi in
  [[0026-punto-unico-de-duplicazione]]).

## Relazione con ADR precedenti

Questo ADR **relativizza** i due ADR che descrivevano il modello systemd come se
fosse l'unico:

- [[0022-systemd-user-bus-down-vs-zero-servizi]] — la distinzione "bus down vs zero
  servizi" resta valida, ma diventa un dettaglio interno del `SystemdUserBackend`:
  il caso e' ora modellato dal segnale strutturato `ManagerStatus::Unavailable { hint }`
  invece dell'euristica `contains()` sullo stderr.
- [[0028-user-manager-guaranteed]] — la garanzia del manager `user@<uid>.service`
  resta valida come dettaglio del backend systemd/Linux. Su Windows il concetto di
  "manager" non esiste (`ManagerStatus::NotApplicable`) e il modulo
  `user_manager` (systemd `--user`) e la sua chiamata di boot sono gated
  `#[cfg(not(windows))]`, quindi inerti su Windows.

Entrambi restano validi per la parte Linux; il presente ADR li inquadra come
specializzazioni del backend systemd all'interno del modello multipiattaforma.

## Riferimenti

- `crates/mcp-core/src/project_workspace/service_manager.rs` — punto unico (trait
  `ServiceBackend`, `active()`, i due backend, i tipi neutri, `user_systemd_dir`).
- Call site che delegano: `crates/mcp-core/src/project_workspace/services.rs`,
  `crates/mcp-core/src/project_workspace/wizard.rs`,
  `crates/mcp-core/src/project_workspace/logs.rs`,
  `crates/mcp-core/src/projects/cleanup.rs`,
  `crates/mcp-core/src/nexus_builtin/services.rs`,
  `crates/mcp-core/src/security/port_enforcer.rs`,
  `crates/mcp-core/src/project_workspace/service_observer_remediation.rs`.
- [[0026-punto-unico-de-duplicazione]] — catalogo dei punti unici (voce "Ciclo di
  vita servizi di progetto") e meccanismo di centralizzazione.
- [[0022-systemd-user-bus-down-vs-zero-servizi]], [[0028-user-manager-guaranteed]] —
  backend systemd/Linux.
- [[isolamento-progetti]] — i servizi di progetto tra le risorse per-progetto.
- Branch: `claude/service-manager-punto-unico` (non mergiato).
