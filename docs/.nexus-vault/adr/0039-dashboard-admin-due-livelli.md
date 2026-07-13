---
id: adr-0039-dashboard-admin-due-livelli
kind: adr
title: "ADR 0039 - Dashboard admin a due livelli (uso quotidiano vs configurazione avanzata)"
slug: 0039-dashboard-admin-due-livelli
tags:
  - adr
  - single-source-of-truth
  - regola-L
  - regola-G
  - admin
  - settings
  - frontend
  - ux
auto_generated: false
created_at: 2026-07-13T00:00:00Z
updated_at: 2026-07-13T00:00:00Z
nexus_meta_version: 1
---

# ADR 0039 - Dashboard admin a due livelli (uso quotidiano vs configurazione avanzata)

## Stato

Accepted - 2026-07-13 (Fase 1 implementata nel commit `6ed7405e`; Fasi 2 e 3
proposte, da approvare prima dell'implementazione).

## Contesto

L'area amministrativa di Nexus (`/admin`) espone la configurazione della
piattaforma direttamente sulla tabella `settings`. Un'analisi sul DB live e sul
codice ha rilevato che l'esperienza era piatta e in parte rotta.

- **Volume e disomogeneita' delle chiavi**: 660 chiavi `settings` distribuite su
  27 categorie live. Le categorie sono fortemente sbilanciate — `agent` (297),
  `orchestrator` (97), `providers` (52), `routing` (48), `general` (40),
  `infrastructure` (22), a scendere. 47 chiavi non hanno alcuna descrizione.
- **Titoli categoria rotti (violazione regola L gia' in essere)**: solo 13
  categorie avevano una label; 19 pagine mostravano il titolo grezzo `cat.X`
  perche' rese con `t("cat.X")` senza fallback, mentre i dizionari `it/en/es`
  contenevano solo 9 chiavi `cat.*`. La sidebar e il titolo pagina divergevano —
  due liste di label parallele invece di un punto unico.
- **Nessuna separazione per profondita'**: circa 30 categorie erano rese
  dall'editor generico chiave/valore flat. Lo stesso trattamento veniva riservato
  a una chiave quotidiana come `default_provider` e a una soglia interna anti-OOM
  da 512 MB: nessuna distinzione tra cio' che l'admin tocca ogni giorno e cio' che
  richiede comprensione dei meccanismi interni.
- **Home vuota**: la landing `/admin` mostrava solo "Seleziona una categoria",
  senza alcuna vista di stato operativo.
- **Documentazione ricca ma scollegata**: `docs/.nexus-vault/api/settings-keys.md`
  esiste (auto-generato dal DB, ~730 righe) ma e' STALE — descrive 36 categorie
  storiche contro le 27 live, perche' rigenerato ad-hoc senza un generatore in
  codice. La colonna `settings.description` era resa come testo muted, senza
  esempi, tooltip o link alla documentazione.
- **Ruoli e preferenze mal collocati**: i ruoli sono solo `viewer|editor|admin`;
  non esiste una pagina di preferenze per utenti non-admin. Tema e lingua vivevano
  sotto `/admin` ma in `localStorage` (sono preferenze per-browser, non
  configurazione di piattaforma). Esiste gia' una tabella `user_profiles`
  per-utente non sfruttata a questo fine.
- **Duplicazioni e pagine orfane**: pagina `orchestrator` piu' categoria
  `orchestrator`; pagina `billing` piu' categoria omonima; `project-database`
  orfana (non raggiungibile dal menu); `nexus-docs` legacy convivente con `kb`;
  la categoria `general` (40 chiavi) e' di fatto un cestino residuo, gia' oggetto
  di una normalizzazione precedente (mig 0408).

## Decisione

Adottare una **tassonomia a due livelli** per l'intera area admin: `daily` (uso
quotidiano) e `advanced` (configurazione profonda). Il livello e' un criterio di
**ordinamento e filtro**, mai di visibilita': l'explorer mostra sempre tutte le
chiavi.

### Mapping dei quattro ambiti richiesti sulla matrice 2x2

L'utente ha chiesto quattro insiemi: impostazioni globali di piattaforma,
impostazioni utente profonde, prompt utente, prompt profondi. Su Nexus questi
quattro insiemi sono la combinazione di due assi ortogonali — il **dominio**
(configurazione di piattaforma vs prompt/template) e la **profondita'**
(quotidiano vs core interno):

| Ambito richiesto | Dominio | Profondita' | Reso su Nexus |
|---|---|---|---|
| Globali piattaforma | configurazione | daily | Home `/admin` + gruppo "Uso quotidiano" della sidebar |
| Utente profondi | configurazione | advanced | Gruppo "Configurazione avanzata" (collassato) + explorer |
| Prompt utente | prompt/template | daily | Categorie prompt in uso quotidiano (routing, behavior) |
| Prompt profondi | prompt/template | advanced | Categoria `prompt_templates` e affini (advanced) |

### Criterio operativo nel DB (regola G): classificazione per-chiave

La classificazione autoritativa vive nel DB, non nel codice: colonna
`settings.ui_level` con dominio `('daily'|'advanced')` e DEFAULT `'advanced'`
(fail-safe). La granularita' e' **per-chiave**, non per-categoria, perche' le
categorie sono miste: `providers` contiene sia `api_key` (quotidiana) sia le
soglie del circuit breaker (profonde).

Regole decisionali per assegnare il livello:

- **daily** = operazione ricorrente dell'admin senza bisogno di conoscere i
  meccanismi interni: ruotare una API key, abilitare un provider o un modello,
  scegliere il behavior mode, impostare un budget, assegnare il provider per
  intent.
- **advanced** = richiede comprensione del funzionamento interno: TTL, timeout,
  retry, cooldown, soglie, cap, e in generale i pattern `agent.*` /
  `orchestrator.*`.
- **`is_secret` NON implica `advanced`**: le API key sono segrete ma quotidiane.

Stima: circa 45-60 chiavi `daily` su 660.

Nella Fase 1 la classificazione e' provvisoriamente per-categoria nel frontend
(`levelForCategory` in `settings-categories.ts`); la Fase 2 la sposta nel DB
per-chiave come previsto dalla regola G.

## Fase 1 - Implementata (commit `6ed7405e`)

Interventi completati, senza alcuna migrazione:

- **Punto unico label (regola L)**: `labelForCategory` in
  `apps/web-ide/lib/settings-categories.ts`, con `KNOWN_CATEGORY_META` completo
  (27 categorie, ciascuna con `label` e `level`). Sidebar e titolo pagina leggono
  la stessa funzione: i 19 titoli grezzi `cat.X` sono spariti.
- **Sidebar a due livelli**: `levelForCategory` divide le voci in "Uso
  quotidiano" e nel gruppo "Configurazione avanzata" collassato di default.
- **Home come dashboard**: `app/admin/page.tsx` mostra widget di stato costruiti
  su endpoint gia' esistenti — stato provider (`GET /api/gateway/providers`),
  budget (`GET /api/admin/providers/budget`), scorciatoie operative e reload
  matrice (`POST /api/gateway/reload`).
- **Pulizia menu**: `nexus-docs` (legacy) rimosso dalla navigazione.

File coinvolti: `apps/web-ide/lib/settings-categories.ts`,
`apps/web-ide/components/settings/settings-panel.tsx`,
`apps/web-ide/components/admin-sidebar.tsx`, `apps/web-ide/app/admin/page.tsx`.
Zero migrazioni.

## Fase 2 - Proposta (classificazione nel DB + explorer + generatore doc)

- **Migrazione** (prossimo numero libero, indicativamente ~`0574`):
  - colonna `settings.ui_level` (`daily|advanced`, DEFAULT `advanced`) con
    backfill esplicito delle circa 50 chiavi `daily`;
  - colonna `settings.docs_behavior TEXT DEFAULT ''`, documentazione del
    "comportamento osservabile" nel formato "Se alzi/attivi X, Nexus ..."
    (esempio prioritario: `agent.context.auto_compact_ratio`);
  - sanatoria delle 47 descrizioni vuote;
  - normalizzazione della categoria `general` seguendo il pattern della mig 0408
    (`agent.*` -> `agent`, `gateway.retry.*` -> `gateway`, `orchestrator.*` ->
    `orchestrator`, `dlp_presidio_*` -> `security`).
- **DTO e API**: estendere il DTO `Setting`
  (`crates/nexus-types/src/settings_dto.rs`) e `list_categories`
  (`crates/mcp-core/src/settings.rs`) con `daily_count`.
- **Pagina `/admin/advanced` (explorer)**: albero delle categorie con conteggi e
  drill-down per prefisso (i 297 `agent.*` per sotto-prefisso derivato DALLA
  CHIAVE, nessun mapping hardcoded), ricerca full-text su chiave + descrizione,
  filtro per livello; riusa la card generica esistente. La route
  `/admin/settings/[category]` resta come deep-link sullo stesso componente.
- **Pannello "Dettagli" per-chiave**: descrizione completa + `docs_behavior` +
  categoria + livello + link "Documentazione categoria" verso
  `/admin/kb?doc=settings-keys` (`KnowledgeWorkspace` ha gia' `initialDocId`;
  manca solo la lettura del `searchParam` in `app/admin/kb/page.tsx`).
- **Generatore deterministico**: `cargo xtask gen-settings-doc`, accanto a
  `crates/xtask/src/audit_settings.rs` (che ha gia' la connessione al DB),
  rigenera `settings-keys.md` dal DB (chiude lo stale) e aggiunge una guardia di
  audit WARN "chiave senza descrizione" con logica ratchet.

## Fase 3 - Proposta (menu utente + consolidamento + backfill esteso)

- **Menu utente**: spostare tema, lingua e profili nel popup `UserHeader`, fuori
  da `/admin` (sono preferenze per-browser); eliminare le pagine `appearance` e
  `language` (prima redirect, poi rimozione).
- **Consolidamento**: eliminare `/admin/nexus-docs` (legacy); mergiare
  `/admin/project-database` in `/admin/nexus-database`; rinominare "Orchestrator"
  in "Piani Orchestrator" per distinguere la pagina dalla categoria settings
  omonima.
- **Documentazione**: backfill esteso di `docs_behavior`; attivare la guardia di
  audit sulla descrizione vuota.

## Roadmap a fasi

| Fase | Ambito | Effort | Stato | File / artefatti principali |
|---|---|---|---|---|
| 1 | Punto unico label, sidebar 2 livelli, home dashboard | S | Fatta (`6ed7405e`) | `settings-categories.ts`, `settings-panel.tsx`, `admin-sidebar.tsx`, `app/admin/page.tsx` |
| 2 | `ui_level`/`docs_behavior` nel DB, explorer, generatore doc | M-L | Proposta | mig ~`0576` (0575 usata dal catalog sync T5), `settings_dto.rs`, `mcp-core/src/settings.rs`, `app/admin/advanced`, `app/admin/kb/page.tsx`, `crates/xtask/src/audit_settings.rs` |
| 3 | Menu utente preferenze, consolidamento pagine, backfill esteso | M | Proposta | `UserHeader`, rimozione `appearance`/`language`/`nexus-docs`, merge `project-database` |

## Conseguenze

### Positive

- Un solo punto di verita' per le label di categoria (regola L): sidebar e titolo
  non possono piu' divergere e i titoli grezzi `cat.X` sono impossibili per
  costruzione.
- Separazione netta tra cio' che l'admin usa ogni giorno e la configurazione
  profonda, senza nascondere nulla: il livello ordina e filtra, la home espone lo
  stato operativo invece di una pagina vuota.
- Nella Fase 2 la classificazione e la documentazione vivono nel DB (regola G),
  con un generatore che chiude lo stale di `settings-keys.md` alla fonte invece di
  rigenerazioni ad-hoc.

### Negative / costi

- Fino alla Fase 2 la classificazione del livello e' duplicata nel frontend
  (per-categoria) rispetto alla destinazione finale (per-chiave nel DB): e' un
  ponte temporaneo, non lo stato finale.
- La ri-categorizzazione di `general` tocca dati esistenti: richiede attenzione ai
  lettori che filtrano per categoria (vedi rischio sotto).

### Rischi e mitigazioni

- **Ri-categorizzazione `general`**: prima della migrazione verificare che nessun
  lettore usi `WHERE category='general'` (controllare `CATEGORY_BULK_READERS` in
  `crates/xtask/src/audit_settings.rs`). Dopo la migrazione rieseguire
  `audit-settings --gate` per riallineare la baseline.
- **Visibilita' garantita**: il livello e' esclusivamente ordinamento e filtro,
  MAI un gate di visibilita'. L'explorer mostra sempre tutte le chiavi.
- **Default `advanced` fail-safe**: una chiave nuova non classificata finisce nel
  gruppo protetto "Configurazione avanzata", mai in home. Nessuna chiave
  sensibile puo' comparire per default tra le operazioni quotidiane.

### Alternative considerate

- **Classificazione per-categoria definitiva** (niente `ui_level` per-chiave).
  Scartata: le categorie sono miste (`providers` ha `api_key` quotidiane e
  circuit breaker profonde); una granularita' per-categoria costringerebbe a
  scelte sbagliate su interi blocchi e violerebbe la regola G tenendo la
  classificazione nel codice.
- **Nascondere le chiavi `advanced`** dietro un flag di visibilita'. Scartata: il
  livello deve restare ordinamento/filtro; nascondere configurazione produce
  configurazione "fantasma" non ispezionabile e favorisce toppe fuori UI.
- **Rigenerare `settings-keys.md` a mano** a ogni deriva. Scartata come toppa
  (regola H): senza un generatore deterministico in codice il documento torna
  stale; il fix definitivo e' `cargo xtask gen-settings-doc` dal DB.

## Riferimenti

- Fase 1: `apps/web-ide/lib/settings-categories.ts` (`labelForCategory`,
  `levelForCategory`, `KNOWN_CATEGORY_META`),
  `apps/web-ide/components/settings/settings-panel.tsx`,
  `apps/web-ide/components/admin-sidebar.tsx`, `apps/web-ide/app/admin/page.tsx`;
  commit `6ed7405e`.
- Fase 2/3 (previsti): `crates/nexus-types/src/settings_dto.rs`,
  `crates/mcp-core/src/settings.rs`, `crates/xtask/src/audit_settings.rs`,
  `apps/web-ide/app/admin/kb/page.tsx`.
- [[settings-keys]] - elenco chiavi settings (auto-generato dal DB, oggi stale;
  la Fase 2 lo rigenera con `cargo xtask gen-settings-doc`).
- [[0026-punto-unico-de-duplicazione]] - regola L e catalogo dei punti unici
  (voce "Categorie settings per la navigazione admin").
- [[0031-audit-settings-fonte-unica]] - audit settings come fonte unica, base del
  generatore doc e della guardia descrizione-vuota.
- Precedente di normalizzazione categoria: migrazione `0408`.
</content>
</invoke>
