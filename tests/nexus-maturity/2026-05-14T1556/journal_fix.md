# Journal cumulativo dei fix applicati a Nexus durante il test di maturita

**Sessione TS**: 2026-05-14T1556
**Branch fix**: `test/nexus-maturity-2026-05-14T1556`
**Baseline IDEAI**: 77dd0929503f1b57cd627195c839f0d33cf95219

Ogni entry: categoria (A-H), file modificati, motivazione, esito post-restart.

---

## Fix #1 — pre-iter 1 (creazione progetto da UI)

**Categoria**: G (Frontend AI Workspace)
**Data**: 2026-05-14T16:24

### Sintomo
Dalla dialog "Progetti" del web-ide (apertura col bottone "⌘" della top navbar) NON e' possibile creare un nuovo progetto registrando una directory locale esistente. L'unico flow disponibile e' "Clone da GitHub". L'utente, per testare il flow standard di creazione progetto, non ha campi sufficienti.

### Diagnosi
Il componente `ProjectImportWizard` ([apps/web-ide/components/project-import-wizard.tsx:541](apps/web-ide/components/project-import-wizard.tsx:541)) esiste ed e' completo (directory browser, register, analyze, db-config), ma **non e' istanziato** in nessuna pagina/componente visibile. La prop `onRegister` di `ProjectSwitcher` ([apps/web-ide/components/project-switcher.tsx:49](apps/web-ide/components/project-switcher.tsx:49)) era dichiarata ma mai usata.

Inoltre i 3 nuovi tool nexus appena committati (`project_register_existing_dir.rs`, `project_register_from_git.rs`, `project_workspace_init.rs`) non hanno wiring lato UI ma sono indipendenti da questo fix.

### File modificati
- [apps/web-ide/components/project-switcher.tsx](apps/web-ide/components/project-switcher.tsx) — 4 hunk:
  1. Import di `ProjectImportWizard`
  2. Nuovo state `importWizardOpen`
  3. Sezione "Importa cartella locale" nella dialog projects con bottone che apre il wizard
  4. Render condizionale del `ProjectImportWizard` come overlay, con handler `onComplete` che chiude wizard, fa refresh lista progetti e switch al nuovo progetto

Nessuna modifica al backend o ai prop publici esistenti — modifica puramente additiva.

### Verifica
- `tsc --noEmit` sul workspace `apps/web-ide`: exit 0
- `git diff` mostra 45 righe aggiunte, 0 rimosse (eccetto cambio mode 100755→100644 di Linux)
- Rebuild web-ide via `./deploy/deploy-local.sh --web` (cache `.next/.turbo` ripulita)

### Esito atteso
Dopo rebuild, in `http://localhost:3000/ide` -> bottone "⌘" -> appare sezione "Importa cartella locale" con bottone "Importa cartella locale..." che apre il wizard (directory browser → analizza → conferma).

### Note per follow-up consolidamento
- I tool agente `project_register_*` rimangono non cablati lato UI ma sono richiamabili come tool da agenti Nexus stessi (consistent con il pattern "tool agente, non UI").
- Lo stub `chat-service:4020` resta da rimuovere o completare separatamente — non bloccante per il test.
- `dev-login` Next.js blocca in production (`NODE_ENV=production`) e `dev_login_server.py` ha `JWT_SECRET` hardcoded diverso dal DB — entrambi da consolidare in fix successivi.

---

## Gap di maturita rilevati durante iter 1 (turno 1 — modello gpt-4.1)

Questi NON sono fix applicati a Nexus, ma findings da consolidare in fix permanenti dopo il completamento del test. Vengono usati per riempire le dimensioni D1..D12 della rubrica finale.

### Gap M1 — UI gestione progetto non popolata
Nexus ha creato `backend/` (Express + 4 routes + middleware auth + test), `frontend/` (Vite + React + Tailwind + Zustand + Vitest), `prisma/schema.prisma`, `prisma/migrations/001_init.sql`. Pero':
- pannello **Porte** mostra "Nessuna porta rilevata per il progetto" (la tabella `nexus_port_allocations` e' vuota)
- pannello **Servizi → System** mostra "Nessun servizio con prefisso nexus-maturity-rental-"
- pannello **Servizi → Tasks** mostra "Nessun evento per il canale selezionato"
- nessun **service unit / container** registrato per i sotto-progetti backend e frontend

Nexus scrive codice sul filesystem ma non integra runtime info (porte attese, run config, service unit) con i pannelli AI Workspace. Categoria fix candidata: **D** (mcp-core hooks su file save che rilevano `package.json`/`Procfile`/`docker-compose.yml` e popolano `nexus_port_allocations` + service registry).

### Gap M2 — project_documents DB non popolato
Nexus ha generato `specs/prd_stack.md` (PRD con Attori, Casi d'uso, NFR) e `backend/README.md`. Pero' la tabella `project_documents` resta vuota (0 righe). Il PRD esiste solo come file `.md` — l'AI Workspace UI (qualunque pannello che si alimenta da `project_documents`) non li vede come "documenti del progetto".

Categoria fix candidata: **D** (hook mcp-core: quando l'agente scrive `*.md`/`docs/**/*` dentro `project_root`, inserisce automaticamente una riga in `project_documents` con `doc_type` inferito da path/nome — PRD, README, ARCHITECTURE, ecc.).

### Gap M3 — agent_steps NON in streaming con gpt-4.1
Con `openai/gpt-4.1` la run scrive 52 step `agent_steps` TUTTI INSIEME alla chiusura (a `brain SSE done`). Durante l'esecuzione `agent_steps` resta vuota, e il pannello AI Trace non puo' mostrare progress live. Con `anthropic/claude-sonnet-4-6` (turn 2) gli step ARRIVANO in streaming.

Categoria fix candidata: **E** (brain LangGraph: l'adapter OpenAI non emette eventi per ogni tool call al mcp-core SSE — uniformare con l'adapter Anthropic).

### Gap M4 — modalita "Continuo" non e' veramente autonoma
Il dropdown UI "Continuo" mappa a backend `automation_mode='automatic'` (e' una alias label, vedi Fix #4). In modalita automatic gpt-4.1 ha completato il primo loop e ha terminato con la domanda "Vuoi che proceda alla sistemazione dei tipi?" interrompendo il lavoro nonostante:
- prompt seed esplicita "Criterio di accettazione: pnpm verify deve passare"
- modalita Continuo dovrebbe implicare autonomia massima

Causa probabile: il system prompt agente in modalita Automatic non e' abbastanza imperativo, oppure il modello e' biased verso la conferma. Categoria fix candidata: **A** (UPDATE su `nexus_prompt_templates` per agent.coder.base/system in automation=Automatic: aggiungere directive "MAI chiedere conferma all'utente, procedi sempre fino al criterio di accettazione esplicito nel task").

### Gap M5 — pnpm-lock.yaml del monorepo IDEAI modificato
Side-effect strutturale: `projects_base_root = /home/administrator/ideai/projects/` pone il progetto target sotto il workspace pnpm IDEAI. Quando Nexus esegue `npm install` o `pnpm install` per `backend/` o `frontend/`, pnpm risale all'workspace e modifica il lockfile del monorepo invece di crearne uno isolato.

Categoria fix candidata: **D+H**. Soluzioni alternative:
- spostare `projects_base_root` FUORI da IDEAI (es. `/home/administrator/projects/`); richiede aggiornamento del `settings` DB e migrazione progetti esistenti
- aggiungere `.npmrc` `lockfile-version=true` + `package-lock=true` nel project target, e/o `pnpm-workspace.yaml` ignore del path projects
- aggiungere `projects/**` a `.gitignore` del monorepo IDEAI cosi' Nexus non puo' contaminare workflow su `git status`

### Gap M10 — pannelli "Risolvi con Nexus" non popolati da errori dell'agente

**Severita**: ALTA per il flow di chiusura del loop maturita.

L'AI Workspace ha bottoni "Risolvi con Nexus" su molti pannelli ([apps/web-ide/components/panels/bottom-panel-manager.tsx:188](apps/web-ide/components/panels/bottom-panel-manager.tsx:188) `promptFromProblem`, `promptFromPort`, `promptFromPlaywrightRun`, [debug-panel.tsx:505](apps/web-ide/components/panels/debug-panel.tsx:505), [run-panel.tsx:822](apps/web-ide/components/panels/run-panel.tsx:822) `buildDiagnosticPrompt`, [git/source-control-panel.tsx:843](apps/web-ide/components/git/source-control-panel.tsx:843)). Il flow target e':

```
agente esegue → errore rilevato → pannello mostra errore + bottone → utente clicca → prompt automatico re-inviato all'agente
```

**Cosa abbiamo osservato in iter_02**:
Nexus dichiara nel messaggio finale: "problemi con fast-jwt, alcuni errori ENOTEMPTY che rallentano la verifica automatica. L'installazione delle dipendenze e in corso". Pero':
- Pannello "Problemi": "Nessun problema aperto" — TypeScript LSP in-editor non e' attivato perche nessun file backend e' aperto
- Pannello "Run & Debug" → SERVIZI SYSTEMD: "Nessun servizio con prefisso nexus-maturity-rental-" — Nexus NON ha creato service unit per il backend
- Pannello "Console Debug": "Nessun output di debug. Avvia un processo nel terminale" — gli output di `run_command` dell'agente non sono streamati qui (l'agente usa il SUO terminal interno)
- Pannello "Porte": "Nessuna porta rilevata"

**Causa**: i pannelli si alimentano da:
- TS LSP solo se file aperto nell'editor utente (non riflette gli errori che l'agente vede nel suo run_command)
- Service registry solo se `+ Configura` manuale (Nexus non lo fa)
- Console Debug solo del terminal UI utente (Nexus usa il suo terminal interno con `run_command` tool)
- Port detection solo su listener attivi (backend non e' mai stato avviato → nessuna porta detectabile)

**Implicazione**: il flow "Risolvi con Nexus" oggi NON CHIUDE per errori di install/build/runtime — solo per errori statici visibili al TS LSP in-editor o per servizi systemd manualmente configurati. La maturita prodotto richiede che gli errori reali incontrati dall'agente fluiscano nei pannelli per essere riproponibili con un click.

**Fix candidato (categoria D — invasivo)**:
1. Quando un `run_command` agent_step termina con exit != 0, mcp-core dovrebbe:
   - Parsare l'output cercando segnali di errore (es. `ENOTEMPTY`, `npm ERR!`, `error TS`, `Cannot find module`)
   - Pubblicare un record in una nuova tabella `project_runtime_issues` (project_id, source: "agent_run_command", run_id, step_id, error_summary, error_detail, status: "open")
   - Il pannello Console Debug o un nuovo pannello "Errori Agente" carica queste righe e per ognuna mostra "Risolvi con Nexus" che invia prompt mirato (es. "Risolvi l'errore X osservato nello step Y del run Z")
2. Alternativamente: il tool `run_command` dovrebbe ritornare exit_code strutturato che il frontend AI Trace puo visualizzare con bottone diretto.

### Gap M15 — UI Source Control non ha un flow "Crea repository GitHub da progetto"

**Severita**: Alta (test maturita E2E)

L'utente ha richiesto di "fare un test sulla creazione del repository su git" durante il rollout del progetto autonoleggio. Pero', il pannello Source Control del web-ide offre solo:

- **Pull dal remote corrente** (richiede remote gia configurato)
- **Push verso il remote corrente** (richiede remote gia configurato, e va in errore secco se manca: `fatal: 'origin' does not appear to be a git repository`)
- **Stage tutto / Rimuovi stage / Commit** (operazioni git locali, ok)
- **Crea branch / cambia branch** (operazioni git locali, ok)
- **GitHub: connesso come <user>** (mostra solo lo stato connessione, badge)
- **Remote: Nessun remote origin configurato — missing_origin_remote** (label rossa, no CTA)

Manca il flow:
1. Bottone "Crea repository GitHub" nella sezione Remote
2. Dialog che chiede nome + privacy + descrizione
3. Chiamata a GitHub API `POST /user/repos` con auth dell'utente collegato
4. `git remote add origin <new_url>` automatico nel progetto
5. Initial push (chiamata a `publish-branch` endpoint esistente)

Gli endpoint backend NON includono un `POST /api/projects/:id/github/create-repo` — esistono solo `github/status`, `github/repositories` (probabilmente list), `github/clone`, `github/publish-branch`, `github/pull-request`.

**Workaround temporaneo (testato durante il test)**: l'utente puo' chiedere a Nexus via chat di creare il repo, e Nexus ha:
- Auth GitHub (token utente in DB)
- `http_request` tool per chiamare `POST https://api.github.com/user/repos`
- `run_command` tool per `git remote add origin ...` e `git push -u origin main`

Pero' questo richiede l'utente sappia cosa chiedere — il flow UI deve essere first-class.

**Fix candidato (categoria D + G)**:
1. Backend: nuovo endpoint `POST /api/projects/:id/github/create-repo` con body `{name, private, description}` che:
   - Chiama GitHub API con il token dell'utente
   - Esegue `git remote add origin <new_clone_url>` nel project_root
   - Ritorna `{repo: GitHubRepositorySummary, originUrl}`
2. Frontend Source Control: bottone "Crea repository GitHub" visibile quando `remote.reason === "missing_origin_remote"`, apre dialog di creazione, dopo successo aggiorna lo stato e abilita Push

### Gap M13 — rendering chat narrative piatto (stream of consciousness senza separazione step)

**Severita**: Media (UX)

Quando l'agente Claude emette un messaggio finale narrativo lungo (es. iter_03 finale 8 minuti, 112 step), il messaggio assistant nella chat e' un unico blocco markdown continuo. Esempio dal vivo (iter_03):

> "Now let me run typecheck and tests to see the current errors:There are two categories of errors: UserRole and BookingStatus not exported from @prisma/client - Prisma client needs generation authenticate/authenticateAdmin not typed on FastifyInstance - need type augmentation Let me fix both:The Prisma client was generated to pnpm location but the backend uses npm. Let me check what node_modules path is being used:..."

I marker tipici del flow narrativo (`Let me X`, `Now check Y`, `Typecheck passes`, ecc.) sono concatenati senza interruzioni di paragrafo, rendendo difficilissimo seguire gli step. Inoltre i risultati delle tool call sono inseriti inline (es. "Let me check what path:The generated client doesn't have...") senza separazione visiva tra "intent dell'agente" e "output osservato".

**Causa probabile**: il MarkdownBlock in [apps/web-ide/components/chat/markdown-renderer.tsx](apps/web-ide/components/chat/markdown-renderer.tsx) renderizza il content del messaggio senza pre-processing. Il modello emette testo senza paragraph breaks tra azioni, e/o il content del messaggio fonde input/output dei tool nel discorso narrativo.

**Fix candidato (categoria G)**:
1. Pre-processing del content prima del rendering: regex che individua marker tipici di transizione (`Let me\b`, `Now\b`, `^\s*The\b`, `passes\.\s+\w`, ecc.) e inserisce `\n\n` prima
2. Oppure: rendering dei tool call come "step card" inline (gia disponibili in `inline-trace-panel.tsx`) con il testo narrativo NEI BLOCCHI tra una card e l'altra
3. Soluzione strutturale: configurare il system prompt agent.coder.base affinche emetta esplicitamente `\n\n` tra fasi (e.g. "tra ogni gruppo di tool consecutivi e il testo narrativo che li introduce inserisci una riga vuota")

Priorita media: non blocca funzionalita ma compromette severamente la leggibilita del log agente, soprattutto in run lunghe.

### Gap M14 — Errori console del frontend dell'app generata non raggiungono Nexus

**Severita**: Alta (test maturita)

Quando l'app generata (es. il frontend autonoleggio) gira ma ha errori JavaScript runtime (es. `SyntaxError: The requested module does not provide an export 'Car'`), Nexus NON ha modo di osservarli automaticamente. Il browser dell'utente carica `http://localhost:5173/` → rende schermo bianco → l'errore esce nella console del browser → nessun pannello Nexus lo vede.

**Causa**: il flow di rilevamento errori dei pannelli "Problemi"/"Console Debug" si alimenta da:
- TypeScript LSP in-editor del web-ide (vede solo file aperti nell'editor di Nexus)
- Container/processi del progetto (vede solo stdout/stderr di run_command)

NON vede:
- Console JavaScript del browser dell'utente sull'app a `:5173`
- HTTP error responses (404, 500) dell'app generata
- Renderizzazioni vuote / Suspense errors / hydration errors di React

**Implicazione per il test**: il bug `SyntaxError 'Car'` di iter_03 NON e' stato rilevato dal flow nativo. Il typecheck statico era passato (perche' `Car` esisteva come tipo ma non come export named). L'agente ha completato la sua run con esito "successo" perche' i suoi curl ritornavano 200 — ma il front-end NON renderizzava nulla. Per scoprirlo ho dovuto aprire il browser manualmente e leggere la console errors.

**Fix candidato (categoria G + D)** — gia partial overlap con M10:
1. Estendere Nexus con un tool `browser_check` che apri (headless) l'URL del frontend dev server, attende il render, cattura console errors + screenshot
2. Aggiungere step automatico in modalita AUTOMATICA: dopo aver avviato il frontend con curl 200, eseguire `browser_check` su quella URL per rilevare errori JS
3. Integrare nel pannello "Console Debug" un poll dell'URL frontend usando Puppeteer/Playwright e mostrare errori console del browser dell'utente
4. Alternativa minimal: il system prompt regola 11 (Fix #5) andrebbe estesa: "verifica HTTP NON BASTA — usa playwright/node se disponibile per renderizzare la pagina e verificare assenza di errori console"

### Gap M18 — Manca auto-install dei tool richiesti alla creazione/clone di un progetto

**Severita**: Alta (richiesta esplicita utente durante test E2E)

Citazione utente:
> "quando si crea un nuovo progetto o importato (clonato) nexus deve installare tutti i tool che necessita per il suo funzionamento in automatico"

Stato attuale: il flow `POST /api/projects/register` o `POST /api/projects/clone` registra il progetto, crea workspace, esegue analyze, ma NON installa automaticamente alcun tool di sviluppo. Per ottenere Playwright/ESLint/Prettier/husky/typecheck-tools serve un prompt manuale all'agente che usa `run_command` piecemeal.

**Fix candidato (categoria D + C, invasivo)**:
1. Nuovo endpoint REST `POST /api/projects/:id/services/wizard/auto-bootstrap` che:
   - Rileva tipo progetto da package.json / Cargo.toml / pyproject.toml
   - Per ogni tipo applica un preset di tool obbligatori:
     - Node/React/Vite: `@playwright/test`, `eslint`, `prettier`, `husky`, `lint-staged`, `vitest`/`jest`
     - Rust workspace: `cargo-clippy`, `cargo-fmt`, `cargo-watch` (gia in toolchain), `cargo-deny` (opt)
     - Python: `ruff`, `pytest`, `mypy`, `pre-commit`
   - Installa via tool dedicati Nexus (vedi M19) — NON `run_command pnpm add`
   - Aggiorna `project_settings` con `auto_bootstrap_completed=true` + lista tool installati
   - Trigger automatico subito dopo `register_project` se `auto_bootstrap` flag nel body (default true)
2. Frontend UI: dialog finale del ProjectImportWizard (Step 5/5 "Azioni suggerite") gia esistente ma vuoto — popolarlo con checklist tool da installare con bottone "Installa tutto" (default checked)

### Gap M19 — Installazione Playwright deve passare per MCP Nexus, non per shell agente

**Severita**: Alta (richiesta esplicita utente, lega M17)

Citazione utente:
> "L'installazione di playwright deve avvenire tramite mcp nexus in modo da gestirne la configurazione"

Stato attuale: il bottone "Abilita Playwright" del pannello UI manda all'agente un prompt operativo "esegui questi step: 1. pnpm add -D @playwright/test, 2. crea config, ecc." → l'agente esegue tutto via `run_command`. La configurazione (BASE_URL dalla port allocation, retries, reporter, projects browser) e' determinata dall'agente caso per caso, con possibilita di errori (vedi Gap M17 — porta sbagliata).

**Fix candidato (categoria D + B)**:
1. Nuovo `nexus_tool` `playwright_install` (in `crates/mcp-core/src/nexus_tools/playwright_install.rs`) che esegue atomicamente nel backend Rust:
   - `npm install -D @playwright/test` nel `project_root` (oppure nel sotto-package frontend se rilevato)
   - `npx playwright install --with-deps chromium` (config-driven sui browser da installare)
   - Genera `playwright.config.ts` deterministico, leggendo `nexus_port_allocations` (con `pick_dev_port` corretto, fix M17), con `baseURL = process.env.BASE_URL ?? "http://localhost:<dev_port>"`, retries=1, reporter=list, browsers configurabili
   - Crea `e2e/` con un test smoke iniziale (root render + console no-errors)
   - INSERT in `project_settings` `playwright_enabled=true`
   - Ritorna `{installed_packages, config_path, dev_port, base_url, smoke_test_path}`
2. Nuovo endpoint REST `POST /api/projects/:id/services/install-playwright` (wrapper sul tool)
3. Pannello Playwright UI: bottone "Abilita Playwright" chiama l'endpoint REST (non manda prompt all'agente). Bottone "Avvia test" continua a chiamare `test_playwright` come oggi
4. Beneficio per agente: quando l'agente vuole installare/configurare Playwright lo fa via `playwright_install` tool, NON via shell, evitando bias e configurazioni inconsistenti

### Gap M17 — bottone "Abilita Playwright" sceglie porta backend invece di dev frontend

**Severita**: Alta (porta sbagliata = test che falliscono sempre)

Nel pannello Playwright del web-ide, cliccando "Abilita Playwright" parte un'agent_run con prompt operativo che include la linea:
> "Abilita Playwright nel progetto. Porta dev Nexus assegnata: **3002** (BASE_URL default: http://localhost:3002)."

Pero' 3002 e' la porta del BACKEND Fastify del progetto autonoleggio, non del frontend Vite dev server (che e' 5173). Nelle nexus_port_allocations sono allocate entrambe:
- `5173` con label `dev`
- `3002` con label `backend`

La logica `pick_dev_port` in [crates/mcp-core/src/nexus_tools/test_playwright.rs:25](crates/mcp-core/src/nexus_tools/test_playwright.rs:25) cerca esplicitamente label "dev"|"app"|"http"|"web"|"frontend"|"serve"|"server" — dovrebbe scegliere 5173. Pero' la funzione/endpoint chiamata dal bottone UI "Abilita Playwright" usa un altro algoritmo (probabilmente "porta numericamente piu' bassa" senza preferenza label) e prende 3002.

**Conseguenza**: i test Playwright generati con BASE_URL=:3002 falliranno tutti perche' 3002 e' un'API Fastify che ritorna 404 su /login, /register, /cars (sono path frontend React).

**Fix candidato (categoria D)**:
1. Identificare il componente backend che alimenta il prompt operativo "Abilita Playwright" (probabilmente in `crates/mcp-core/src/project_workspace/wizard.rs` o un endpoint dedicato `services/wizard/install` per kind=playwright)
2. Sostituire la selezione porta con `pick_dev_port` (gia esistente in test_playwright.rs)
3. Aggiungere fallback descrittivo nel prompt: "Porte allocate disponibili: dev=5173, backend=3002. Per test E2E del frontend usa BASE_URL=http://localhost:5173"

### Gap M11 — EXPLORER non si auto-refresh ai write dell'agente

**Severita**: Media

L'EXPLORER del web-ide ([apps/web-ide/components/project-explorer.tsx](apps/web-ide/components/project-explorer.tsx) presumibile) NON aggiorna il tree dei file quando l'agente Nexus scrive nuovi file via `tool_write_file`.

**Comportamento osservato in iter_02**:
- A inizio iter_02 l'EXPLORER mostrava "Nessun file disponibile nella directory selezionata"
- Nexus ha scritto 15 file (`backend/`, `docs/`, `prisma/`) in 4 min
- L'EXPLORER e' rimasto vuoto fino a quando ho cliccato manualmente "Rianalizza progetto" in Source Control (azione separata e non ovvia per l'utente)
- DOPO la rianalisi, l'EXPLORER ha mostrato correttamente le 3 directory espandibili con tree completo dei file singoli

**Causa probabile**: nessun file watcher / WebSocket subscription dal frontend al filesystem del progetto target. L'EXPLORER carica il listing una volta (al mount o ad analyze) e non ascolta cambiamenti runtime.

**Fix candidato (categoria G + D)**:
1. mcp-core: aggiungere endpoint SSE `GET /api/projects/:id/fs-events` che notifica file create/modify/delete (basato su `notify` crate Rust o polling DB se gia tracciato in code index)
2. web-ide ProjectExplorer: connettersi all'SSE al mount, invalidare la cache del tree quando arriva un evento
3. Soluzione minimale (no SSE): l'EXPLORER fa refresh automatico ogni 10s, oppure ricaricara al completamento di ogni agent_run (listener su run status)

### Gap M12 — Source Control mostra solo directory aggregate, non file singoli; messaggio "Repo: Non disponibile" inconsistente

**Severita**: Media

Il pannello Source Control ([apps/web-ide/components/git/source-control-panel.tsx](apps/web-ide/components/git/source-control-panel.tsx)) mostra una sezione "Modifiche / UNTRACKED (N)" che lista i path da `git status --porcelain`. Pero':

**Comportamento osservato**:
- Nexus ha creato 15 file in 3 directory (`backend/`, `docs/`, `prisma/`)
- Source Control mostra: `UNTRACKED (3)` con righe `U backend`, `U docs`, `U prisma` (aggregato a DIRECTORY)
- **Non c'e' modo di vedere quali file SINGOLI sono dentro `backend/`** o di stage solo alcuni — solo "+ All" o "+" sulla directory intera
- Inoltre la sezione "Repo: Non disponibile" sopra la lista contraddice la CRONOLOGIA COMMIT sotto, che mostra correttamente il baseline `58a74b5`. Probabilmente "Repo: Non disponibile" riferisce a metadati "remote upstream" (visto `missing_origin_remote`) ma l'etichetta e' fuorviante

**Fix candidato (categoria G + D)**:
1. Backend mcp-core: l'endpoint che ritorna le modifiche git deve eseguire `git status --porcelain` SENZA flag `-uno-untracked-mode` (default mostra file singoli, ma `-uall` esplicito + parser che separa per file invece di directory)
2. Frontend Source Control: rendering tree-like delle modifiche (directory espandibili con file singoli sotto, ognuno con stato `??`/`M`/`D`/`A`)
3. Frontend: la stringa "Repo: Non disponibile" va o rimossa o sostituita con "Remote: non configurato" coerente con `missing_origin_remote` sotto

### Gap M8 — Leak IDEAI nel frontend (path/servizi internal esposti all'utente)

**Severita**: ALTA per la maturita di Nexus come prodotto utente-facing.

Il progetto utente registrato (`nexus-maturity-rental`) e' creato dentro `/home/administrator/ideai/projects/...` perche' `projects_base_root` punta li. Questa path assoluta del monorepo IDEAI **viene esposta in piu' punti dell'UI Nexus**, mostrando all'utente la struttura interna del prodotto invece di un'esperienza isolata "il mio progetto e' un contenitore opaco".

**Leak rilevati**:
- Pannello **Servizi → System**: stringa "Root: /home/administrator/ideai/projects/nexus-maturity-rental-2026-05-14T1556"
- **EXPLORER sidebar**: header e voci mostrano path complete `/home/administrator/ideai/projects/...`
- **Terminale integrato**: prompt PS1 di default mostra `administrator@Dino:~/ideai/projects/...$`
- **Sidebar Servizi**: voci `MCP Core` / `Neural Core` espongono i servizi interni di Nexus (admin-only)
- **AI Trace / `agent_steps.tool_input`**: path assolute IDEAI registrate nella chat history
- **Messaggi chat assistant**: possibile inserimento di path assolute quando l'agente conferma scritture (es. "ho creato `/home/administrator/ideai/projects/.../file.ts`")

**Causa**: `projects_base_root = /home/administrator/ideai/projects` (impostato in `settings`). Il frontend usa la path canonical del workspace e la mostra letterale in ogni UI element che la riceve dal backend.

**Fix candidato consolidato (categoria G + D), da applicare come Fix #5 a fine iter_02**:
1. **Frontend (cat G)**:
   - Servizi → System: rimuovere il prefisso `/home/administrator/ideai/projects/`, mostrare solo il segmento finale del path o un alias `~/<project-slug>`
   - EXPLORER: stesso trattamento, opzionalmente mostrare un trail breadcrumb relativo
   - Sidebar Servizi: nascondere `MCP Core` e `Neural Core` se `role !== 'admin'` o aggiungere un toggle "Mostra servizi interni"
   - AI Trace: stripping della path assoluta nel rendering del `tool_input` quando inizia con `projects_base_root`
2. **Backend (cat D)**:
   - mcp-core: nel handler che ritorna `rootPath` ai client, sostituire la path con un alias path-relative `~/<slug>` se l'utente non e' admin
   - chat_messages: nel formatter dell'output del tool (es. `tool_write_file` ritorna "File '/home/.../X' scritto"), aggiungere un step di sanitizzazione che converte path assolute IDEAI in path relative al project_root
3. **Soluzione strutturale alternativa (piu' ampia)**: spostare `projects_base_root` FUORI da `/home/administrator/ideai/` (es. `/home/administrator/projects/`). Risolve M5 e M8 insieme ma richiede migrazione progetti esistenti e re-registrazione di `redemptor`. Da valutare in roadmap.

### Gap M7 — collect.sh ha bug di quoting psql via docker exec
A fine iter_01 i CSV `agent_runs.csv`, `agent_steps.csv`, `ai_usage_ledger.csv` sono 0 byte (i file `.err` 92-184 byte contengono il vero errore). La query `COPY (...) TO STDOUT WITH CSV HEADER` con quoting via `docker exec -i psql -At -c "..."` rompe l'apostrofo dentro la WHERE clause. Categoria fix candidata: **H** (riscrivere collect.sh con heredoc o file SQL temporaneo).

### Gap M6 — auto-routing modello per turn (interessante, non gap)
Tra turn 1 e turn 2 il routing ha cambiato modello: gpt-4.1 (turn 1, 1.12 EUR per 4 min) → claude-sonnet-4-6 (turn 2). E' un comportamento desiderato della routing matrix, ma rende meno deterministico il test di maturita. Da annotare in `iterations_summary.csv` per ogni turn.

---

## Fix #3 — post-iter 1 (Gap M4: autonomia MODALITÀ AUTOMATICA)

**Categoria**: D (codice Rust mcp-core)
**Data**: 2026-05-14T17:05
**Commit**: post-iter 1 sul branch `test/nexus-maturity-2026-05-14T1556`

### Sintomo
In modalita Continuo (backend `automation_mode='automatic'`) gpt-4.1 ha terminato il primo turno con la domanda "Vuoi che proceda alla sistemazione dei tipi?" nonostante il prompt seed avesse esplicitato come criterio di accettazione "pnpm verify deve passare" e nonostante il system prompt automation gia contenesse "NON chiedere conferma".

### Diagnosi
Il blocco directive `=== MODALITÀ AUTOMATICA ===` e hardcoded in [crates/mcp-core/src/chat_messages.rs:1825](crates/mcp-core/src/chat_messages.rs:1825). Aveva 8 regole ma non vietava esplicitamente i messaggi terminali con domanda, ne legava la chiusura del loop al successo del criterio di accettazione. Il modello (gpt-4.1) interpretava la regola 8 ("Concludi SEMPRE con un messaggio") come autorizzazione a chiudere anche col criterio non soddisfatto, ed emetteva la "domanda di conferma" come parte naturale di quel messaggio.

### File modificati
- [crates/mcp-core/src/chat_messages.rs:1826-1837](crates/mcp-core/src/chat_messages.rs:1826) — directive AUTOMATICA estesa con:
  1. Regola 2 ampliata: vietate frasi terminali "Vuoi che proceda?", "Posso continuare?", "Devo modificare?", "Procedo con...?"
  2. Nuova regola 7 (CONTRATTO DI CHIUSURA): il messaggio finale puo' essere emesso solo dopo aver eseguito con esito POSITIVO il criterio di accettazione esplicito (es. pnpm verify, cargo check + clippy + test). Se fallisce, continua il ciclo fix→verify.
  3. Nuova regola 8 (BLOCCO ESTERNO): solo problemi non risolvibili dall'agente (credenziali, servizi terzi) giustificano chiusura prima del criterio
  4. Nuova regola 9: il messaggio iniziale dell'utente e' autorizzazione completa, non servono conferme intermedie
  5. Nuova regola 10: contenuto strutturato del messaggio finale (sintesi prodotto + esito criterio + blocchi). NESSUNA domanda terminale

### Verifica
`cargo check -p mcp-core` exit 0 in 14.57s. Rebuild `--rust` (release) + restart mcp-core.

### Esito atteso
In iter_02 gpt-4.1 (o qualunque modello selezionato dal routing per il turn) dovrebbe iterare in autonomia su typecheck/lint/test fino al passaggio del criterio, senza chiedere conferme intermedie.

---

## Fix #4 — post-iter 1 (Gap M2: project_documents non popolato)

**Categoria**: D (codice Rust mcp-core)
**Data**: 2026-05-14T17:10
**Commit**: stesso commit di Fix #3

### Sintomo
Nexus ha generato durante iter_01 il file `specs/prd_stack.md` (PRD funzionale, 2.9 kB) e `backend/README.md`. La tabella `project_documents` resta vuota — l'AI Workspace UI (pannelli "Documenti progetto") non vede questi documenti.

### Diagnosi
Il tool `tool_write_file` in [crates/mcp-core/src/agent_tools/files.rs:140](crates/mcp-core/src/agent_tools/files.rs:140) scrive il file su FS, reindicizza con `reindex_single_file` e fa `maybe_auto_scan_file`, ma non popola alcuna riga in `project_documents`. La tabella ha un check constraint `doc_type ∈ {functional_analysis, technical_analysis, er_diagram, project_management, release_notes}` e indice unique `uq_project_documents_path (project_id, file_path)` — il framework e' pronto, manca il trigger.

### File modificati
- [crates/mcp-core/src/agent_tools/files.rs:180-260](crates/mcp-core/src/agent_tools/files.rs:180) — aggiunto:
  1. Hook nel `tokio::spawn` post-write: chiama `upsert_project_document_if_doc(db, project_id, rel_path, content)`
  2. Nuova funzione `upsert_project_document_if_doc` async che:
     - Filtra solo file `.md`/`.markdown`
     - Mappa il nome del file al `doc_type` valido dal check constraint:
       - `prd*` / `specs/*` / file con "functional" → `functional_analysis`
       - `README.md` / `architecture*` / `docs/*` / file con "technical" → `technical_analysis`
       - `erd*` / `schema_diagram*` → `er_diagram`
       - `changelog*` / `release_notes*` → `release_notes`
       - `contributing*` / `roadmap*` → `project_management`
       - Altri `.md` non rilevanti → skip
     - Titolo estratto dalla prima riga `# ...` del file (max 255 char), fallback al filename
     - `INSERT ... ON CONFLICT (project_id, file_path) DO UPDATE` (idempotente)
     - `status='draft'`, `metadata={"source":"agent_write_file"}`
  3. Errori non bloccanti (eventuali fallimenti del DB insert non impediscono la scrittura del file)

### Verifica
`cargo check -p mcp-core` exit 0. Rebuild `--rust` + restart.

### Esito atteso
In iter_02, ogni scrittura di `.md` rilevante popolera automaticamente `project_documents`. L'utente potra' vedere i documenti nei pannelli UI dedicati.

### Limiti noti / Follow-up
- Solo `tool_write_file` ha l'hook. Altri tool che scrivono file (`edit_file`, `patch_file`, `create_file` se esiste) NON triggerano l'upsert. Estendere agli altri write-tool come iterazione successiva.
- I 5 doc_type del check constraint sono restrittivi: `prd`, `readme`, `architecture`, `spec`, `doc` come tipi nativi sarebbero piu' espressivi. Migrazione futura potrebbe ampliare l'enum.
- Files modificati MANUALMENTE (es. l'utente edita un README in shell) non triggerano l'hook. Solo i write fatti dall'agente.

---

## Fix #6 — post-iter 4 (guardrail git: solo via tool Nexus, no shell run_command)

**Categoria**: D (codice Rust mcp-core)
**Data**: 2026-05-14T18:40
**Branch test sha**: vedi `/tmp/ideai_after_fix6.sha`

### Sintomo
L'utente ha esplicitato durante il test E2E:
> "metterei un guardrail nella chat per far usare git solo tramite pannello nexus o al massimo tramite mcp di nexus"

Iter_05 ha mostrato che Nexus, per task GitHub complessi (creare repo, configurare remote, push iniziale), ricorre naturalmente a `run_command git ...` e `run_command gh ...` come shell escape — bypassando completamente la tracciabilita dei pannelli Source Control e Cronologia Commit. Gli endpoint REST `/api/projects/:id/github/publish-branch` riflettono lo stato git solo se l'agente li chiama esplicitamente.

### File modificati
- [crates/mcp-core/src/chat_messages.rs](crates/mcp-core/src/chat_messages.rs) — aggiunta regola 13 alla directive MODALITA AUTOMATICA:
  - Vietato `run_command git ...` e `run_command gh ...`
  - Lista esplicita dei tool agent dedicati: 16 git_* read-only, 5 git_* write (status/stage/commit/push/pull), 24 gh_* su issue/PR/release/workflow
  - Lista endpoint REST Nexus utilizzabili (`/api/projects/:id/github/clone`, `/publish-branch`, `/pull-request`)
  - Direttiva BLOCCO ESTERNO per operazioni mancanti (es. `git remote add`, `gh repo create`): segnalarle invece di aggirarle, cosi che vengano consolidate come nuovi tool/endpoint permanenti

### Verifica
`cargo check -p mcp-core` exit 0 (atteso). Rebuild `--rust` in background.

### Esito atteso iter_06+
- L'agente NON usera piu `run_command git/gh`
- Per le operazioni write supportate, il pannello Source Control mostrera lo stato aggiornato in tempo reale
- Per le operazioni non supportate, l'agente emettera BLOCCO ESTERNO con la descrizione precisa, alimentando il backlog di Gap M16

---

### Gap M16 — mancano tool/endpoint per `git remote add` e `gh repo create`

**Severita**: Alta (correlato a M15)

Anche con il guardrail M6 in vigore, le operazioni:
- `git remote add origin <url>` (configurare il remote di un progetto)
- `gh repo create <name>` (creare un nuovo repository su GitHub da zero)

non sono coperte da:
- Nessun tool agent dedicato in `agent_tools/git.rs` (solo status/stage/commit/push/pull)
- Nessun tool nexus_tools (i `git_*` esistenti sono read-only)
- Nessun endpoint REST mcp-core (`github/clone`, `github/publish-branch`, `github/pull-request` — manca `github/create-repo` e `github/add-remote`)

**Fix candidato (categoria D)**:
1. Nuovo agent tool `tool_git_remote_add(name, url)` in `agent_tools/git.rs` → wrapper su `run_git_command(["remote","add",name,url])`
2. Nuovo nexus_tool `gh_repo_create(name, private, description)` → chiama GitHub API con token utente, restituisce clone_url
3. Nuovo endpoint REST `POST /api/projects/:id/github/create-repo` che combina (a) creazione repo via GitHub API e (b) `git remote add origin` automatico, ritornando lo stato GitHub aggiornato
4. UI Source Control: bottone "Crea repository GitHub" che chiama l'endpoint (chiude anche Gap M15)

---

## Fix #5 — post-iter 2 (criterio accettazione include avvio applicazione)

**Categoria**: D (codice Rust mcp-core)
**Data**: 2026-05-14T18:00
**Branch test sha**: vedi `/tmp/ideai_after_fix5.sha`

### Sintomo
A fine iter_02, Nexus (modello gpt-4.1, modalita automatic, Fix M4 attivo) ha:
- Generato tutto lo scaffolding (backend Express + Prisma schema + docs/PRD)
- Provato `npm install` con errori `ENOTEMPTY` e issue su `fast-jwt`
- Concluso col messaggio: "Sto installando le dipendenze e risolvendo le issue di build per garantirti che la verifica sia pulita... Procedo direttamente con la risoluzione..."

Pero' la chat run e' poi **terminata** senza che Nexus avesse effettivamente avviato il backend o il frontend. L'utente ha richiesto esplicitamente: "dobbiamo arrivare a poter lanciare il progetto e vederlo funzionare". Il criterio "pnpm verify deve passare" e' troppo blando — verify puo' passare anche senza che l'app si avvii davvero (typecheck + lint + test unitari sono statici).

### File modificati
- [crates/mcp-core/src/chat_messages.rs:1838-1840](crates/mcp-core/src/chat_messages.rs:1838) — aggiunta regola 11 e 12 al blocco MODALITÀ AUTOMATICA:
  - **Regola 11**: criterio di accettazione default per app web/servizio = (a) compila/typecheck/lint OK, (b) test pass, (c) backend avviato e HTTP 2xx, (d) frontend dev server attivo e HTML root servito. Il criterio NON e' soddisfatto se l'app non e' stata raggiunta via HTTP almeno una volta.
  - **Regola 12**: tecnica operativa per servizi long-running — `run_command` con `background: true`, attesa, poi `curl http://localhost:PORT/`. Documenta porte effettive nel messaggio finale.

### Verifica
`cargo check -p mcp-core` exit 0. Rebuild `--rust` + restart mcp-core.

### Esito atteso iter_03
Con Fix #5 attivo, Nexus dovrebbe:
- Non concludere il messaggio finale se l'app non e' stata avviata e testata via curl
- Provare attivamente a fixare gli errori di `fast-jwt`/`ENOTEMPTY` perche' senza dipendenze installate non puo' avviare l'app
- Eseguire `npm run dev` (o equivalente) in background, attendere, e verificare con curl
- Riportare nel messaggio finale le porte effettive del backend (es. :3001) e del frontend (es. :5173)

### Strategia iter_03
- NON faccio rollback del progetto target (i 15 file + node_modules di iter_02 sono mantenuti)
- Cancello solo la chat session vecchia (per partire con context fresco)
- Invio un nuovo prompt che dice "continua il lavoro che hai iniziato in iter_02, completa l'installazione, avvia backend+frontend, mostra le porte attive"
- Cosi' Nexus parte gia avanzato e si concentra su "fix install errors → avvio → verifica HTTP"

---

## Fix #2 — pre-iter 1 (backdrop overlay wizard incompleto)

**Categoria**: G (Frontend AI Workspace)
**Data**: 2026-05-14T16:40

### Sintomo
Quando si apre il wizard "Importa progetto esistente" (cliccando "Importa cartella locale..." nella dialog projects), l'overlay scuro che dovrebbe oscurare la pagina sotto il modale NON copre tutta la pagina: la sidebar SOURCE CONTROL, l'Editor Workspace a destra, i pannelli Problemi/Terminale e la status bar in basso restano completamente visibili e cliccabili dietro il wizard.

L'utente lo ha notato a colpo d'occhio nel test UI automatico.

### Diagnosi
Il root container del `ProjectImportWizard` ([apps/web-ide/components/project-import-wizard.tsx:657](apps/web-ide/components/project-import-wizard.tsx:657)) usava `className="fixed inset-0 flex-row"` come se ci fosse Tailwind CSS. Il progetto **NON usa Tailwind** (`grep -r tailwindcss` nel web-ide ritorna 0 match, niente import `@tailwind` in `globals.css`). Quindi:
- `fixed` e `inset-0` erano stringhe inerti -> il div restava `position: static` in-flow nel suo parent (l'`<>` fragment dentro `ProjectSwitcher`)
- Il `background: "rgba(0,0,0,0.5)"` veniva applicato ma occupava solo l'area in-flow, non l'intera pagina

Altri modali del web-ide (es. `ProjectSwitcher`, [apps/web-ide/components/project-switcher.tsx:191-201](apps/web-ide/components/project-switcher.tsx:191)) usano correttamente inline style `position: "fixed", inset: 0` e funzionano bene.

Le altre utility classes nel wizard (`flex-col-gap-16`, `text-muted`, `text-base`, ecc.) sono definite in [apps/web-ide/app/globals.css](apps/web-ide/app/globals.css) e funzionano — solo `fixed` e `inset-0` mancavano.

### File modificati
- [apps/web-ide/components/project-import-wizard.tsx:657](apps/web-ide/components/project-import-wizard.tsx:657) — sostituito `className="fixed inset-0 flex-row"` con `style={{ position: "fixed", inset: 0, display: "flex", ... }}` mantenendo tutto il resto invariato.

### Verifica
- Rebuild `./deploy/deploy-local.sh --web` con `.next` e `.turbo` puliti
- Verifica visiva: apri "⌘" -> "Importa cartella locale..." -> backdrop oscura tutta la pagina

### Note per follow-up
Audit suggerito: `grep -rn 'className="[^"]*\b(fixed|inset-0|absolute|relative|flex|grid|hidden)\b' apps/web-ide/**/*.tsx` per individuare altri usi di utility Tailwind non supportate. Una regola lint custom (Fix categoria F futuro) potrebbe intercettarli prima del merge.

---
