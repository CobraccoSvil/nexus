# Tech debt: duplicazione del codice

Metrica e baseline della duplicazione cross-linguaggio (TS/JS/Rust/Python),
parte dell'operativizzazione della regola L (vedi
[ADR 0026](.nexus-vault/adr/0026-punto-unico-de-duplicazione.md)).

## Come si misura

```bash
bash scripts/dup-report.sh                 # misura + gate ratchet vs baseline
bash scripts/dup-report.sh --report-only   # solo misura
bash scripts/dup-report.sh --update-baseline   # riallinea la baseline (solo al ribasso)
```

Lo strumento e' `jscpd` (config in `jscpd.json`, soglia `minLines=5`, `minTokens=50`).
Il report dettagliato (con i singoli cloni) finisce in `.dup-report/`.

## Gate "ratchet"

Il numero di cloni puo' solo SCENDERE rispetto a `.dup-baseline.json`. Il job CI
in `.github/workflows/verify.yml` fallisce se la duplicazione aumenta. Dopo ogni
wave di consolidamento si riallinea la baseline al ribasso. La baseline non si
alza mai: il debito e' monotono decrescente.

## Baseline

La baseline iniziale e' registrata in `.dup-baseline.json` alla chiusura di Wave 0.
Aggiornare questa tabella a ogni wave che riduce il debito.

| Data | Wave | % righe duplicate | Cloni | Note |
|---|---|---|---|---|
| Wave 0 | 0 | 5.27% (16899 righe) | 1234 | Baseline iniziale, pre-consolidamento (2648 file, 20 formati) |
| Wave 2 | 2 | 5.26% | 1234 | Consolidate TemplateCache/TtlCache + get_template_or_default in nexus-cache/nexus-types. Conteggio jscpd invariato: le due copie differivano nel formato, quindi non erano exact-clone; la duplicazione era strutturale, ora coperta dal guard check-single-source. |
| Wave 3 | 3 | 5.26% | 1233 | get_setting: 4 implementazioni divergenti consolidate in una query unica (read_setting_raw) + viste in nexus-auth; i 3 def-site di mcp-core ora re-export. -1 clone. |
| Wave 4 | 4 | 5.26% | 1233 | Capability gia' fonte unica via ADR 0024 (vista v_model_capabilities): nessun churn. Aggiunto solo guard sul classificatore (classify_capabilities/infer_capabilities_from_name). Cloni invariati. |
| Wave 5 | 5 | 5.25% | 1229 | ensure_required_settings: default statici -> migrazione 0325 (no env var, regola G/H); parte dinamica projects_base_root -> punto unico nexus_types::ensure_projects_base_root (prima duplicata in mcp-core + admin-service). -4 cloni. |
| Wave 6 (parz.) | 6 | 5.24% | 1231 | json_extract: punto unico estrazione JSON da output LLM (agentic_classifier + routes/agent delegano). json_extract.py verificato 0 cloni; il +2 e' fluttuazione jscpd su cloni preesistenti (nexus_tools), non regressione. db_pool (~30 file), ttl_cache, intents, fix settings_db rinviati. |
| Wave 6b/6e | 6 | 5.24% | 1231 | brain/utils/{db_pool, ttl_cache}.py + brain/router/intents.py: punti unici Python paritetici al Rust (nexus-cache::TtlCache, nexus-auth::settings). settings_db.py converge su db_pool (incluso resolve_port), corretta docstring fuorviante, aggiunte varianti *_checked (regola H). catalog_loader.py converge su db_pool+ttl_cache, rimosso default URL hardcoded (regola G). agentic_classifier.py importa ALLOWED_INTENTS da intents.py. Test pytest: 4 nuovi file. Guard estesi: 4 nuovi checkpoint attivi (get_db_url, TtlCache python, ALLOWED_INTENTS, extract_json_block). Gate ratchet invariato. Convergenza degli altri ~28 file con psycopg2.connect resta come task incrementale (richiede pytest/venv). |
| Wave 7 | 7 | 5.22% | 1231 | Frontend admin: lib/format.ts esteso con formatDate/formatDateTime/formatBytes/formatCurrency; nuovo lib/use-list-data.ts (hook fetch+loading+error+reload); nuovo components/admin/AdminModal.tsx (wrap di ModalPortal con backdrop standard, ESC handler, role=dialog). AdminPageHeader esteso con slot action opzionale per supportare header con bottone affiancato. Adozione iniziale: pagine users, ai-feedback, sudo-manager, nexus-database convergono su AdminPageHeader + lib/format. tsc + eslint puliti, dup ratchet invariato. Adozione delle restanti ~15 pagine resta task incrementale (PR per gruppi di 4-5 con Playwright smoke). |
| Wave 8 | 8 | 5.22% | 1231 | Cross-language: chunking (8a) + error classifier (8b) con golden test cross-language. brain/utils/text_chunk.py paritetico a rag/chunker.rs; crates/mcp-core/src/provider_error_classifier.rs paritetico a brain/providers/error_handler.py. Fixture condivisa tests/fixtures/{chunker,error_classifier}_golden.json, letta da pytest E cargo test (drift = bug). Il golden test ha gia' giustificato la sua esistenza catturando un bug di regex Rust (continuazione di riga con backslash in raw string literal). context_offload.py converge su text_chunk; embeddings/service.py resta sul vecchio algoritmo con TODO documentato (cambio richiede re-index Qdrant dietro feature flag). RPC ClassifyError per ora invariata: provider_error_classifier.rs e' pronto, ma la rimozione della hop gRPC e' un cambio osservabile rimandato. 2 cloni del boilerplate dei test golden marcati jscpd:ignore (duplicazione GIUSTIFICATA). 13 guard checkpoint attivi totali. |
| Residui R1+R2+R3+R4 | post-8 | 5.20% | **1229** | Chiusura dei 4 residui tracciati. R1: 28 file Python con psycopg2.connect convergono su brain.utils.db_pool.get_db_url() (rimosse 5 occorrenze del default URL hardcoded in helpers.py + 9 file con il pattern unico, regola G applicata). R2: 7 pagine admin convertite su AdminPageHeader (billing, nexus-docs, profiles, project-learning, project-porting, prompts/dashboard, vector-maintenance) — pattern automatico con script, 4 fix manuali sui pattern atipici (import multi-linea, path relativo). R3: classify_error in neural_client.rs ora delega a provider_error_classifier::classify_text Rust, RPC ClassifyError piu' chiamata (zero hop gRPC sul path d'errore caldo). R4: feature flag rag.chunker.algorithm via mig 0326 (default 'legacy', switch a 'unified' richiede re-index Qdrant manualmente documentato in commento). Verifiche: py_compile pulito su brain/, tsc + lint puliti su web-ide, clippy -D warnings + golden test cross-language verdi, ratchet sceso a 1229 (baseline riallineata al ribasso). |
| Fase A (esclusione rumore) | post-residui | 5.49% | **1121** | Caccia ai cloni "fasulli" che inquinavano la metrica: il cmd di jscpd aveva `.` come argomento finale, forzando la scansione di TUTTO incluso recovery/files{,2}/ (file di backup storici), .git/hooks/ e altri non-codice. Script dup-report.sh corretto per passare path espliciti (apps packages crates brain) come da jscpd.json. jscpd.json esteso con ignore: recovery/**, .git/**, deploy/grafana/**, **/*_pb2.py, **/*_pb2_grpc.py, **/*.pb.rs, **/page.tsx.backup*. -108 cloni eliminati (570L regression_gate_node.py vs backup, 352L auto_link.rs vs backup, 75L+63L+59L migrazioni gemelle nei backup, hooks git, dashboard Grafana). La % SALE da 5.20 a 5.49 perche' il denominatore totale scende: e' il segnale corretto che ora misuriamo "vero codice" e non rumore. |
| Fase B (pilota fs_browse) | post-A | 5.48% | **1121** | Consolidamento pilota: nuovo modulo `nexus_types::fs_browse` con `BrowseDirectoryNode`, `list_root_candidates`, `list_directories`, `validate_directory_name`. Prima duplicati identici (~81 righe) tra admin-service/src/settings.rs e mcp-core/src/settings.rs. Il singolo clone 81L sparisce dalla top list, righe duplicate scendono di -28 (14024->13996). Conteggio cloni resta a 1121 (jscpd rivela cloni minimi <5L preesistenti che erano "coperti" dal clone grosso). 4 nuovi test unit sui FS helpers. Onesta' sul resto della Fase B: i cluster grossi rimanenti (mcp_client + mcp_connectors fra mcp-core e plugin-service ~566L, admin_projects/admin_users fra admin-service e mcp-core ~250L, doc-service/documents.rs) richiedono refactor architetturale grosso (nuovo crate nexus-mcp-client, riassetto endpoint admin) che esce dallo scope incrementale. Lasciati come task dedicato. |

## Hotspot noti (da consolidare)

Riferimento al piano di campagna; i punti unici target sono nel catalogo di ADR 0026.

- Rust: `parse_user_id` (Wave 1), `TemplateCache` (Wave 2), `get_setting` 5 varianti (Wave 3),
  capability detection ~12 file (Wave 4), health/cooldown (Wave 5).
- Python: `psycopg2.connect()` ~31 file (Wave 6a), cache 60s 4 punti (Wave 6b),
  JSON-da-markdown 3+ nodi (Wave 6c), intent duplicati (Wave 6e).
- Frontend: header/modali/stili inline su ~20 pagine admin (Wave 7).
- Cross-language: chunking 4 impl Python vs 1 Rust (Wave 8a).
