#!/usr/bin/env bash
# scripts/check-single-source.sh — Guard testuale dei "punti unici" nominati.
#
# Fallisce se una logica che deve avere UN solo punto di controllo (regola L /
# ADR 0026) ricompare in una definizione fuori dal suo modulo autoritativo.
# E' un guard a costo zero, complementare a jscpd (scripts/dup-report.sh): jscpd
# misura il copy-paste generico, questo blocca le regressioni sui punti unici noti.
#
# I check sono ATTIVATI in modo incrementale: ogni wave della campagna di
# de-duplicazione decommenta il proprio check quando il consolidamento e'
# completo. In Wave 0 nessun consolidamento e' ancora avvenuto, quindi la
# sezione attiva e' vuota: e' corretto e atteso.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

fail=0

# assert_single LABEL REGEX ALLOWED_GLOB [SEARCH_DIRS...]
# Verifica che la definizione che matcha REGEX compaia solo in file che
# combaciano con ALLOWED_GLOB. Esclude le directory di build.
assert_single() {
  local label="$1" pattern="$2" allowed="$3"; shift 3
  local dirs=("$@"); [[ ${#dirs[@]} -eq 0 ]] && dirs=(crates apps packages)
  local hits bad=""
  hits="$(grep -rEl --include='*.rs' --include='*.ts' --include='*.tsx' \
    --exclude-dir=target --exclude-dir=node_modules --exclude-dir=.next \
    --exclude-dir=.turbo --exclude-dir=dist \
    -e "$pattern" "${dirs[@]}" 2>/dev/null || true)"
  while IFS= read -r f; do
    [[ -z "$f" ]] && continue
    # shellcheck disable=SC2053
    if [[ "$f" != $allowed ]]; then bad+="  $f"$'\n'; fi
  done <<< "$hits"
  if [[ -n "$bad" ]]; then
    echo "!! single-source [$label]: definizione fuori dal punto unico ($allowed):" >&2
    printf '%s' "$bad" >&2
    fail=1
  else
    echo "OK single-source [$label]"
  fi
}

# --- Check attivi (decommentare alla chiusura della wave corrispondente) ---

# Wave 1:
assert_single "parse_user_id" 'fn parse_user_id' 'crates/nexus-types/src/lib.rs' crates

# Wave 2:
assert_single "TtlCache" 'struct TtlCache' 'crates/nexus-cache/src/lib.rs' crates
assert_single "TemplateCache" 'struct TemplateCache' 'crates/nexus-types/src/templates.rs' crates

# Wave 3:
assert_single "get_setting" 'fn get_setting[^_a-zA-Z]' 'crates/nexus-auth/src/lib.rs' crates

# Scrittura settings (2026-07-16): la scrittura vive accanto alla lettura, in
# nexus-auth. Prima `update_setting` era duplicato in mcp-core e admin-service,
# ed entrambe le copie creavano la chiave assente con un INSERT in categoria
# 'custom': un refuso nel nome produceva una scrittura inefficace spacciata per
# riuscita (il sistema legge la chiave giusta, mai quella col refuso).
# Il primo check protegge la definizione, il secondo il divieto: un handler che
# reintroduca l'INSERT di ripiego lo fa ricomparire fuori dal punto unico.
assert_single "update_setting_value" 'fn update_setting_value' 'crates/nexus-auth/src/lib.rs' crates
assert_single "settings INSERT di ripiego" "INSERT INTO settings \(key, value, category" 'crates/nexus-auth/src/lib.rs' crates/mcp-core/src/settings.rs crates/admin-service/src/settings.rs

# Wave 4 (capability: la fonte DATI e' gia' unica via ADR 0024, vista
# v_model_capabilities; qui si protegge il classificatore di scrittura).
assert_single "classify_capabilities" 'fn classify_capabilities' 'crates/mcp-core/src/model_catalog_sync.rs' crates
assert_single "infer_capabilities_from_name" 'fn infer_capabilities_from_name' 'crates/mcp-core/src/model_catalog_sync.rs' crates

# Wave 5 (registry default: statici nella migrazione 0325; parte dinamica unica):
assert_single "ensure_projects_base_root" 'fn ensure_projects_base_root' 'crates/nexus-types/src/lib.rs' crates

# Wave 8b (error classifier testuale, punto unico Rust):
assert_single "rust classify_text" 'pub fn classify_text' 'crates/mcp-core/src/provider_error_classifier.rs' crates

# Wave 4+5 (2026-06-11): punti unici del consolidamento E1-E6
assert_single "walk FS nexus_tools" 'pub fn walk_project_files' 'crates/nexus-tool-kit/src/fs_scan.rs' crates
assert_single "catalog query Postgres" 'pub fn list_catalog_rows' 'crates/nexus-tool-kit/src/db_helper.rs' crates
assert_single "registrazione progetto" 'pub async fn register_project_records' 'crates/nexus-tool-kit/src/project_register_common.rs' crates
assert_single "endpoint MCP server condivisi" 'pub async fn list_servers_core' 'crates/nexus-mcp-client/src/server_endpoints.rs' crates

# Incidente Beaty-Book (2026-07-02): placeholder di redazione ([REDACTED:...],
# __NEXUS_..._N__) copiati come valori nei tool_input. Punto unico:
# security/redaction_guard.rs; i call site (run_command, run_service,
# enforce_on_write, nexus_db_query) delegano.
assert_single "redaction_guard" 'fn (find_redacted_placeholder|enforce_no_redacted_placeholder)' 'crates/mcp-core/src/security/redaction_guard.rs' crates

# Font tipografico (2026-07-01): punto unico in apps/web-ide/app/layout.tsx
# (next/font/local -> --font-mono). I componenti usano var(--font-mono); vietato
# reintrodurre stack font hardcoded negli inline style. Ricerca confinata a
# apps/web-ide: la webview vscode-ext ha un contesto CSS separato, fuori scope.
# NB: NexusLogo.tsx usa l'attributo SVG fontFamily="monospace" (glifo del logo),
# non catturato dal pattern che richiede "fontFamily:" con i due punti.
assert_single "font web-ide" "ui-monospace|['\"]JetBrains Mono|fontFamily:[[:space:]]*['\"]monospace" 'apps/web-ide/app/layout.tsx' apps/web-ide

# Convergenza pool DB per-progetto (2026-07-03): i mattoni comuni della
# risoluzione (registry project_database_config role nexus_metadata, directory
# nexus_data_routing) vivono SOLO in crates/nexus-project-pools;
# mcp-core::project_db_routes e i servizi delegano.
# I glob ammettono anche i test del crate stesso.
#
# La separazione DB e' SEMPRE attiva: il flag e' stato eliminato (mig 0527) e i
# rami OFF rimossi. Il rischio da presidiare non e' piu' "il flag e' letto fuori
# dal crate" ma "il flag viene REINTRODOTTO", riaprendo un rollback al meta che
# leggerebbe tabelle droppate (mig 0525). Il pattern non richiede piu' il
# prefisso get_setting( — che dopo la rimozione non matchava piu' nulla, e
# rendeva il check verde per assenza di bersaglio: l'unica occorrenza legittima
# e' il doc-comment storico in lib.rs, gia' coperto dal glob del crate.
assert_single "flag separazione DB rimosso (mig 0527)" 'db\.project_separation\.enabled' 'crates/nexus-project-pools/*' crates
assert_single "registry DB metadati progetto" "connection_role = 'nexus_metadata'" 'crates/nexus-project-pools/*' crates
assert_single "directory nexus_data_routing" '(FROM|INTO) nexus_data_routing' 'crates/nexus-project-pools/*' crates

# Derivazione del nome DB fisico del progetto (2026-07-14): viveva in due copie
# (provision.rs + agent_tools/command.rs::sanitize_app_db_name) che troncavano la
# base a 52 e a 56. Entrambi i nomi restavano sotto il NAMEDATALEN di Postgres
# (63), quindi la divergenza non produceva errori: per uno slug oltre 52 caratteri
# il pannello REST e il tool agente creavano DUE database fisici per lo stesso
# progetto. Il guard confina la definizione; la divergenza in se' e' coperta dai
# test di regressione in provision.rs (mod tests).
assert_single "derivazione nome DB progetto" 'fn derive_project_db_name' 'crates/mcp-core/src/project_db_routes/provision.rs' crates

# Richiesta della chat al gateway (2026-07-27): modello, pin del provider
# forzato e coppia prenotata a ledger nascono in UN punto. Prima la chat ne
# teneva una copia inline in execute_via_gateway che prefissava il modello col
# provider (`deepseek/coder-large`) e non valorizzava `pin_provider`: la
# forzatura dell'utente non arrivava al gateway e rispondeva un altro fornitore.
# Sul PREFISSO non c'e' un guard testuale: `format!("{provider}/{model}")` ha
# usi legittimi (etichette di display, `model_used`, e gli adapter che lo
# accoppiano a `pin_provider`, dove il gateway lo strippa). Un guard che gridasse
# su quelli misurerebbe un'altra cosa. A presidiare il prefisso c'e' il test di
# mutazione `provider_forzato_viaggia_come_pin_e_il_modello_non_e_prefissato`.
assert_single "richiesta gateway chat" 'fn build_chat_gateway_call' 'crates/mcp-core/src/orchestrator/model_routing.rs' crates

# Preferenza vs pin del provider (2026-07-27): "quanto vincola il provider che
# l'utente ha scelto?" ha UN vocabolario e UN punto in cui il vincolo duro nasce.
# Prima la forza del vincolo non esisteva sul wire — il pulsante "Forza" del
# composer non arrivava mai al backend — e chi leggeva il solo nome del provider
# doveva DEDURLA. Finche' l'override non aveva effetto la deduzione era innocua;
# col pin funzionante avrebbe reso duro ogni cambio di dropdown, e persistente
# (la preferenza di sessione riproponeva quel nome a ogni messaggio).
# Il primo check protegge il vocabolario, il secondo il divieto: solo
# `ProviderChoice::resolve` puo' coniare un pin, e lo fa dalla richiesta in
# corso — mai da un ricordo. Un `ProviderChoice::Pinned(...)` costruito altrove
# sarebbe di nuovo un vincolo nato senza che l'utente lo dia in quel momento.
assert_single "vocabolario forza-vincolo provider" 'enum ProviderOverrideMode' 'crates/mcp-core/src/orchestrator/provider_choice.rs' crates
# Si cerca la COSTRUZIONE (`::Pinned(`), non il nome: i riferimenti in rustdoc
# (`[ProviderChoice::Pinned]`) documentano il concetto e non coniano nulla.
assert_single "nascita del pin duro" 'ProviderChoice::Pinned\(' 'crates/mcp-core/src/orchestrator/provider_choice.rs' crates

# Applicazione del vincolo di provider (2026-07-27): "questo fornitore e'
# ammesso per il run?" e' UNA domanda con UNA risposta. Nel percorso agentico i
# fornitori nascono in piu' punti — catena di escalation, ripiego cross-provider,
# upscale di finestra, cambio di tier — e la tentazione, ogni volta che se ne
# aggiunge uno, e' di scrivere li' il confronto col fornitore scelto. Il primo
# ramo che se ne dimentica riapre il difetto, e in silenzio: il pin resta
# dichiarato ovunque mentre il run e' gia' altrove. Il predicato vive nel tipo
# (`ProviderPin::ammette`), gli adapter lo chiamano, nessuno lo riscrive.
# Pattern con la parentesi: `fn ammette` da solo pesca anche i nomi che
# COMINCIANO per "ammette" (c'e' gia' un test `ammette_tier_3_e_context_default`
# in nexus-gateway), e un guard che grida su un omonimo misura un'altra cosa.
assert_single "predicato del vincolo provider" 'fn ammette\(' 'crates/mcp-core/src/orchestrator/provider_choice.rs' crates

# Aggregazione problemi ripetitivi (2026-07-09): chiave di gruppo semantica e
# pipeline dedup+raggruppamento del pannello Problemi. Punto unico:
# project_workspace/problem_aggregation.rs; get_project_problems delega.
assert_single "problem_group_key" 'fn problem_group_key' 'crates/mcp-core/src/project_workspace/problem_aggregation.rs' crates
assert_single "aggregate_problems" 'fn aggregate_problems' 'crates/mcp-core/src/project_workspace/problem_aggregation.rs' crates

# Lettura dei run Playwright da `jobs` (2026-07-22): la query viveva in DUE posti
# (endpoint REST e snapshot del dispatcher). Quando `jobs` e' stata migrata al DB
# per-progetto solo la copia in logs.rs e' stata aggiornata: lo snapshot ha
# continuato a leggerla dal meta-pool, dove la tabella non esiste piu', e
# rispondeva 500 a ogni bootstrap del dispatcher. Punto unico:
# logs.rs::playwright_runs_for_project; chi serve quei run delega a quello.
assert_single "lettura run playwright" "kind ILIKE '%playwright%'" 'crates/mcp-core/src/project_workspace/logs.rs' crates

# Listino modelli (2026-07-15): "quanto costa (provider, model)?" e' UNA domanda,
# e la risposta vive solo in crates/nexus-pricing. Era scritta TRE volte (mcp-core,
# nexus-gateway, billing-service) e le copie erano divergenti sui soldi: filtro
# `is_enabled` sulla contabilita' (sottostima sui modelli disabilitati ma chiamati),
# currency di default USD contro EUR (il default EUR aveva gia' prodotto 3.993
# righe di ledger orfane, corrette dalla mig 0294) e `pricing_state` letto da una
# sola delle tre. Nessun compilatore poteva vederlo: sono tre funzioni private
# omonime in crate che non si conoscono.
#
# Il guard NON vieta di leggere `ai_price_catalog` (context_window, elenco
# provider, capability sono letture legittime che non prezzano nulla). Confina due
# cose precise:
#   1. la currency di piattaforma: `billing_base_currency` ha UN solo lettore;
#   2. le funzioni omonime: erano tre `fn resolve_active_price` private in crate
#      diversi — invisibili l'una all'altra, quindi impossibili da confrontare.
# Le CHIAMATE `nexus_pricing::...` restano libere ovunque.
# La chiave si cerca QUOTATA (`"billing_base_currency"` o `'...'` in SQL): cosi'
# il guard becca le letture e non i messaggi d'errore che la nominano per aiutare
# chi legge il log.
pricing_hits="$(grep -rEn \
  "[\"']billing_base_currency[\"']|fn +(resolve_active_price|calculate_cost|get_platform_currency|platform_currency)\b" \
  crates \
  --include='*.rs' \
  2>/dev/null \
  | grep -v '^crates/nexus-pricing/' \
  | grep -vE ':[0-9]+: *(//|/\*|\*)' \
  || true)"
if [[ -n "$pricing_hits" ]]; then
  echo "!! pricing-single-source: listino/currency definiti fuori da nexus-pricing:" >&2
  echo "$pricing_hits" >&2
  echo "   Chiama nexus_pricing::{resolve_active_price, platform_currency, calculate_cost}." >&2
  echo "   Le tre copie storiche divergevano sui soldi (filtro is_enabled, default" >&2
  echo "   USD vs EUR, pricing_state letto da una sola): tenerne UNA e' il punto." >&2
  fail=1
else
  echo "OK pricing-single-source: listino e currency vivono solo in nexus-pricing"
fi

# Contabilita' del ledger (2026-07-27): "quale riga si scrive in ai_usage_ledger"
# e' UNA domanda, e la risposta vive solo in crates/nexus-ledger. Erano QUATTRO
# scrittori in due crate che non si vedevano, con le SQL tenute gemelle a mano —
# il commento sopra `SQL_UPDATE_LEDGER_FINALIZE` lo dichiarava: "Gemella di
# `SQL_INSERT_LEDGER_TESTO` nel gateway". Le divergenze erano gia' in atto e
# tutte sui soldi:
#   1. nessuno dei due sapeva dell'altro -> una chiamata lasciava DUE righe
#      'finalized' (misurato il 27/07/2026: 0.002339 addebitati due volte);
#   2. `ai_quota_policies.cost_limit` e' NUMERIC e sqlx non lo decodifica in f64:
#      il gateway aveva il cast ::float8, mcp-core no. Invisibile finche' nessuno
#      configurava una quota di COSTO, cioe' finche' non c'era una riga da
#      decodificare;
#   3. il marker del job batch dichiarava una currency 'EUR' di propria
#      iniziativa, con la piattaforma su USD.
# Nessun compilatore poteva vederlo: SQL in stringhe, in crate che non si
# conoscono.
#
# Il guard NON vieta di LEGGERE il ledger: report admin, breakdown per run,
# viste analitiche e monitor sono letture di presentazione che non decidono
# nulla. Confina la SCRITTURA (INSERT/UPDATE), che e' dove nasce il denaro.
#
# Ambito: i sorgenti. Nei test la garanzia e' di natura diversa (regola O: un
# test deve attraversare il PRODUTTORE, non ricopiarlo) e non e' un grep a
# darla; oggi il produttore vero e' raggiungibile — prima non lo era, ed e' il
# motivo per cui il crate esiste. Caso noto residuo:
# `crates/mcp-core/tests/m71_cost_breakdown.rs`, che semina righe per verificare
# un LETTORE (il breakdown costi per run) e non uno scrittore.
ledger_hits="$(grep -rEn \
  "(INSERT +INTO|UPDATE) +ai_usage_ledger" \
  crates \
  --include='*.rs' \
  2>/dev/null \
  | grep -v '^crates/nexus-ledger/' \
  | grep -v '/tests/' \
  | grep -vE ':[0-9]+: *(//|/\*|\*)' \
  || true)"
if [[ -n "$ledger_hits" ]]; then
  echo "!! ledger-single-source: scrittura di ai_usage_ledger fuori da nexus-ledger:" >&2
  echo "$ledger_hits" >&2
  echo "   Chiama nexus_ledger::{reserve, record_tokens, record_media, insert_marker," >&2
  echo "   finalize, release, settle}. Le copie storiche divergevano sui soldi:" >&2
  echo "   due righe finalizzate per una chiamata, una quota di costo illeggibile," >&2
  echo "   una currency inventata. Tenerne UNA e' il punto." >&2
  fail=1
else
  echo "OK ledger-single-source: il ledger lo scrive solo nexus-ledger"
fi

# Output fatturabile (2026-07-30): "quanti token di output si pagano" e' UNA
# domanda, e la somma vive solo in nexus-types::token_usage.
#
# Nasce da un difetto che e' stato invisibile per l'intera vita dell'adapter
# Google: `GoogleUsageMetadata` non dichiarava `thoughtsTokenCount`, serde lo
# scartava in silenzio, e `candidatesTokenCount` — che porta il solo testo
# VISIBILE — finiva a ledger come output. Misurato il 30/07/2026 su
# `gemini-2.5-flash`: 3 token visibili contro 157 di pensiero, entrambi
# fatturati da Google alla tariffa di output.
#
# I due lati che pagano sono due (la riga di ledger nel gateway, il freno di
# spesa del run in mcp-core) e nessuno dei due vede l'altro: e' esattamente la
# forma in cui le copie divergono. La somma passa da
# `nexus_types::token_usage::completion_tokens_billable`.
#
# Il guard confina la SOMMA, non la lettura: propagare il campo, mostrarlo o
# scriverlo in una trace non decide un addebito.
billable_hits="$(grep -rEn \
  'reasoning_tokens[^;]*\.unwrap_or\(0\)|(output_tokens|completion_tokens)[^;]*\+[^;]*reasoning_tokens|reasoning_tokens[^;]*\+[^;]*(output_tokens|completion_tokens)' \
  crates \
  --include='*.rs' \
  2>/dev/null \
  | grep -v '^crates/nexus-types/' \
  | grep -vE ':[0-9]+: *(//|/\*|\*)' \
  || true)"
if [[ -n "$billable_hits" ]]; then
  echo "!! output-fatturabile: somma del ragionamento fuori da nexus-types::token_usage:" >&2
  echo "$billable_hits" >&2
  echo "   Chiama nexus_types::token_usage::completion_tokens_billable(output, reasoning)." >&2
  echo "   Sommare a mano in un secondo punto e' come il difetto da cui nasce: chi paga" >&2
  echo "   e' il gateway (riga di ledger) E mcp-core (freno di spesa), e non si vedono." >&2
  echo "   NON sommare dentro output_tokens: is_degenerate_completion misura il testo" >&2
  echo "   PRODOTTO, e un turno hollow smetterebbe di essere riconoscibile." >&2
  fail=1
else
  echo "OK output-fatturabile: la somma del ragionamento vive solo in nexus-types"
fi

# Identificatori canonici (2026-07-09): enum/command identifiers in inglese,
# niente sinonimi IT negli parser Rust (regola CLAUDE.md sezione N).
alias_hits="$(grep -rEn \
  '\| "automatico"|\| "continuo"|\| "conferma"|\| "studio"|from_str_lenient' \
  crates/mcp-core crates/nexus-agent-graph \
  --include='*.rs' \
  2>/dev/null || true)"
if [[ -n "$alias_hits" ]]; then
  echo "!! canonical-identifiers: sinonimi IT o from_str_lenient trovati:" >&2
  echo "$alias_hits" >&2
  fail=1
else
  echo "OK canonical-identifiers: nessun sinonimo IT nei parser enum"
fi

# Scala dei performance_tier (2026-07-15): UN solo posto la conosce, anche per
# chi ordina in SQL. Il vocabolario vive in nexus-agent-graph/decisions/tiers.rs
# (PERFORMANCE_TIERS + tier_rank) e `tier_rank_sql` GENERA da li' l'espressione
# SQL: aggiungere un livello lo porta ovunque.
#
# Perche' esiste questa guard. La scala viveva solo in Rust, quindi ogni query
# che voleva ordinare per capacita' era COSTRETTA a riscriverla a mano — 9 copie.
# Una di queste (agent_run.rs) era rimasta a TRE livelli dopo il passaggio a
# cinque (mig 0528): 'frontier' e 'high' collassavano su 'light', e l'escalation
# "sali al modello piu' capace" scartava i 7 modelli frontier del catalog
# preferendo un medium. Nessun compilatore poteva vederlo: e' una stringa SQL.
# Il difetto e' sopravvissuto per mesi proprio perche' nessuno lo guardava.
#
# Esclusi:
#  - tiers.rs: e' la FONTE che genera l'espressione;
#  - le righe di COMMENTO: la documentazione cita il difetto per spiegarlo
#    (model_service.rs lo riporta verbatim come motivazione del design);
#  - il test-double della vista in escalation_port.rs (#[cfg(test)]): e' uno
#    specchio DICHIARATO della vista viva (mig 0528/0598), che nelle migrazioni
#    ha il CASE a mano perche' l'SQL non puo' chiamare il Rust. Non e' codice di
#    produzione. Sostituirlo con una fixture derivata dalla migrazione reale e'
#    un lavoro suo, gia' annotato dal censimento dei punti unici.
#  - le migrazioni: immutabili e gia' applicate.
tier_case_hits="$(grep -rEn \
  "CASE +(lower\(trim\()?performance_tier" \
  crates apps \
  --include='*.rs' --include='*.ts' --include='*.tsx' \
  2>/dev/null \
  | grep -v 'decisions/tiers.rs' \
  | grep -vE ':[0-9]+: *(//|/\*|\*)' \
  | grep -v 'agent_graph_adapter/escalation_port.rs' \
  || true)"
if [[ -n "$tier_case_hits" ]]; then
  echo "!! tier-scale: scala tier scritta a mano fuori dal vocabolario unico:" >&2
  echo "$tier_case_hits" >&2
  echo "   Usa nexus_agent_graph::decisions::tiers::tier_rank_sql(col): genera" >&2
  echo "   l'espressione da PERFORMANCE_TIERS, cosi' la scala resta UNA." >&2
  fail=1
else
  echo "OK tier-scale: nessuna scala tier scritta a mano fuori da tiers.rs"
fi

# Selezione modelli (2026-07-15): ogni consumatore passa dal SERVIZIO UNICO
# (orchestrator/model_service.rs). La vecchia famiglia a strati
# (best_model_for_tier*, select_agentic_model*, best_non_agentic_model) resta
# solo come implementazione interna: chiamarla da un call site nuovo rimette
# la degradazione in balia di QUALE strato scegli — il difetto che si e' ripetuto
# TRE volte (purpose, fan-out del consiglio, ramo non-agentico), ogni volta
# perche' qualcuno ha chiamato lo strato comodo.
#
# Ammessi: gli strati stessi (model_routing.rs), il servizio, i test, e
# route_model_from_catalog (usa la variante `governed`, non ancora esposta dal
# servizio: e' il gruppo 6e, dichiarato aperto).
selettori_fuori="$(grep -rEn \
  '\b(best_model_for_tier(_excluding|_pinned|_pinned_with_tier)?|select_agentic_model(_pinned|_pinned_with_tier)?|best_non_agentic_model)\(' \
  crates \
  --include='*.rs' \
  2>/dev/null \
  | grep -v 'src/orchestrator/model_routing.rs' \
  | grep -v 'src/orchestrator/model_service' \
  | grep -vE 'tests\.rs|/tests/' \
  | grep -vE ':[0-9]+: *(//|/\*|\*)' \
  || true)"
if [[ -n "$selettori_fuori" ]]; then
  echo "!! model-selection: call site che scavalca il servizio unico:" >&2
  echo "$selettori_fuori" >&2
  echo "   Usa orchestrator::model_service::select_model(db, &ModelRequest{..}):" >&2
  echo "   la degradazione e' un PARAMETRO (TierPolicy), non un effetto di quale" >&2
  echo "   funzione chiami, e il gate segue il Profile invece della diligenza." >&2
  fail=1
else
  echo "OK model-selection: nessun call site scavalca il servizio unico"
fi

# Scala relativa dei tier (2026-07-19, mig 0614): la banda dal leader si
# calcola in UN solo posto. Il prior dell'indice e le bande measured della
# batteria delegano entrambi a tier_from_leader: due copie della scala
# divergerebbero in silenzio come le due formulazioni della precedenza fonti.
assert_single "tier_from_leader" 'fn tier_from_leader\(' 'crates/mcp-core/src/orchestrator/model_service.rs' crates

# Scrittura del tier (2026-07-16): performance_tier + tier_source si scrivono
# SOLO da model_service::apply_tier, che conosce la precedenza delle fonti
# (manual > measured > synced > fonte ignota). Esteso il 2026-07-19 (mig 0615):
# anche measured_score si scrive solo dal gemello apply_measured_score, nello
# stesso modulo — lo score e' confrontabile solo se lo scrive una mano sola.
#
# Perche' un guard e non la sola buona volonta': la regola e' gia' stata scritta
# due volte, in due linguaggi diversi e lontani — una WHERE in
# `refresh_tier_prior` ("tocca solo NULL o facts_prior") e un CASE dentro
# SQL_QUALIFIED ("salta se manual") — e reggevano solo finche' restavano
# allineate a mano. Il doppione del tier si e' gia' ripresentato una volta
# (models.rs derivava il tier dal solo prezzo mentre l'altro punto aveva anche
# l'indice: vinceva il meno informato). Un terzo writer domani non deve poter
# nascere per distrazione.
#
# Ammessi: il servizio stesso, i test, e le migrazioni (immutabili, gia'
# applicate: una migrazione SQL non puo' chiamare Rust).
tier_writers="$(grep -rEn \
  "(UPDATE|SET)[^\"']*\b(performance_tier|tier_source|measured_score) *=" \
  crates apps \
  --include='*.rs' --include='*.ts' --include='*.tsx' \
  2>/dev/null \
  | grep -v 'src/orchestrator/model_service' \
  | grep -vE 'tests\.rs|/tests/' \
  | grep -vE ':[0-9]+: *(//|/\*|\*)' \
  || true)"
if [[ -n "$tier_writers" ]]; then
  echo "!! tier-write: scrittura del tier fuori dal punto unico:" >&2
  echo "$tier_writers" >&2
  echo "   Usa orchestrator::model_service::apply_tier(exec, provider, model," >&2
  echo "   tier, TierSource::{Synced|Measured|Manual}): la precedenza fra le" >&2
  echo "   fonti vive li' ed e' testata una volta sola." >&2
  fail=1
else
  echo "OK tier-write: il tier si scrive solo dal punto unico (apply_tier)"
fi

# Il nome del modello e' OPACO (2026-07-16): la slash in `openai/gpt-oss-120b`
# (groq) o `z-ai/glm-5.2` (openrouter) e' parte del NOME pubblicato dal
# provider, non il separatore della nostra convenzione `provider/modello`.
# Toglierla senza verificare che il prefisso sia DAVVERO il provider produce un
# nome mutilato e un 404: misurato contro l'API di groq,
#   openai/gpt-oss-120b -> 200 | gpt-oss-120b -> 404.
#
# La regola viveva in due copie divergenti (`strip_model_prefix` in routes.rs
# strippava alla cieca, `strip_provider_prefix` nel resolver alias no) e il
# commento della prima ammetteva "allineato a ... qui in locale". Ora e' una
# sola funzione, e questo guard impedisce che ne nasca un'altra.
split_cieco="$(grep -rEn \
  "(model|modello)[a-z_]*\.split(_once)?\('/'\)" \
  crates/nexus-gateway/src \
  --include='*.rs' \
  2>/dev/null \
  | grep -v 'model_alias_resolver.rs' \
  | grep -vE ':[0-9]+: *(//|/\*|\*)' \
  || true)"
if [[ -n "$split_cieco" ]]; then
  echo "!! model-name-opaco: split del nome modello fuori dal punto unico:" >&2
  echo "$split_cieco" >&2
  echo "   Il nome di un modello e' OPACO: una slash NON significa provider/modello" >&2
  echo "   (groq: openai/gpt-oss-120b, openrouter: z-ai/glm-5.2 — la slash e' nel nome)." >&2
  echo "   Usa model_alias_resolver::strip_provider_prefix(model, provider): toglie il" >&2
  echo "   prefisso solo se e' davvero il provider di destinazione." >&2
  fail=1
else
  echo "OK model-name-opaco: il nome del modello si splitta solo nel punto unico"
fi

# ── regola O: la diagnostica chiama il codice, non lo imita ──────────────────
# Uno script che RICOPIA una query di produzione e' un punto unico violato: la
# copia divergera' in silenzio. Accaduto il 2026-07-17: uno script diagnostico
# aveva ricopiato SQL_CLAIM leggendo la suite dalla tabella sbagliata e diceva
# "0 candidati" mentre erano 29. Un numero sbagliato con la faccia seria vale meno
# di nessun numero.
#
# Il pattern copre le colonne E le tabelle dell'eleggibilita': con le sole due
# colonne di partenza, uno script che ricopiava la regola guardando lo stato di
# qualificazione o la tabella dei profili passava indisturbato — cioe' proprio
# l'errore accaduto, che stava nella scelta della TABELLA della suite.
sql_ricopiato="$(grep -rIlE "qualification_suite_version|qualification_backoff_until|qualification_expires_at|qualification_state|ai_model_probe_profile"   --include=*.py --include=*.sh --include=*.ps1 --include=*.js --include=*.ts   scripts/ tools/ 2>/dev/null | grep -v "check-single-source.sh" || true)"
if [[ -n "$sql_ricopiato" ]]; then
  echo "!! diagnostica-non-imita: uno script ricopia la query del claim della batteria:" >&2
  echo "$sql_ricopiato" >&2
  echo "   La copia divergera' dal codice e mentira'. Chiedi al sistema invece di" >&2
  echo "   riscrivere la domanda (regola O). La strada esiste:" >&2
  echo "     cargo xtask battery-explain            chi e' eleggibile ADESSO" >&2
  echo "     cargo xtask battery-explain <modello>  perche' si', perche' no" >&2
  fail=1
else
  echo "OK diagnostica-non-imita: nessuno script ricopia la query del claim"
fi

# ── regola O: un test attraversa il produttore ───────────────────────────────
# Il Value del turno agentico nasce SOLO da agent_turn_value_from_gw. Un test che
# lo fabbrica a mano fissa l'assunto che dovrebbe verificare: e' l'incidente
# turn['result'] (chiave che nessuno scriveva -> content_chars 0 per costruzione,
# test verdi, modelli sani bocciati). Chi ha bisogno di un turno finto parte dal
# produttore, o dichiara perche' non puo'.
turno_a_mano="$(grep -rIn "tool_use_blocks\"\s*:" --include=*.rs crates/ 2>/dev/null   | grep -v "neural_client.rs"   | grep -v "probe_agentic_loop.rs"   | grep -v "model_qualification.rs"   || true)"
if [[ -n "$turno_a_mano" ]]; then
  echo "!! turno-dal-produttore: un turno agentico e' costruito a mano fuori dai punti noti:" >&2
  echo "$turno_a_mano" >&2
  echo "   Il Value del turno nasce da agent_turn_value_from_gw: un test che lo" >&2
  echo "   fabbrica condivide l'errore col codice e resta verde per sempre (regola O)." >&2
  fail=1
else
  echo "OK turno-dal-produttore: il turno agentico non si fabbrica a mano"
fi

# ── il vincolo di provider raggiunge le porte del run ────────────────────────
# Le due porte che possono cambiare fornitore in corsa nascono legate al vincolo
# dell'utente (`.con_vincolo`) nel punto che le costruisce per il run. Se qualcuno
# togliesse quella chiamata, il codice COMPILEREBBE e i test delle porte
# resterebbero verdi (le costruiscono da se'): il vincolo sparirebbe in silenzio,
# senza errori, cambiando solo il fornitore su cui gira il run. E' il difetto
# originale, e la giunzione che lo riapre non ha un test suo — `build_native_engine`
# assembla quattordici impl e richiede DB + ToolRunnerDeps reali. Qui c'e' il
# presidio che manca li'.
#
# LA FINESTRA, e perche' non la contiguita': fino al 28/07 questo check cercava la
# stringa `${porta}::new(db.clone()).con_vincolo(` su UNA riga. Misurava una FORMA
# TESTUALE, non il fatto, e ha due modi di sbagliare. Il primo e' rumoroso e si e'
# manifestato: aggiungendo `.con_veto(...)` alla catena, rustfmt l'ha spezzata su
# tre righe e il presidio ha respinto un commit in cui il vincolo c'era — con
# l'effetto peggiore di indurre a piegare il CODICE alla forma che il grep sa
# leggere. Il secondo e' silenzioso: un presidio che si accontenta della forma si
# aggira senza volerlo. Ora la chiamata si cerca entro una FINESTRA di righe dalla
# costruzione, cosi' sopravvive a qualunque riformattazione.
#
# La finestra e' 6 e non piu' larga per una ragione misurata: in native_engine.rs
# le due porte nascono a ~10 righe di distanza, e una finestra che le sconfina
# dichiarerebbe vincolata una porta leggendo la riga dell'ALTRA — il presidio
# tornerebbe verde proprio nel caso che deve prendere.
FINESTRA_VINCOLO=6
vincolo_scollegato=""
for porta in PgEscalationPort CatalogModelUpscalePort; do
  # `grep -A` porta con se' le righe successive: la costruzione e le sue chiamate
  # in catena stanno nello stesso blocco qualunque sia l'andata a capo.
  blocco="$(grep -A"$FINESTRA_VINCOLO" "${porta}::new(db" \
    crates/mcp-core/src/native_engine.rs 2>/dev/null || true)"
  if [[ -z "$blocco" ]]; then
    vincolo_scollegato+="  ${porta}: nessuna costruzione trovata in native_engine.rs"$'\n'
  elif ! printf '%s' "$blocco" | grep -q '\.con_vincolo('; then
    vincolo_scollegato+="  ${porta}: costruita senza .con_vincolo(input.provider_pin)"$'\n'
  fi
done
if [[ -n "$vincolo_scollegato" ]]; then
  echo "!! vincolo-alle-porte: il pin dell'utente non raggiunge chi sceglie il fornitore:" >&2
  printf '%s' "$vincolo_scollegato" >&2
  echo "   Senza il vincolo la porta e' libera di ripiegare su un altro fornitore" >&2
  echo "   mentre la UI continua a dichiarare il pin (ADR 0023, aggiornamento 3)." >&2
  fail=1
else
  echo "OK vincolo-alle-porte: le porte del run nascono col vincolo dell'utente"
fi

# ── il veto del sistema raggiunge la porta di escalation ─────────────────────
# Gemello del check sopra, per il vincolo di segno OPPOSTO: «giudice != worker».
# La porta di escalation nasce anche col veto (`.con_veto`), altrimenti un sub-run
# di review che cade ripiega sul fornitore del worker — cioe' il giudice torna a
# girare sul modello che ha scritto il codice da giudicare. Misurato il 26/07/2026
# (run 609000c1): 10 revisori scelti su openrouter, le loro trace su
# deepseek-v4-flash e deepseek-v4-pro, cioe' il fornitore del padre.
#
# Serve un presidio SUO per la stessa ragione del vincolo: i test delle porte le
# costruiscono da se' e resterebbero verdi senza il veto. Solo la porta di
# escalation: l'upscale sale di modello dentro il tier gia' scelto e non e' la
# strada da cui il giudice tornava sul worker.
veto_scollegato=""
blocco_escalation="$(grep -A"$FINESTRA_VINCOLO" 'PgEscalationPort::new(db' \
  crates/mcp-core/src/native_engine.rs 2>/dev/null || true)"
if [[ -z "$blocco_escalation" ]]; then
  veto_scollegato="  PgEscalationPort: nessuna costruzione trovata in native_engine.rs"
elif ! printf '%s' "$blocco_escalation" | grep -q '\.con_veto('; then
  veto_scollegato="  PgEscalationPort: costruita senza .con_veto(input.provider_veto)"
fi
if [[ -n "$veto_scollegato" ]]; then
  echo "!! veto-alle-porte: il veto del sistema non raggiunge chi sceglie il fornitore:" >&2
  echo "$veto_scollegato" >&2
  echo "   Senza il veto un sub-run di review puo' ripiegare sul fornitore del" >&2
  echo "   worker: il giudice torna a girare sul modello che ha scritto il codice." >&2
  echo "   Punto unico della regola: veto_del_giudice in subagent_native.rs." >&2
  fail=1
else
  echo "OK veto-alle-porte: la porta di escalation nasce col veto del giudice"
fi

# ── sizing dei pool verso il DB per-progetto ─────────────────────────────────
# Lo stesso DB <slug>_nexus veniva aperto con due tetti decisi in due punti che
# si ignoravano: 5 sul percorso caldo di mcp-core, 3 in nexus-project-pools. Una
# patch applicata all'uno era un no-op sull'altro. Ora il tetto vive solo in
# nexus_project_pools::sizing. Il guard e' ristretto alle due strade del DB di
# progetto: i pool verso il cluster admin o verso DB definiti dall'utente
# (provisioning, cleanup, connessioni utente) restano legittimi e non sono qui.
pool_sizing_hits="$(grep -rEn '\.max_connections\(' \
  crates/nexus-project-pools/src \
  --include='*.rs' \
  2>/dev/null \
  | grep -v '^crates/nexus-project-pools/src/sizing.rs:' \
  || true)"
if ! grep -q 'sizing::project_pool_options' \
  crates/mcp-core/src/project_db_routes/provision.rs 2>/dev/null; then
  pool_sizing_hits="${pool_sizing_hits}
crates/mcp-core/src/project_db_routes/provision.rs: non delega piu' a sizing::project_pool_options"
fi
if [[ -n "${pool_sizing_hits// /}" ]]; then
  echo "!! project-pool-sizing: il tetto del pool per-progetto e' deciso fuori dal punto unico:" >&2
  echo "$pool_sizing_hits" >&2
  echo "   Usa nexus_project_pools::sizing::project_pool_options()." >&2
  echo "   Due sizing per lo stesso DB significa che chi ne cambia uno crede di" >&2
  echo "   aver cambiato il comportamento, e non ha cambiato nulla." >&2
  fail=1
else
  echo "OK project-pool-sizing: un solo tetto per il DB per-progetto"
fi

# ── registro dei pool per-progetto ───────────────────────────────────────────
# Il guard qui sopra era VERDE il 2026-07-22, mentre il cluster app era saturo:
# copriva il tetto per pool, e il tetto era rispettato: a crescere era il NUMERO
# dei pool. mcp-core teneva la propria cache (TtlCache 600s) e nexus-project-pools
# un'altra (300s), entrambe nello stesso processo, entrambe verso <slug>_nexus:
# 15 connessioni su un solo database, tre pool. Misurare la cosa sbagliata e'
# peggio che non misurare, perche' il verde diventa una prova a discarico
# (regola O: lo strumento non raggiungeva il suo oggetto).
#
# Ora il registro e' uno solo: nexus_project_pools::pool_or_open. Questo check
# fallisce se ricompare una mappa da progetto a pool fuori di li'.
pool_registry_hits="$(grep -rEn '(TtlCache|HashMap|DashMap|BTreeMap)<\s*Uuid\s*,[^>]*PgPool' \
  crates \
  --include='*.rs' \
  --exclude-dir=target \
  2>/dev/null \
  | grep -v '^crates/nexus-project-pools/src/lib.rs:' \
  || true)"
if [[ -n "${pool_registry_hits// /}" ]]; then
  echo "!! project-pool-registry: un secondo registro di pool per-progetto:" >&2
  echo "$pool_registry_hits" >&2
  echo "   Usa nexus_project_pools::pool_or_open() / cached_pool()." >&2
  echo "   Il tetto per pool non governa quanti pool esistono: due registri" >&2
  echo "   verso lo stesso database raddoppiano le connessioni, e il ruolo" >&2
  echo "   Postgres si esaurisce per TUTTI i progetti insieme." >&2
  fail=1
else
  echo "OK project-pool-registry: un solo registro di pool per-progetto"
fi

# ── ciclo di vita dei pool per-progetto ──────────────────────────────────────
# Un pool NON e' un dato che scade: e' una risorsa. Con un TTL, alla scadenza
# `get` risponde None SENZA rimuovere la entry, il chiamante ne apre uno nuovo e
# il vecchio resta vivo finche' l'ultima PgPool clonata (che un run tiene per
# tutta la sua durata) non viene droppata. E' cosi' che lo stesso database si
# ritrovava con due e tre pool.
if grep -qE 'TtlCache' crates/nexus-project-pools/src/lib.rs 2>/dev/null \
  && grep -qE 'TtlCache::new' crates/nexus-project-pools/src/lib.rs 2>/dev/null; then
  echo "!! project-pool-lifetime: il registro dei pool e' tornato a scadenza TTL:" >&2
  echo "   crates/nexus-project-pools/src/lib.rs" >&2
  echo "   Alla scadenza il pool viene RIAPERTO e il precedente sopravvive nelle" >&2
  echo "   clone in uso: il numero di pool cresce da solo. L'invalidazione di un" >&2
  echo "   pool e' esplicita (forget_pool), mai temporale." >&2
  fail=1
else
  echo "OK project-pool-lifetime: il registro dei pool non scade a tempo"
fi

# ── migrazioni-numero-unico ──────────────────────────────────────────────────
# Il numero di versione di una migrazione e' un punto unico per definizione:
# due file con lo stesso prefisso numerico e nomi diversi passano il merge di
# git senza conflitto, e mcp-core rifiuta di AVVIARSI ("migration N was
# previously applied but has been modified"). TRE collisioni in due giorni
# (0620, 0622, 0624): sessioni parallele che scelgono lo stesso numero libero.
# Vale per db/migrations e per db/migrations/project (numerazioni separate).
for dir in db/migrations db/migrations/project; do
  [[ -d "$dir" ]] || continue
  dup="$(ls "$dir" 2>/dev/null | grep -E '^[0-9]+_.*\.sql$' | sed -E 's/^([0-9]+)_.*/\1/' | sort | uniq -d)"
  if [[ -n "$dup" ]]; then
    echo "!! migrazioni-numero-unico: numeri DUPLICATI in $dir:" >&2
    for n in $dup; do ls "$dir" | grep -E "^${n}_" | sed 's/^/     /' >&2; done
    echo "   Due migrazioni con lo stesso numero: il servizio non si avvia." >&2
    echo "   Rinumera la piu' recente al primo numero libero." >&2
    fail=1
  fi
done
if [[ -z "${dup:-}" ]]; then
  echo "OK migrazioni-numero-unico: nessun numero di migrazione duplicato"
fi

# ── purpose-esito-tipizzato (2026-07-20) ─────────────────────────────────────
# Regola M: l'esito della risoluzione purpose→modello si decide sul TIPO
# (nexus_types::purpose::PurposeUnresolved, downcast/match sulla variante),
# mai col parsing del testo del messaggio. Due incidenti reali:
# learned_instructions interrompeva il batch su contains("purpose") (falsi
# positivi su qualunque errore che citasse la parola) e code_docs_enricher
# cercava "purpose non configurato", sottostringa MAI prodotta dal punto unico
# (il messaggio reale ha il nome del purpose in mezzo): break morto in silenzio.
assert_single "purpose classificato dal testo" '\.contains\("purpose' 'crates/nexus-types/src/purpose.rs' crates

# ── migrazioni-search-path ───────────────────────────────────────────────────
# sqlx esegue la migrazione e l'INSERT di registrazione in _sqlx_migrations
# nella STESSA transazione: una migrazione che manipola il search_path (residuo
# del preambolo pg_dump: set_config('search_path','',...) o SET search_path)
# fa fallire quell'INSERT non qualificato con "relazione non esiste" e nessun
# DB vergine puo' piu' essere migrato (incidente vendita-immobile 20/07).
sp_hits="$(grep -rniE "set_config\('search_path'|SET search_path" db/migrations --include='*.sql' 2>/dev/null | grep -vE ':[0-9]+:\s*--' || true)"
if [[ -n "$sp_hits" ]]; then
  echo "!! migrazioni-search-path: manipolazione del search_path in una migrazione:" >&2
  echo "$sp_hits" | sed 's/^/     /' >&2
  echo "   Rompe l'INSERT di registrazione del migrator sqlx (stessa transazione)." >&2
  echo "   Rimuovi la riga: gli oggetti vanno qualificati con lo schema esplicito." >&2
  fail=1
else
  echo "OK migrazioni-search-path: nessuna migrazione manipola il search_path"
fi

# ── error-presentation (2026-07-22) ──────────────────────────────────────────
# Un errore si RENDE leggibile in un solo posto
# (nexus-types/src/error_presentation.rs), partendo dai segnali strutturati.
# Il difetto storico: mancava il punto unico della PRESENTAZIONE (esisteva solo
# quello della classificazione), quindi ogni superficie usava il Display degli
# errori tipizzati -- che per contratto porta il body grezzo -- e da li' sono
# nate quattro funzioni gemelle che tagliavano CARATTERI invece di tradurre
# (compact_provider_error alla prima graffa, humanize_ai_error sulla prima riga,
# format_compact_error a 300 char, humanizeTraceText a regex). Erano cieche a
# ogni Debug senza graffa: il MetadataMap di tonic e la catena
# io(ConnectionRefused, os_error=10061) arrivavano intatti in chat.
#
# Due divieti, entrambi sintomi dello stesso errore di progetto:
# 1) nuove funzioni di compattazione testuale fuori dal modulo autoritativo;
# 2) regex che riconoscono un errore dalla FORMA del suo Debug (regola M).
ep_hits="$(grep -rnE "fn (compact_|humanize_|format_compact_)[a-z_]*error" crates --include='*.rs' 2>/dev/null \
  | grep -v 'crates/nexus-types/src/error_presentation.rs' || true)"
if [[ -n "$ep_hits" ]]; then
  echo "!! error-presentation: una resa d'errore fuori dal punto unico:" >&2
  echo "$ep_hits" | sed 's/^/     /' >&2
  echo "   Il messaggio si costruisce da ErrorFacts via render_user_error" >&2
  echo "   (crates/nexus-types/src/error_presentation.rs), non tagliando caratteri." >&2
  fail=1
else
  echo "OK error-presentation: nessuna resa d'errore duplicata nei crate"
fi

ep_regex="$(grep -rnE "MetadataMap|grpc[_ -]?status|details:\\\\s\*\\\\\[" crates apps --include='*.rs' --include='*.ts' --include='*.tsx' 2>/dev/null \
  | grep -vE 'crates/nexus-types/src/error_presentation.rs|/(node_modules|\.next)/|scripts/' \
  | grep -vE ':[0-9]+:\s*(//|\*|#)' || true)"
if [[ -n "$ep_regex" ]]; then
  echo "!! error-presentation: si riconosce un errore dalla FORMA del suo Debug:" >&2
  echo "$ep_regex" | sed 's/^/     /' >&2
  echo "   Regola M: lo stato tecnico si legge da status/code/enum alla fonte," >&2
  echo "   non da una regex sul testo gia' appiattito." >&2
  fail=1
else
  echo "OK error-presentation: nessun riconoscimento d'errore dal testo"
fi

# ── schema-di-test-dalla-migrazione (2026-07-22) ─────────────────────────────
# Lo schema su cui gira un test del dominio run/chat deriva dalla MIGRAZIONE
# reale (nexus_migrations_embedded::PROJECT_MIGRATOR, il migrator del set
# db/migrations/project che la produzione applica al DB <slug>_nexus), mai da un
# CREATE TABLE ricopiato a mano nel modulo di test.
#
# Il difetto (regola O): una fixture scritta a mano non fallisce quando diverge —
# mente in silenzio. `nexus_agent_todos` ne aveva DUE, diverse fra loro e dalla
# migrazione (seq INTEGER vs BIGINT, depends_on UUID[] vs TEXT[], content NOT
# NULL vs nullable), entrambe senza project_id, senza i CHECK e senza la FK verso
# nexus_agent_plans: hanno retto finche' una query di produzione non ha chiesto
# una colonna che non avevano (acceptance_criteria). Convertendo i test allo
# schema reale sono emerse righe che il DB di produzione rifiuta e che i test
# creavano da anni: run senza sessione, todo senza piano, step senza tool_input.
#
# L'elenco delle tabelle protette si LEGGE dal set di migrazioni (non e' una
# lista ricopiata qui, che divergerebbe a sua volta).
proj_tables="$(grep -hoE 'CREATE TABLE (IF NOT EXISTS )?(public\.)?[a-z_]+' db/migrations/project/*.sql 2>/dev/null \
  | awk '{print $NF}' | sed 's/^public\.//' | sort -u)"
if [[ -z "$proj_tables" ]]; then
  echo "!! schema-di-test: nessuna tabella letta da db/migrations/project (set mancante?)" >&2
  fail=1
else
  schema_hits=""
  for t in $proj_tables; do
    # I commenti che CITANO una vecchia fixture (la doc dei seeder lo fa) non
    # sono codice: si filtrano come negli altri check.
    h="$(grep -rnE "CREATE TABLE (IF NOT EXISTS )?(public\.)?$t[[:space:](]" crates --include='*.rs' \
      --exclude-dir=target 2>/dev/null | grep -vE ':[0-9]+:\s*(//|/\*|\*)' || true)"
    [[ -n "$h" ]] && schema_hits+="$h"$'\n'
  done
  if [[ -n "$schema_hits" ]]; then
    echo "!! schema-di-test: tabella del set project ricreata a mano in un test:" >&2
    printf '%s' "$schema_hits" | sed 's/^/     /' >&2
    echo "   Usa lo schema reale:" >&2
    echo "     #[sqlx::test(migrator = \"nexus_migrations_embedded::PROJECT_MIGRATOR\")]" >&2
    echo "   e semina le righe coi seeder (mcp_core::test_support::seed_*)." >&2
    fail=1
  else
    echo "OK schema-di-test: nessuna tabella del set project ricreata a mano"
  fi
fi

# ── fixture-settings (2026-08-05) ───────────────────────────────────────────
# Gemello del check sopra per una tabella META: `settings` non e' nel set
# project, quindi PROJECT_MIGRATOR non la porta e la fixture esplicita resta
# necessaria — ma UNA sola, in mcp_core::test_support.
#
# Il difetto misurato: quattordici `mod tests` di mcp-core la ricreavano a mano,
# gia' divergenti fra loro (quattro colonne in ui_flags, tre in
# governance_telemetry, due altrove, e un `value TEXT` NULLABLE in native_engine
# dove la mig 0002 dice `NOT NULL DEFAULT ''`). Il secondo motivo e' la cache:
# `nexus_auth::get_setting` legge attraverso una cache di PROCESSO con TTL 60s,
# quindi un seed scritto con una query propria resta invisibile alla rilettura
# per piu' della durata dell'intera suite. Quattro moduli l'avevano scoperto da
# soli e invalidavano a mano; il quinto (agent_tools::testing) se n'era
# dimenticato. Il punto unico invalida accanto al seed, come fa la produzione in
# `update_setting_value`.
#
# COSA NON COPRE: le scritture diverse dalla creazione (un `UPDATE settings`
# nudo dentro un test su DB migrato resta possibile). Il check e' sulla forma
# che si puo' riconoscere senza falsi positivi; il resto lo tiene la fixture,
# che e' l'unica strada comoda.
settings_hits="$(grep -rnE 'CREATE TABLE (IF NOT EXISTS )?(public\.)?settings[[:space:](]' \
  crates/mcp-core --include='*.rs' --exclude-dir=target 2>/dev/null \
  | grep -v 'src/test_support.rs' | grep -vE ':[0-9]+:\s*(//|/\*|\*)' || true)"
if [[ -n "$settings_hits" ]]; then
  echo "!! fixture-settings: tabella 'settings' ricreata a mano in un test di mcp-core:" >&2
  printf '%s' "$settings_hits" | sed 's/^/     /' >&2
  echo "   Delega al punto unico (regola L):" >&2
  echo "     crate::test_support::create_settings_table(&pool).await;" >&2
  echo "     crate::test_support::seed_setting(&pool, chiave, valore).await;" >&2
  echo "   Il seed invalida la cache di processo: senza, la rilettura vede il" >&2
  echo "   valore precedente per 60 secondi (regola F, test indipendenti)." >&2
  fail=1
else
  echo "OK fixture-settings: la tabella settings nasce dal punto unico"
fi

# ── migrazione-stub ─────────────────────────────────────────────────────────
# Una migrazione il cui corpo e' solo `SELECT 1;` occupa un numero di versione
# senza contenerne lo schema: e' informazione distrutta in modo irrecuperabile.
# Le quattro della serie 0104-0107 furono svuotate al bootstrap del monorepo
# perche' i loro oggetti erano gia' presenti sul DB di sviluppo; l'originale non
# esiste in nessun ramo (git log --follow da' un solo commit, e li' il file e'
# gia' stub). Risultato: per anni un DB ricostruito da zero non ha ricevuto
# `nexus_quality_scans` ne' le colonne vettoriali di
# `project_quality_findings`, e nessuno strumento poteva vederlo — uno stub non
# fallisce, lascia solo un buco. Il ripristino ha richiesto un'indagine e una
# migrazione nuova (0637), scritta leggendo lo schema dal DB vivo.
#
# Le quattro storiche sono immutabili e restano dove sono: il check le esclude
# per numero e blocca solo le NUOVE.
#
# La lettura e' un solo processo `awk`, non un loop di fork per file. La versione
# precedente ne apriva quattro per migrazione (basename, sed, tr, tr): su ~640
# file e su Windows, dove un fork costa 100-200ms, il check da solo impiegava
# 4m48s a ogni commit che tocca un .rs — la stessa forma gia' vista in
# port_enforcer (9.9s -> 21ms sostituendo i fork con una syscall). La regola del
# corpo utile e' invariata (via da ogni riga il commento `--`, poi ogni spazio,
# confronto in minuscolo con `select1;`): cambia solo chi la applica.
#
# I nomi arrivano ad awk sullo standard input e NON sulla riga di comando: 640
# path sforano il limite di lunghezza della command line di Windows (32767
# caratteri), e un `awk ... db/migrations/*.sql` fallirebbe al crescere del set.
# Un path che non esiste (glob senza match) fa fallire subito `getline`, lascia
# il corpo vuoto e non produce hit, come faceva prima il test `-f`.
stub_hits="$(
  printf '%s\n' db/migrations/*.sql db/migrations/project/*.sql \
  | awk '
      /\/010[4-7]_/ { next }   # le quattro storiche: immutabili, escluse per numero
      {
        corpo = ""
        while ((getline riga < $0) > 0) {
          sub(/--.*/, "", riga)
          gsub(/[[:space:]]/, "", riga)
          corpo = corpo riga
        }
        close($0)
        if (tolower(corpo) == "select1;") print
      }
    '
)"
if [[ -n "$stub_hits" ]]; then
  echo "!! migrazione-stub: migrazione con corpo 'SELECT 1;' (nessuno schema dentro):" >&2
  printf '%s\n' "$stub_hits" | sed 's/^/     /' >&2
  echo "   Una migrazione deve contenere il DDL che dichiara, anche se l'oggetto" >&2
  echo "   esiste gia' sul TUO DB: usa IF NOT EXISTS / ADD COLUMN IF NOT EXISTS," >&2
  echo "   cosi' un DB ricostruito da zero lo riceve e uno popolato non cambia." >&2
  fail=1
else
  echo "OK migrazione-stub: nessuna nuova migrazione svuotata a 'SELECT 1;'"
fi

# ── test-provider-live-onesto ───────────────────────────────────────────────
# I test di parita' provider (crates/nexus-gateway/tests/provider_tool_loop.rs)
# chiamano le API reali e saltano quando la chiave non c'e'. Per anni lo skip era
# `eprintln!("skip x: no key"); return`, cioe' un VERDE: in CI le chiavi non
# esistono (nessun workflow del repo referenzia `secrets.`, a parte il job
# dedicato provider-live.yml), quindi quei test non hanno mai toccato un provider
# mentre l'intestazione dichiarava di catturare i 400 di DeepSeek e Anthropic.
# Uno skip indistinguibile da un successo e' peggio di un test assente: dava
# copertura percepita proprio sui difetti che continuavano a costare run.
#
# Il file ha ora un punto unico, `chiave_provider`, che sotto
# REQUIRE_PROVIDER_TESTS=1 fallisce e altrimenti stampa un marker contato dalla
# sentinella `copertura_live_dichiarata`. Questo check impedisce di rientrare
# nello skip muto: una lettura di chiave fuori dal punto unico sarebbe invisibile
# sia alla sentinella sia al conteggio.
live_test="crates/nexus-gateway/tests/provider_tool_loop.rs"
if [[ -f "$live_test" ]]; then
  live_hits=""

  # 1. Nessun nome di chiave provider scritto a mano fuori dalla tabella: nella
  #    versione onesta `leggi_chiave` riceve solo le variabili che vengono da
  #    PROVIDER_KEYS, quindi un letterale *_API_KEY nel corpo di un test e' per
  #    definizione un accesso che scavalca il punto unico (e con esso il marker,
  #    il conteggio della sentinella e il fallimento sotto REQUIRE).
  if grep -nE '(env::var|leggi_chiave)\("[A-Z_]*API_KEY"' "$live_test" >/dev/null; then
    live_hits+="chiave provider letta con un nome letterale invece che via chiave_provider()"$'\n'
  fi

  # 2. Nessuno skip stampato a mano: il marker lo emette il punto unico.
  if grep -nE 'eprintln!\("skip' "$live_test" >/dev/null; then
    live_hits+="skip stampato a mano (eprintln \"skip...\"): usare chiave_provider(), che emette il marker contato"$'\n'
  fi

  # 3. Il punto unico e la sentinella devono esistere.
  grep -q 'fn chiave_provider' "$live_test" ||
    live_hits+="manca il punto unico chiave_provider()"$'\n'
  grep -q 'fn copertura_live_dichiarata' "$live_test" ||
    live_hits+="manca la sentinella copertura_live_dichiarata() che dichiara il conteggio"$'\n'

  # 4. Ogni etichetta della tabella PROVIDER_KEYS deve avere il suo test: una
  #    riga in tabella senza test gonfierebbe il denominatore della copertura
  #    (n/5) senza che nessuno chiami quel provider.
  while read -r etichetta; do
    [[ -n "$etichetta" ]] || continue
    grep -q "fn ${etichetta}_tool_loop" "$live_test" ||
      live_hits+="provider '${etichetta}' in PROVIDER_KEYS senza il suo fn ${etichetta}_tool_loop"$'\n'
  done < <(sed -n '/PROVIDER_KEYS/,/^];/p' "$live_test" |
    grep -oE '^\s*\("[a-z0-9_]+"' | grep -oE '"[a-z0-9_]+"' | tr -d '"')

  if [[ -n "$live_hits" ]]; then
    echo "!! test-provider-live-onesto: lo skip dei test di parita' provider puo' tornare invisibile:" >&2
    printf '%s' "$live_hits" | sed 's/^/     /' >&2
    echo "   Vedi l'intestazione di $live_test: un test che salta senza dirlo e" >&2
    echo "   un test che ha interrogato il provider sono lo stesso verde (regola O)." >&2
    fail=1
  else
    echo "OK test-provider-live-onesto: skip provider visibile e contato dalla sentinella"
  fi
fi

# ── test-skip-visibile ──────────────────────────────────────────────────────
# Stessa regola dei test di parita' provider, applicata agli integration test di
# mcp-core: erano 42 `eprintln!("skip ...")` + `return` in 9 file (sette
# stampavano il solo "skip", senza dire di cosa), e ognuno di essi si presentava
# nel gate come un contratto verificato. In CI `DATABASE_URL` c'e', ma
# NEXUS_TEST_JWT no e nessun mcp-core e' in ascolto: tutti i test al wire erano
# verdi senza aver interrogato niente.
#
# Ora la precondizione passa dal crate `nexus-test-preconditions` (`salta`), che
# stampa un marker NEXUS_TEST_SKIP e sotto REQUIRE_INTEGRATION_TESTS=1 fallisce;
# la sentinella `mcp-core/tests/precondizioni_integrazione.rs` dichiara il quadro
# a ogni esecuzione. Il punto unico vive in un crate perche' mcp-core e' bin-only
# e nexus-auth / nexus-project-pools sono sue DIPENDENZE: nessuno dei tre poteva
# ospitarlo per gli altri, e tre copie sarebbero tre verita' (regola L).
#
# Questo check copre tutti i crate con test opportunistici e impedisce sia di
# rientrare nello skip muto, sia di ri-fondare un secondo punto unico locale.
#
# Il pattern cerca il LETTERALE `"skip`, non `eprintln!("skip`: uno skip
# formattato su due righe
#
#     eprintln!(
#         "skip: binary non trovato",
#     );
#
# sfugge a un pattern ancorato alla macro, ed era esattamente la forma di quello
# in `agent_tools_safety` — che il guard ha percio' dichiarato pulito per due
# esecuzioni di fila. Cercare la stringa copre entrambe le forme e qualunque altra
# macro la stampi. Esclude le righe di commento (`^[^/]*`): le intestazioni citano
# `eprintln!("skip: ...")` per spiegare cosa ha sostituito.
crate_opportunistici=(
  crates/mcp-core/tests
  crates/nexus-auth/tests
  crates/nexus-project-pools/tests
)
skip_muti=""
while IFS= read -r hit; do
  [[ -n "$hit" ]] && skip_muti+="$hit"$'
'
done < <(grep -rnE '^[^/]*"skip' "${crate_opportunistici[@]}" 2>/dev/null || true)

# Un secondo `fn salta` / `fn db_o_salta` / `fn jwt_o_salta` fuori dal crate
# autoritativo sarebbe una copia della stessa decisione, libera di divergere: e'
# il modo in cui questo difetto e' nato (ogni file il suo skip, ogni crate il suo
# helper di connessione).
punto_unico="crates/nexus-test-preconditions/src/lib.rs"
doppioni=""
while IFS= read -r hit; do
  [[ -n "$hit" ]] && doppioni+="$hit"$'
'
done < <(grep -rnE '^[^/]*fn (salta|db_o_salta|jwt_o_salta)\('   --include='*.rs' crates/ 2>/dev/null | grep -v "^$punto_unico:" || true)

if [[ -n "$skip_muti" || -n "$doppioni" ]]; then
  if [[ -n "$skip_muti" ]]; then
    echo "!! test-skip-visibile: skip stampato a mano negli integration test:" >&2
    printf '%s' "$skip_muti" | sed 's/^/     /' >&2
    echo "   Usare nexus_test_preconditions::salta(Motivo::...) — stampa il marker" >&2
    echo "   NEXUS_TEST_SKIP e, con REQUIRE_INTEGRATION_TESTS=1, FALLISCE." >&2
  fi
  if [[ -n "$doppioni" ]]; then
    echo "!! test-skip-visibile: punto unico dello skip ri-definito fuori da $punto_unico:" >&2
    printf '%s' "$doppioni" | sed 's/^/     /' >&2
    echo "   Aggiungerlo al crate condiviso e usarlo come dev-dependency." >&2
  fi
  fail=1
else
  sentinella="crates/mcp-core/tests/precondizioni_integrazione.rs"
  mancanti=""
  [[ -f "$punto_unico" ]] && grep -q 'pub fn salta' "$punto_unico" ||
    mancanti+="manca il punto unico salta() in $punto_unico"$'
'
  [[ -f "$sentinella" ]] && grep -q 'fn precondizioni_dichiarate' "$sentinella" ||
    mancanti+="manca la sentinella precondizioni_dichiarate in $sentinella"$'
'
  if [[ -n "$mancanti" ]]; then
    echo "!! test-skip-visibile: il punto unico dello skip non e' al suo posto:" >&2
    printf '%s' "$mancanti" | sed 's/^/     /' >&2
    fail=1
  else
    echo "OK test-skip-visibile: nessuno skip muto, un solo punto unico delle precondizioni"
  fi
fi

# --- Identita' del binario in esecuzione (2026-07-27) ---
# `/health` e' lo strumento con cui si verifica quale artefatto stia girando
# dopo un deploy. Il valore nasceva in crates/mcp-core/build.rs da
# SystemTime::now(): cargo riesegue uno script di build solo quando cambiano le
# dipendenze che lo script DICHIARA, quindi il timestamp restava congelato
# all'ultima modifica di build.rs mentre il binario veniva ricompilato. Misurato
# il 27/07/2026: /health dichiarava il 20/07 su un binario linkato quel giorno.
# Il punto unico legge l'mtime del proprio eseguibile a runtime (regola O).
assert_single "running_binary" 'pub fn running_binary' 'crates/nexus-types/src/build_info.rs' crates

ts_incisi="$(grep -rlE 'SystemTime::now|Instant::now|rustc-env=BUILD' --include='build.rs' \
  --exclude-dir=target crates/ 2>/dev/null || true)"
if [[ -n "$ts_incisi" ]]; then
  echo "!! build-stamp: uno script di build incide nel binario un valore che scorre nel tempo:" >&2
  printf '%s' "$ts_incisi" | sed 's/^/     /' >&2
  echo "   Cargo non riesegue lo script a ogni link: il valore resterebbe indietro" >&2
  echo "   rispetto al binario. Usare nexus_types::build_info::running_binary()." >&2
  fail=1
else
  echo "OK build-stamp: nessun timestamp inciso da uno script di build"
fi

# ── cache-di-configurazione-chiavata ────────────────────────────────────────
# Una cache DI PROCESSO che memorizza configurazione letta da un database deve
# essere chiavata per DATABASE (`nexus_auth::pool_identity`). Con una chiave
# costante il primo lettore decide per tutti fino alla scadenza del TTL, e
# mcp-core interroga piu' database (il meta e un `<slug>_nexus` per progetto).
#
# Non e' teoria: il 2026-07-27 `qualification_gate` teneva il gate in una statica
# senza chiave, un `#[sqlx::test(migrator = "META_MIGRATOR")]` lo accendeva (mig
# 0595) e per 60s ogni altro test del processo si vedeva il catalog svuotato —
# sei test di `internal_routing` rossi o verdi a seconda di chi partiva primo
# (regole F e O). Lo stesso valeva per il flag isolamento, che cachava con chiave
# `()` un valore GIA' cachato da `nexus_auth`.
#
# Il divieto colpisce solo le cache `static`: una `TtlCache<(), _>` come CAMPO di
# una struct e' legittima (l'istanza e' gia' legata alla sua fonte).
cache_hits="$(grep -rnE --include='*.rs' --exclude-dir=target \
  'static [A-Z_]+ *:.*TtlCache<\(\)' crates 2>/dev/null || true)"
if [[ -n "$cache_hits" ]]; then
  echo "!! cache-di-configurazione-chiavata: cache di processo con chiave costante:" >&2
  printf '%s\n' "$cache_hits" | sed 's/^/     /' >&2
  echo "   Chiavala per database con nexus_auth::pool_identity(db), o togli la" >&2
  echo "   cache se sotto c'e' gia' nexus_auth::get_setting (che cacha da solo)." >&2
  fail=1
else
  echo "OK cache-di-configurazione-chiavata: nessuna cache statica a chiave costante"
fi

# ── lettura-manifest-servizio (2026-07-28) ──────────────────────────────────
# Il manifest WinSW si legge da UN solo posto: deploy/lib/nexus-manifest.ps1.
# `<arguments>` ed `<env>` sono OPZIONALI e il generatore li omette quando vuoti
# (test `un_servizio_senza_argomenti_non_emette_il_tag`). Chi rilegge il manifest
# con l'adapter a proprieta' (`$x.service.arguments`) tratta l'opzionale da
# obbligatorio e si rompe SOLO sotto StrictMode, cioe' solo per certi percorsi di
# invocazione: il 28/07/2026 dev-start.ps1 funzionava lanciato a mano e falliva
# dentro deploy-local.ps1, lasciando 7 servizi su 8 giu' dopo un deploy riuscito.
man_hits="$(grep -rlE '\[xml\]' --include='*.ps1' deploy/ 2>/dev/null \
  | grep -v 'deploy/lib/nexus-manifest.ps1' || true)"
if [[ -n "$man_hits" ]]; then
  echo "!! lettura-manifest-servizio: il manifest si rilegge fuori dal punto unico:" >&2
  printf '%s\n' "$man_hits" | sed 's/^/     /' >&2
  echo "   Delegare a Read-NexusServiceManifest (deploy/lib/nexus-manifest.ps1)," >&2
  echo "   che legge i tag opzionali con XPath e non dipende da StrictMode." >&2
  fail=1
else
  echo "OK lettura-manifest-servizio: un solo lettore dei manifest WinSW"
fi

# ── strictmode-non-si-propaga (2026-07-28) ──────────────────────────────────
# `Set-StrictMode` ha scope DINAMICO: impostato a livello di file in una libreria
# dot-sourced, vale per il chiamante e per tutto cio' che il chiamante invoca
# dopo. deploy/lib/nexus-publish.ps1 lo faceva, e deploy-local.ps1 lo propagava
# fino a dev-start.ps1 — che cosi' cambiava comportamento a seconda di CHI lo
# aveva lanciato. Una libreria imposta la modalita' dentro le proprie funzioni,
# dove ha lo scope di cio' che protegge e non detta legge a nessun altro.
sm_hits="$(grep -rn '^Set-StrictMode' --include='*.ps1' deploy/lib/ 2>/dev/null || true)"
if [[ -n "$sm_hits" ]]; then
  echo "!! strictmode-non-si-propaga: una libreria dot-sourced impone la modalita' a valle:" >&2
  printf '%s\n' "$sm_hits" | sed 's/^/     /' >&2
  echo "   Spostare Set-StrictMode DENTRO le funzioni che lo richiedono." >&2
  fail=1
else
  echo "OK strictmode-non-si-propaga: nessuna libreria impone StrictMode al chiamante"
fi

# ── catena-write-scope (2026-07-28) ─────────────────────────────────────────
# La MISURA delle scritture fuori scope (mig 0646) vale solo se lo scope
# dichiarato dal pianificatore arriva davvero fino al contesto dei tool. La catena
# e' fatta di passaggi di campo omonimi — ParsedTask -> SubagentExecInputs ->
# NativeRunInput -> ToolRunnerExecutorAdapter -> ToolContextCore — che il
# compilatore verifica nei TIPI ma non nel VALORE: sostituirne uno con
# `Vec::new()` compila, non rompe alcun test, e rende la misura cieca in silenzio.
# La colonna si riempirebbe di `no_scope_declared` e il numero direbbe "il
# pianificatore e' preciso" quando invece non e' stato misurato niente. E' la
# stessa famiglia della whitelist mai raggiunta dalla produzione e della chiave di
# qualificazione mai scritta con sette test verdi.
#
# Il guard e' testuale e lo dichiara: NON e' un test di esecuzione, e' un
# ancoraggio sui tre anelli che nessun test attraversa.
ws_missing=""
ws_check() { # file regex etichetta
  grep -qE "$2" "$1" 2>/dev/null || ws_missing+="  $3 ($1)"$'\n'
}
ws_check crates/mcp-core/src/agent_tools/subagent_native.rs \
  'write_scope: write_scope\.to_vec\(\)' \
  'ParsedTask -> SubagentExecInputs'
ws_check crates/mcp-core/src/native_engine.rs \
  'input\.write_scope\.clone\(\)' \
  'NativeRunInput -> ToolRunnerExecutorAdapter'
ws_check crates/mcp-core/src/agent_graph_adapter/tool_executor.rs \
  '&self\.write_scope' \
  'adapter -> build_ctx_with_root'
ws_check crates/mcp-core/src/tool_runner_server.rs \
  'write_scope: write_scope\.to_vec\(\)' \
  'build_ctx_with_root -> ToolContextCore'
ws_check crates/nexus-agent-graph/src/nodes/todo_runner.rs \
  'todo\.get\("write_scope"\)' \
  'passo di piano -> dispatch_one (il ramo dominante)'
ws_check crates/nexus-agent-graph/src/nodes/todo_runner.rs \
  '"write_scope": write_scope' \
  'subagent_task_json (forma comune ai due rami)'
if [[ -n "$ws_missing" ]]; then
  echo "!! catena-write-scope: un anello della propagazione non passa piu' il valore:" >&2
  printf '%s' "$ws_missing" >&2
  echo "   Senza quell'anello la misura scope (mig 0646) registra 'no_scope_declared'" >&2
  echo "   su tutto e sembra dire che il pianificatore non sbaglia mai." >&2
  fail=1
else
  echo "OK catena-write-scope: lo scope dichiarato raggiunge il contesto dei tool"
fi

# ── memorie-nel-prompt (2026-07-28) ─────────────────────────────────────────
# Le memorie di progetto hanno DUE consumatori (turno singolo e run agentico) e
# devono avere UN caricatore. Il difetto che ha creato il punto unico e' proprio
# la seconda strada che non c'era: il consumo viveva dentro `Orchestrator::run`,
# raggiungibile solo da `run_turn`, e in Conferma/Automatico l'handler dispatcha
# a `spawn_agent_run` e ritorna prima — il pannello "Memoria del progetto" non
# aveva alcun effetto sui run agentici.
#
# Due ancoraggi. Primo: la FORMA del blocco vive solo nel punto unico, cosi' un
# call site non se ne scrive una versione propria.
assert_single "blocco-memorie-prompt" 'Correzioni note \(da rispettare se pertinenti\)' \
  'crates/mcp-core/src/prompt_memories.rs' crates
# Secondo: il caricamento sta DENTRO il compositore del system prompt agentico.
# Se tornasse nel chiamante, comporre quel prompt senza richiamare le memorie
# ridiventerebbe possibile — e la regressione sarebbe di nuovo invisibile, perche'
# un prompt senza memorie e' un prompt perfettamente valido.
#
# La verifica guarda DENTRO il corpo di quella funzione, non il file: un
# `grep` sull'intero sorgente direbbe "OK" anche con la chiamata spostata nei
# test o in un altro punto, cioe' proprio nel caso che deve intercettare.
mem_file="crates/mcp-core/src/chat_messages/agent_run.rs"
mem_innesto="$(awk '
  /^pub\(crate\) async fn compose_agent_system_text/ { dentro = 1; next }
  dentro && /ProjectMemories::load/ { print "trovato"; exit }
  dentro && /^}/ { exit }
' "$mem_file" 2>/dev/null)"
if [[ -z "$mem_innesto" ]]; then
  echo "!! memorie-nel-prompt: il system prompt agentico non passa piu' dal richiamo delle memorie." >&2
  echo "   Il caricamento deve restare dentro compose_agent_system_text ($mem_file)," >&2
  echo "   non nel chiamante: li' comporre il prompt senza memorie tornerebbe possibile." >&2
  fail=1
else
  echo "OK memorie-nel-prompt: il percorso agentico richiama le memorie di progetto"
fi
# ── estremi-bucket-porte ────────────────────────────────────────────────────
# Gli estremi del bucket di un progetto si chiedono a `project_bucket_range`, non
# si ricalcolano. Il motivo non e' l'eleganza: la stessa somma girava in sei punti
# con DUE convenzioni diverse -- `start + SIZE` (fine ESCLUSA) e `start + SIZE - 1`
# (INCLUSA) -- e chi confrontava doveva indovinare quale delle due aveva davanti.
# Una porta di confine cadeva dentro o fuori a seconda del file che la guardava.
#
# La dispersione e' anche cio' che ha tenuto nascosto il difetto vero: il predicato
# che autorizzava una porta a diventare allocazione del progetto non riceveva il
# `project_id`, quindi la domanda "questa porta e' TUA?" non era proprio ponibile,
# e nessuno dei sei call site poteva porgliela.
#
# Cerca l'aritmetica sul SIZE fuori dal punto unico. Il modulo (`% SIZE`) e il
# confronto (`< SIZE`) restano leciti: non producono un estremo.
estremi_hits="$(
  grep -rnE '(\+[[:space:]]*PROJECT_PORT_BUCKET_SIZE|saturating_add\(PROJECT_PORT_BUCKET_SIZE)' \
    --include='*.rs' crates 2>/dev/null \
  | grep -v '^crates/nexus-tool-kit/src/ports.rs:' || true
)"
if [[ -n "$estremi_hits" ]]; then
  echo "!! estremi-bucket-porte: estremi del bucket ricalcolati fuori dal punto unico:" >&2
  printf '%s\n' "$estremi_hits" | sed 's/^/     /' >&2
  echo "   Usa project_bucket_range(&project_id) -> (start, end) con end INCLUSO," >&2
  echo "   o port_in_project_bucket(&project_id, port) se la domanda e' l'appartenenza." >&2
  echo "   Vedi crates/nexus-tool-kit/src/ports.rs (regola L / ADR 0026)." >&2
  fail=1
else
  echo "OK estremi-bucket-porte: gli estremi del bucket vengono dal punto unico"
fi
# ── criterio di progresso di una correzione ──────────────────────────────────
# Due presidi per lo stesso punto unico (regola L):
#
# 1. Il CONFRONTO fra gli hash del contenuto vive solo in
#    `WriteFact::cambia_il_contenuto`. La forma piu' probabile della ricaduta e'
#    comoda e silenziosa: aggiungere `AND before_sha256 IS DISTINCT FROM
#    after_sha256` alla query dell'adapter. Il gate resterebbe verde e
#    perderebbe la distinzione fra "non ha scritto" e "ha riscritto file
#    identici", che sono due comportamenti diversi dell'agente e vanno detti
#    come tali nel rimando.
# 2. La porta dev'essere INNESTATA nel nodo. Senza `.with_mutation_progress(...)`
#    il ReviewGate torna, in silenzio e con tutti i test verdi, a riconvocare i
#    revisori su codice non modificato: il difetto del 28/07/2026 (tre panel
#    sullo stesso codice, 1.243.417 token) e' esattamente uno scollegamento del
#    genere.
progresso_hits="$(grep -rEn 'before_sha256' \
  --include='*.rs' --include='*.sql' --include='*.ts' --include='*.tsx' \
  crates/ db/ apps/ 2>/dev/null \
  | grep -E 'after_sha256' \
  | grep -E '(!=|==|<>|IS DISTINCT FROM|is_distinct)' \
  | grep -vE '^[^:]+:[0-9]+:\s*(//|#|--|\*)' \
  | grep -v '^crates/nexus-agent-graph/src/decisions/correction_progress.rs:' \
  || true)"
if ! grep -q 'with_mutation_progress(' crates/mcp-core/src/native_engine.rs 2>/dev/null; then
  progresso_hits="${progresso_hits}
crates/mcp-core/src/native_engine.rs: ReviewGateNode costruito senza .with_mutation_progress()"
fi
if [[ -n "${progresso_hits// /}" ]]; then
  echo "!! correction-progress: il criterio di progresso e' deciso fuori dal punto unico:" >&2
  printf '%s\n' "$progresso_hits" >&2
  echo "   Il confronto degli hash vive in WriteFact::cambia_il_contenuto" >&2
  echo "   (decisions/correction_progress.rs); la porta MutationProgressPort porta" >&2
  echo "   i fatti e NON li filtra. Un filtro in SQL rende una riscrittura" >&2
  echo "   identica indistinguibile da 'non ha scritto niente'." >&2
  fail=1
else
  echo "OK correction-progress: il criterio vive nel punto unico e la porta e' innestata"
fi

# ── conformita' ai requisiti del Consiglio ───────────────────────────────────
# Due presidi per lo stesso punto unico (regola L), e il secondo e' il piu'
# importante: e' la forma del difetto che il modulo chiude.
#
# 1. I REQUISITI si leggono dalla sintesi con `requirements_from_synthesis`.
#    Chi si prendesse il campo per conto proprio prima o poi ci aggiungerebbe
#    `recommendations` "gia' che c'e'", e una raccomandazione non applicata
#    diventerebbe uno scostamento: il rilievo si trasforma in rumore da ignorare,
#    che e' il modo in cui un rilievo smette di essere letto.
# 2. Il riscontro dev'essere INNESTATO in TUTTE le chiusure di un run nativo.
#    `Ok(map_outcome(...))` diretto in un ingresso significa che quel percorso
#    chiude senza guardare i requisiti — cioe' il comportamento di prima del fix,
#    su una strada sola e in silenzio. E' esattamente cosi' che il segnale del
#    Consiglio e' rimasto non riscontrato: nessuno lo collegava, tutto verde.
#    Un `assert` sul campo NON e' una lettura decisionale: verifica la forma del
#    payload prodotto, che e' proprio cio' che si vuole restare libero di
#    controllare. Escluso di proposito, non per far tacere il check.
conf_hits="$(grep -rEn '\["requirements"\]|get\("requirements"\)' \
  --include='*.rs' crates/ 2>/dev/null \
  | grep -vE '^[^:]+:[0-9]+:\s*(//|#|--|\*)' \
  | grep -v 'assert' \
  | grep -v '^crates/nexus-agent-graph/src/decisions/requirement_conformance.rs:' \
  | grep -v '^crates/nexus-agent-graph/src/decisions/advisory_panel.rs:' \
  || true)"
if grep -qE '^\s*Ok\(map_outcome\(' crates/mcp-core/src/native_engine.rs 2>/dev/null; then
  conf_hits="${conf_hits}
crates/mcp-core/src/native_engine.rs: un ingresso chiude con map_outcome() senza riscontro"
fi
if ! grep -q 'ConformanceReport::nota' crates/mcp-core/src/chat_messages/agent_run.rs 2>/dev/null; then
  conf_hits="${conf_hits}
crates/mcp-core/src/chat_messages/agent_run.rs: il resoconto non consuma piu' la nota di conformita'"
fi
if [[ -n "${conf_hits// /}" ]]; then
  echo "!! requisiti-consiglio: la conformita' e' decisa o consumata fuori dal punto unico:" >&2
  printf '%s\n' "$conf_hits" | sed 's/^/     /' >&2
  echo "   I requisiti si leggono con requirements_from_synthesis (SOLO quelli:" >&2
  echo "   recommendations e' l'altra lista); il verdetto e' di judge() sul" >&2
  echo "   CONTENUTO del file; le chiusure di un run passano da" >&2
  echo "   map_outcome_con_riscontro. Vedi decisions/requirement_conformance.rs." >&2
  fail=1
else
  echo "OK requisiti-consiglio: il riscontro e' innestato e la nota arriva al resoconto"
fi

# ── forma-punto-memoria + contatore-memorie-per-punto (2026-07-28) ──────────
# I punti della collection memorie/correzioni hanno TRE produttori (compattazione
# di sessione, correzioni admin, correzioni di chat) e devono avere UNA forma.
# Quando la forma era ricopiata da ciascuno, i tre payload divergevano in
# silenzio: un punto malformato non da' errore, semplicemente non viene mai
# richiamato (manco `active`, che il filtro esige) o non viene mai contato.
#
# Primo ancoraggio: chi scrive un punto passa dal costruttore unico.
#
# Il conteggio dei produttori esaminati viene STAMPATO, e zero produttori e' un
# FALLIMENTO: un check che tace quando passa e' indistinguibile da un check che
# non ha trovato il suo oggetto (path rinominato, grep cambiato), e resterebbe
# verde proprio nel caso in cui ha smesso di guardare (regola O).
mem_produttori=0
for f in $(grep -rl --include='*.rs' 'upsert_prompt_correction_point' crates 2>/dev/null || true); do
  # vector_memory.rs definisce sia l'upsert sia il costruttore.
  [[ "$f" == "crates/mcp-core/src/vector_memory.rs" ]] && continue
  mem_produttori=$((mem_produttori + 1))
  if ! grep -q 'prompt_correction_payload' "$f"; then
    echo "!! forma-punto-memoria: $f scrive un punto memoria con un payload proprio." >&2
    echo "   Usare vector_memory::prompt_correction_payload: i campi che la ricerca" >&2
    echo "   esige (project_id, text, active) sono parametri, non campi da ricordare." >&2
    fail=1
  fi
done
if [[ "$mem_produttori" -eq 0 ]]; then
  echo "!! forma-punto-memoria: nessun produttore di punti memoria trovato." >&2
  echo "   Il check non ha raggiunto il suo oggetto: verificare il nome" >&2
  echo "   upsert_prompt_correction_point e il percorso crates/." >&2
  fail=1
else
  echo "OK forma-punto-memoria: $mem_produttori produttori passano dal costruttore unico"
fi

# Secondo ancoraggio: il recupero si contabilizza per PUNTO. E' la regressione
# appena chiusa: il bump si agganciava a `payload["correction_id"]`, che i tre
# produttori scrivevano in tre modi (mai, l'id del punto, l'id della riga). Per
# due famiglie su tre `retrieved_count` restava a zero per sempre, e il pruner
# notturno di chat_learning (ramo `unused_ttl`) disattivava dopo 90 giorni
# memorie richiamate ogni giorno, cancellandone il punto vettoriale.
#
# La verifica guarda DENTRO il corpo del bump: un grep sul file resterebbe verde
# con la query spostata o riscritta altrove.
mem_bump="$(awk '
  /^async fn bump_retrieval/ { dentro = 1 }
  dentro && /qdrant_point_id = ANY/ { print "trovato"; exit }
  dentro && /^}/ { exit }
' crates/mcp-core/src/prompt_memories.rs 2>/dev/null)"
if [[ -z "$mem_bump" ]]; then
  echo "!! contatore-memorie-per-punto: il recupero non si contabilizza piu' per punto." >&2
  echo "   bump_retrieval deve agganciarsi a qdrant_point_id (UNIQUE), non a un campo" >&2
  echo "   del payload: quello dipende da quale dei tre produttori ha scritto il punto," >&2
  echo "   e un contatore fermo a zero fa potare la memoria dopo 90 giorni." >&2
  fail=1
else
  echo "OK contatore-memorie-per-punto: il recupero si contabilizza per qdrant_point_id"
fi

# ── appartenenza-processo + iniezione-porta-libera (2026-07-29) ────────────
# «Di CHI e' questo processo?» ha UN solo punto in cui si risponde. Il criterio
# precedente rispondeva a «e' del progetto?» (bucket + is_tracked_pid), che e'
# una domanda piu' LARGA: allocando per `frontend` l'adozione prendeva il primo
# server del bucket — il BACKEND — e ne legava la porta al frontend.
assert_single "appartenenza-processo" 'fn classify_ownership' \
  'crates/mcp-core/src/project_workspace/service_ownership.rs' crates

# Il consumatore, non solo la definizione: i TRE rami di find_or_allocate che
# possono legare una porta gia' in uso a un servizio devono passare dal verdetto.
# Un grep sul file resterebbe verde se la funzione esistesse ma nessuno la
# chiamasse (regola O: il codice morto ha test verdi). Il conteggio e' stampato:
# zero consumatori trovati e' un FALLIMENTO, non un silenzio.
# Conta le CHIAMATE, non le menzioni: senza la parentesi e senza escludere i
# commenti, il guard resterebbe verde con la chiamata rimossa e il commento che
# la descrive ancora al suo posto (misurato: contava 3 rami su 2 reali).
rami_verdetto="$(awk '
  /^pub async fn find_or_allocate/ { dentro = 1 }
  dentro && !/^[[:space:]]*\/\// && /resolve_stale_adoption\(|owned_listener\(/ { n++ }
  dentro && /^}/ { exit }
  END { print n + 0 }
' crates/mcp-core/src/project_workspace/allocate_port.rs 2>/dev/null)"
# Atteso: l'adozione dell'allocazione stantia + il riuso quando non esiste riga
# per la label. Il terzo ramo (occupante della porta gia' allocata) passa da
# active_port_action, verificato dai suoi test.
if [[ "$rami_verdetto" -lt 2 ]]; then
  echo "!! appartenenza-processo: find_or_allocate consulta il verdetto in $rami_verdetto rami su 2." >&2
  echo "   Ogni ramo che lega una porta gia' in uso deve passare da service_ownership:" >&2
  echo "   'primo processo del bucket che somigli a un server' e 'stessa classe di" >&2
  echo "   label' sono le domande larghe che legano a un servizio la porta di un altro." >&2
  fail=1
else
  echo "OK appartenenza-processo: $rami_verdetto rami di find_or_allocate passano dal verdetto"
fi

# Il ramo "nessuna riga per questa label" non deve tornare a scegliere il
# candidato col matching per CLASSE: quello accetta una qualsiasi risorsa
# "di frontend", e per un processo non registrato la label la inventa
# derive_orphan_label dal nome del programma (regola M).
if awk '
  /^pub async fn find_or_allocate/ { dentro = 1 }
  dentro && /resource_resolver::resolve_for_label\(/ { trovato = 1 }
  dentro && /^}/ { exit }
  END { exit !trovato }
' crates/mcp-core/src/project_workspace/allocate_port.rs 2>/dev/null; then
  echo "!! appartenenza-processo: find_or_allocate sceglie di nuovo con resolve_for_label." >&2
  echo "   Quel matching e' per CLASSE di servizio (frontend include web/ui/client/" >&2
  echo "   vue/next/react) e su un processo non registrato lavora su una label" >&2
  echo "   DEDOTTA DAL NOME DEL PROGRAMMA. Usare owned_listener." >&2
  fail=1
else
  echo "OK appartenenza-processo: il riuso non passa piu' dal matching per classe"
fi

# La FONTE delle risorse non deve tornare a indovinare uno scopo dal nome del
# programma. Un processo senza riga di allocazione compare come `service-<porta>`;
# se ricevesse "frontend" perche' il suo comando contiene "vite", quella label
# entrerebbe nella lista mescolata alle label vere lette dal DB, e nessuno dei
# consumatori potrebbe distinguerle: da li' si eredita una porta, si nega a un
# agente il proprio avvio, e si UCCIDE l'albero di un processo
# (`free_listening_scope_port` -> `try_free_port`).
if awk '
  /fn orphan_placeholder_label/ { dentro = 1 }
  dentro && /vite|next|nuxt|astro|react|svelte|frontend|backend/ { trovato = 1 }
  dentro && /^}/ { exit }
  END { exit !trovato }
' crates/mcp-core/src/project_workspace/resource_resolver.rs 2>/dev/null; then
  echo "!! identita-non-indovinata: la fonte delle risorse deduce di nuovo uno scopo dal programma." >&2
  echo "   orphan_placeholder_label deve dare un identificatore POSIZIONALE" >&2
  echo "   (service-<porta>): il nome di un programma non e' un'identita' (regola M)," >&2
  echo "   e una label inventata e' indistinguibile da una letta dal DB." >&2
  fail=1
else
  echo "OK identita-non-indovinata: un processo non registrato non riceve uno scopo"
fi

# I percorsi di AVVIO di un servizio delegano l'alloca+inietta al punto unico:
# ognuno ricopiava la sequenza, e due su tre in caso di errore proseguivano
# "senza PORT iniettato" — cioe' lasciando scegliere la porta al framework,
# fuori dal bucket del progetto.
avvii=0
avvii_bad=0
for f in crates/mcp-core/src/project_workspace/services.rs \
         crates/mcp-core/src/project_workspace/service_manager.rs \
         crates/mcp-core/src/agent_tools/service.rs; do
  [[ -f "$f" ]] || continue
  avvii=$((avvii + 1))
  if grep -q 'find_or_allocate_port' "$f"; then
    echo "!! iniezione-porta-libera: $f alloca la porta di avvio da se'." >&2
    echo "   Delegare ad allocate_port::web_service_port_env: iniettare PORT e' una" >&2
    echo "   promessa, e va mantenuta solo su una porta davvero bindabile." >&2
    avvii_bad=$((avvii_bad + 1))
    fail=1
  fi
done
if [[ "$avvii" -eq 0 ]]; then
  echo "!! iniezione-porta-libera: nessun percorso di avvio esaminato." >&2
  echo "   Il check non ha raggiunto il suo oggetto: verificare i percorsi." >&2
  fail=1
elif [[ "$avvii_bad" -eq 0 ]]; then
  echo "OK iniezione-porta-libera: $avvii percorsi di avvio delegano al punto unico"
fi

# «L'app SENZA server mostra il proprio contenuto?» (2026-08-08, mig 0685)
#
# Il criterio e il suo discriminante vivono in un punto solo. Il secondo conta
# quanto il primo: «questo progetto e' un'app statica?» ha due fatti (servizio e
# pagina) che si raccolgono in posti diversi, ed e' esattamente la forma in cui
# una decisione si sparpaglia — due call site che rispondono ciascuno a modo suo
# e divergono in silenzio.
assert_single "resa-statica" 'fn classifica_resa' \
  'crates/nexus-agent-graph/src/decisions/static_render.rs' crates
assert_single "natura-app-dai-fatti" 'fn classifica_natura' \
  'crates/nexus-agent-graph/src/decisions/static_render.rs' crates

# Il guard che conta davvero: la pagina da guardare non si cerca due volte.
#
# `detect_static_entry` (mcp-core/src/static_preview.rs) e' gia' il punto unico
# di «qual e' la pagina di questo progetto», e la usa il pannello Servizi per il
# pulsante "Apri nel browser". Il gate DEVE guardare quella stessa pagina: una
# seconda ricerca — foss'anche un solo `index.html` scritto a mano nel motore —
# darebbe al criterio un bersaglio diverso da quello che l'utente apre, e il
# verde varrebbe per un file che nessuno guarda. Si esaminano le sole righe di
# CODICE (nei test il nome e' la fixture, e li' e' legittimo).
per_file_entry=0
for f in crates/mcp-core/src/native_engine.rs \
         crates/mcp-core/src/agent_graph_adapter/criteria_runner.rs; do
  [[ -f "$f" ]] || continue
  per_file_entry=$((per_file_entry + 1))
  if awk '
    /^#\[cfg\(test\)\]/ { exit }
    /^[[:space:]]*\/\// { next }
    /"[^"]*index\.html"|"[^"]*index\.htm"|"[^"]*home\.html"|"[^"]*main\.html"/ {
      trovato = 1; print "   " FILENAME " riga " NR ": " $0 > "/dev/stderr"
    }
    END { exit !trovato }
  ' "$f" 2>&1; then
    echo "!! entry-dal-punto-unico: la pagina del progetto e' cercata a mano." >&2
    echo "   Delegare a static_preview::detect_static_entry: il gate deve" >&2
    echo "   guardare la STESSA pagina che il pannello Servizi apre." >&2
    fail=1
  fi
done
if [[ "$per_file_entry" -eq 0 ]]; then
  echo "!! entry-dal-punto-unico: nessun file esaminato (percorsi cambiati?)." >&2
  fail=1
else
  echo "OK entry-dal-punto-unico: $per_file_entry file delegano il rilevamento della pagina"
fi

# «Le RISORSE che la pagina referenzia sono arrivate?» (2026-08-09, mig 0692)
#
# Il criterio vive in un punto solo, e con esso il predicato «questa richiesta
# osservata e' fallita?»: quel predicato lo usano DUE criteri sugli stessi fatti
# (il dialogo sulle chiamate dati, questo sulle risorse), e due encoding della
# stessa regola divergerebbero al primo status che uno dei due decidesse di
# trattare diversamente — senza che nulla fallisca.
assert_single "risorse-di-pagina" 'fn classifica_risorse' \
  'crates/nexus-agent-graph/src/decisions/risorse_pagina.rs' crates
assert_single "richiesta-fallita" 'fn richiesta_fallita' \
  'crates/nexus-agent-graph/src/decisions/browser_dialogue.rs' crates

# Il guard che conta davvero: il TIPO di una risorsa lo dichiara il browser.
#
# `resourceType()` e' un segnale strutturato (regola M). Dedurlo dall'estensione
# dell'URL sembra equivalente e non lo e': `/api/thumb?id=3` e' un'immagine e
# `/logo.png.txt` non lo e', quindi l'euristica sbaglia in ENTRAMBE le
# direzioni — manca un tipo compromesso, o ne inventa uno. E sbaglierebbe in
# silenzio, perche' il verdetto resterebbe della forma giusta. Si esaminano le
# sole righe di CODICE: nei test gli URL con estensione sono la fixture.
tipi_esaminati=0
tipi_bad=0
for f in crates/nexus-agent-graph/src/decisions/risorse_pagina.rs \
         crates/mcp-core/src/agent_tools/browser_probe.rs; do
  [[ -f "$f" ]] || continue
  tipi_esaminati=$((tipi_esaminati + 1))
  if awk '
    /^#\[cfg\(test\)\]/ { exit }
    /^[[:space:]]*\/\// { next }
    /ends_with\("\.|extension\(\)|\.png"|\.jpg"|\.jpeg"|\.svg"|\.webp"|\.css"|\.js"/ {
      trovato = 1; print "   " FILENAME " riga " NR ": " $0 > "/dev/stderr"
    }
    END { exit !trovato }
  ' "$f" 2>&1; then
    echo "!! tipo-risorsa-dichiarato: il tipo e' dedotto dall'URL." >&2
    echo "   Usare il campo che il browser dichiara (resourceType()): un URL" >&2
    echo "   senza estensione puo' essere un'immagine, e viceversa." >&2
    tipi_bad=$((tipi_bad + 1))
    fail=1
  fi
done
if [[ "$tipi_esaminati" -eq 0 ]]; then
  echo "!! tipo-risorsa-dichiarato: nessun file esaminato (percorsi cambiati?)." >&2
  fail=1
elif [[ "$tipi_bad" -eq 0 ]]; then
  echo "OK tipo-risorsa-dichiarato: $tipi_esaminati file leggono il tipo dal browser"
fi

# «Lo stile che il codice dichiara e' applicato?» (2026-07-29, mig 0655)
#
# Il criterio vive in un punto solo. Un secondo giudice dello stile — anche
# scritto meglio — riporterebbe la domanda al problema che il modulo chiude: due
# risposte diverse alla stessa domanda, e nessuna delle due autorevole.
assert_single "stile-applicato" 'fn classify_styling' \
  'crates/nexus-agent-tools/src/ui_styling.rs' crates

# Il guard che conta davvero: nel CRITERIO non entrano nomi di framework.
#
# La domanda posta al codice e' «le classi hanno una fonte?», non «c'e'
# Tailwind?»: Tailwind e' un'istanza, e il giorno in cui il criterio la nomina
# ricomincia l'inseguimento delle varianti (regola H) — il difetto del 29/07 si
# ripresenterebbe identico col framework successivo. I nomi stanno in
# `settings.agent.ui_styling.*`, dove aggiungerne uno e' una riga.
#
# Si guardano le sole righe di CODICE fino a `#[cfg(test)]`: nei commenti il
# nome dell'istanza serve a spiegare il difetto reale, e nei test e' la fixture.
if [[ -f crates/nexus-agent-tools/src/ui_styling.rs ]]; then
  if awk '
    /^#\[cfg\(test\)\]/ { exit }
    /^[[:space:]]*\/\// { next }
    /[Tt]ailwind|[Uu]noCSS|unocss|windicss|bootstrap|@mui\/|chakra-ui|@emotion\/|styled-components/ {
      trovato = 1; print "   riga " NR ": " $0 > "/dev/stderr"
    }
    END { exit !trovato }
  ' crates/nexus-agent-tools/src/ui_styling.rs 2>&1; then
    echo "!! vocabolario-stile-nel-db: il criterio nomina un framework." >&2
    echo "   La domanda e' «le classi hanno una fonte?», non «c'e' <framework>?»." >&2
    echo "   I nomi vivono in settings.agent.ui_styling.utility_frameworks /" >&2
    echo "   .runtime_packages: aggiungerne uno deve restare una riga, non un deploy." >&2
    fail=1
  else
    echo "OK vocabolario-stile-nel-db: il criterio non nomina nessuna istanza"
  fi
else
  echo "!! vocabolario-stile-nel-db: modulo ui_styling.rs non trovato." >&2
  echo "   Il check non ha raggiunto il suo oggetto (regola O)." >&2
  fail=1
fi

# «Qual e' la richiesta dell'utente per QUESTO turno?» (2026-07-29)
#
# Una domanda, una risposta: il task fissato all'origine del run. Aveva DUE
# consumatori con due euristiche diverse sulla cronologia — il supervisore col
# primo `Message::Human` (incidente Chat 11), il focus del turno con l'ultimo,
# che in un run agentico e' un tool_result o un `<system-reminder>`.
assert_single "task-del-turno" 'fn current_turn_task' \
  'crates/nexus-agent-graph/src/decisions/turn_task.rs' crates
assert_single "chiave-task-del-turno" '"original_task"' \
  'crates/nexus-agent-graph/src/decisions/turn_task.rs' crates

# La stessa domanda per un SUB-run: il suo messaggio e' il mandato PIU' il
# contorno (contesto del chiamante, formato atteso), e fissare tutto il blocco
# come "la richiesta" era vero per il run principale e falso per ogni figura
# convocata — col focus che ne mostra i primi 600 caratteri, cioe' l'inizio del
# contorno, dichiarandoli al modello come il compito da svolgere. Le due forme
# nascono INSIEME qui: chi ricompone la decorazione altrove puo' consegnare una
# richiesta diversa da quella decorata, e nessun tipo lo fermerebbe.
assert_single "mandato-subagente" 'fn compose_subagent_mandate' \
  'crates/mcp-core/src/agent_tools/subagent_native.rs' crates

# Il guard che conta: il focus del turno NON torna a leggere la cronologia.
#
# Il ruolo `user` sul canale interno significa "questo lo legge il modello",
# non "questo lo ha scritto l'utente": tool_dispatch, i promemoria e i nudge
# anti-stallo producono tutti `Message::Human`. Un ripiego sulla history —
# anche solo "se il task manca, prendi l'ultimo messaggio" — rimette in piedi
# il difetto, e lo rimette dove nessun test lo guarda: nel caso in cui il dato
# non c'e'. Si guardano le sole righe di CODICE fino a `#[cfg(test)]`: nei
# commenti quei nomi servono a spiegare il difetto, nei test sono la fixture.
if [[ -f crates/nexus-agent-graph/src/decisions/turn_focus.rs ]]; then
  if awk '
    /^#\[cfg\(test\)\]/ { exit }
    /^[[:space:]]*\/\// { next }
    /Message::Human|\.messages|state\.messages/ {
      trovato = 1; print "   riga " NR ": " $0 > "/dev/stderr"
    }
    END { exit !trovato }
  ' crates/nexus-agent-graph/src/decisions/turn_focus.rs 2>&1; then
    echo "!! focus-non-legge-la-cronologia: il focus del turno torna sui messaggi." >&2
    echo "   La richiesta si legge da decisions::turn_task (fissata all'origine)," >&2
    echo "   non dalla history: li' il ruolo 'user' non identifica l'utente." >&2
    fail=1
  else
    echo "OK focus-non-legge-la-cronologia: il focus legge il task, non i messaggi"
  fi
else
  echo "!! focus-non-legge-la-cronologia: modulo turn_focus.rs non trovato." >&2
  echo "   Il check non ha raggiunto il suo oggetto (regola O)." >&2
  fail=1
fi


# «Questo tool ha fallito?» (2026-07-30, censimento confronti semantici)
#
# I tool agente ritornano una String nuda: l'unico canale d'errore e' il
# marker U+274C in testa al risultato (nexus-types::tool_outcome). Tre
# consumatori (anti-loop, final_gate, supervisore) lo leggevano prima da un
# vocabolario testuale ricopiato a mano in piu' punti — regola L violata dalla
# fonte. tool_failure/is_tool_failure devono restare gli UNICI costruttore e
# riconoscitore del contratto: se ricompaiono altrove, il prossimo consumatore
# torna a indovinare.
assert_single "contratto-fallimento-tool" 'fn tool_failure\(|fn is_tool_failure\(' \
  'crates/nexus-types/src/tool_outcome.rs' crates

# Il guard che conta: nessuna riga di CODICE (non commenti, non test — dove
# asserire che un produttore emetta il marker e' la fixture legittima, non una
# violazione) ricostruisce a mano la condizione che is_tool_failure incapsula
# (`.starts_with()` sul marker). E' esattamente la duplicazione che c'era prima
# di questo fix fra tool_runner_server.rs e signals.rs, con vocabolari
# leggermente diversi che potevano divergere in silenzio. Il filtro commenti/test
# passa da awk (stessa forma di `stile-applicato`/`focus-non-legge-la-cronologia`
# sopra); il match sul marker passa da grep -F (stringa fissa, niente ambiguita'
# di escape fra bash/awk/regex sulla sequenza `\u{274C}`).
contratto_ricopiato=""
for f in $(grep -rlF "starts_with(" --include='*.rs' \
    --exclude-dir=target --exclude-dir=node_modules crates 2>/dev/null); do
  [[ "$f" == "crates/nexus-types/src/tool_outcome.rs" ]] && continue
  hit="$(awk '
    /^#\[cfg\(test\)\]/ { exit }
    /^[[:space:]]*\/\// { next }
    { print NR ": " $0 }
  ' "$f" | grep -F -e "starts_with('\u{274C}')" -e "starts_with('\u{274c}')" || true)"
  [[ -n "$hit" ]] && contratto_ricopiato+="  $f"$'\n'"$hit"$'\n'
done
if [[ -n "$contratto_ricopiato" ]]; then
  echo "!! contratto-fallimento-tool: la condizione del marker e' ricopiata a mano:" >&2
  printf '%s' "$contratto_ricopiato" >&2
  echo "   Chiama nexus_types::tool_outcome::is_tool_failure(risultato)." >&2
  fail=1
else
  echo "OK contratto-fallimento-tool: nessun call site ricopia la condizione del marker"
fi

# L'esito di un tool sta in un CAMPO, non nel testo (2026-08-01, regola Q)
#
# `RispostaTool` porta `esito`/`exit_code` accanto al testo, e il ponte
# `da_testo_legacy` e' l'UNICO punto autorizzato a ricostruirli dalla stringa —
# finche' i tool non sono tutti migrati. Il marker in testa a una stringa e' un
# campo travestito da prosa: `is_tool_failure` guarda la testa, e due
# composizioni legittime del repo vi anteponevano prosa di successo, lasciando
# l'apparato anti-loop della firma "servizio non in ascolto" irraggiungibile per
# costruzione. Nessun test poteva accorgersene, perche' il contratto non era un
# tipo.
assert_single "risposta-tool-ponte-legacy" 'fn da_testo_legacy\('   'crates/nexus-types/src/tool_outcome.rs' crates

# Il guard che conta: nessun CONSUMATORE decide l'esito rileggendo il testo di
# una RispostaTool. Il campo c'e': leggerlo dalla stringa e' tornare al difetto
# con il tipo giusto in mano. Esclusi commenti e test (dove costruire il caso
# legacy e' la fixture legittima).
# Le sole righe di CODICE fino a `#[cfg(test)]`: in un test asserire che un
# tool migrato NON scriva il marker e' la fixture che PROVA il contratto, non
# una violazione (stessa forma del guard sopra).
esito_dal_testo=""
for f in $(grep -rlE --include='*.rs' --exclude-dir=target     'is_tool_failure\(&?[a-z_]+\.testo' crates 2>/dev/null); do
  hit="$(awk '
    /^#\[cfg\(test\)\]/ { exit }
    /^[[:space:]]*\/\// { next }
    /is_tool_failure\(&?[a-z_]+\.testo/ { print NR ": " $0 }
  ' "$f" || true)"
  [[ -n "$hit" ]] && esito_dal_testo+="  $f"$'
'"$hit"$'
'
done
if [ -n "$esito_dal_testo" ]; then
  echo "!! risposta-tool-esito-dal-campo: un consumatore rilegge il testo per" >&2
  echo "   sapere com'e' andata, avendo il campo a disposizione:" >&2
  printf '%s' "$esito_dal_testo" >&2
  echo "   Usa risposta.esito.e_fallito() (regola Q)." >&2
  fail=1
else
  echo "OK risposta-tool-esito-dal-campo: l'esito si legge dal campo"
fi

# Un conflitto di unicita' si riconosce dal CODICE, mai dal messaggio (2026-08-02)
#
# Il Display di sqlx::Error dipende da `lc_messages` del server: MISURATO il
# 01/08/2026 su questo Postgres, che risponde in italiano — «un valore chiave
# duplicato viola il vincolo univoco "..."» — dove ne' "unique" ne' "duplicate"
# compaiono. Ogni `contains` su quelle parole era gia' cieco, non a rischio di
# diventarlo, e i difetti che ne nascevano erano opposti: un profilo con nome
# gia' preso rispondeva 500 "errore interno", e una direttiva dava 409 anche
# quando il DB era irraggiungibile.
assert_single "conflitto-unicita-dal-codice" 'pub fn is_unique_violation\('   'crates/nexus-types/src/db_error.rs' crates

# Il guard che conta: nessuno riconosce il conflitto dal testo dell'errore.
# Escluse le righe di commento (dove la parola compare per SPIEGARE il difetto).
conflitto_dal_testo=""
for f in $(grep -rlE --include='*.rs' --exclude-dir=target     'to_string\(\)\.contains\("(unique|duplicate|uq_)' crates 2>/dev/null); do
  hit="$(awk '
    /^[[:space:]]*\/\// { next }
    /to_string\(\)\.contains\("(unique|duplicate|uq_)/ { print NR ": " $0 }
  ' "$f" || true)"
  [[ -n "$hit" ]] && conflitto_dal_testo+="  $f"$'
'"$hit"$'
'
done
if [ -n "$conflitto_dal_testo" ]; then
  echo "!! conflitto-unicita-dal-codice: il conflitto e' riconosciuto dal testo" >&2
  echo "   dell'errore, che dipende dalla lingua del server:" >&2
  printf '%s' "$conflitto_dal_testo" >&2
  echo "   Usa nexus_types::db_error::is_unique_violation(&e) (SQLSTATE 23505)." >&2
  fail=1
else
  echo "OK conflitto-unicita-dal-codice: nessun riconoscimento dal messaggio"
fi

# Il corpo che parte da un endpoint OpenAI-compat nasce in UN punto (2026-07-30)
#
# La sequenza "risolvi la preferenza di fornitore -> costruisci il body col
# dialetto di cache del client -> applica i quirk di forma" era ricopiata in
# `complete_with_reasoning` e `stream_with_reasoning`. La duplicazione non era
# solo debito: era la ragione per cui nessun test la attraversava (i test
# chiamavano `build_request_body` a mano passando dialetto e ordine, cioe'
# fissando l'assunto da verificare). MISURATO: revocando i tre livelli di
# affinita' nei due call site, `cargo test -p nexus-gateway` restava a 407 verdi.
# Se la chiamata torna a essere piu' di una, la copertura di quel percorso
# scende a meta' senza che nulla lo dica.
if [[ -f crates/nexus-gateway/src/providers/openai_compat.rs ]]; then
  if awk '
    /^#\[cfg\(test\)\]/ { exit }
    /^[[:space:]]*\/\// { next }
    /build_request_body\(/ && !/fn build_request_body\(/ {
      n++; righe = righe "   riga " NR ": " $0 "\n"
    }
    END {
      if (n == 1) { exit 1 }
      printf "   chiamate trovate: %d\n%s", n, righe > "/dev/stderr"
      exit 0
    }
  ' crates/nexus-gateway/src/providers/openai_compat.rs 2>&1; then
    echo "!! corpo-richiesta-openai-compat: il corpo non nasce in un punto solo." >&2
    echo "   Attesa UNA chiamata a build_request_body, dentro" >&2
    echo "   OpenAiCompatClient::corpo_della_richiesta: complete e stream delegano" >&2
    echo "   a quella. Zero chiamate = la giunzione e' stata rimossa; piu' di una =" >&2
    echo "   e' tornata duplicata, e i test ne attraversano solo un ramo (regola O)." >&2
    fail=1
  else
    echo "OK corpo-richiesta-openai-compat: una sola giunzione verso il wire"
  fi
else
  echo "!! corpo-richiesta-openai-compat: modulo openai_compat.rs non trovato." >&2
  echo "   Il check non ha raggiunto il suo oggetto (regola O)." >&2
  fail=1
fi

# L'esito di una suite di test nasce in UN punto (2026-08-01)
#
# La stessa suite veniva eseguita da TRE attori che non si riconoscevano
# (final_gate come criterio, agente con run_playwright_tests, ciclo review dopo
# ogni rimando): 53 esecuzioni in una serata sulla stessa app, 31 rosse e 21
# verdi, perche' l'esito non era legato allo stato del codice e un rosso
# instabile veniva letto come difetto reale. Il vocabolario dell'esito, la
# classificazione e la chiave di stato devono restare in un posto solo: se
# ricompaiono altrove, il prossimo consumatore torna a rispondersi da se'.
assert_single "suite-outcome" 'enum SuiteOutcome' \
  'crates/mcp-core/src/suite_verification/mod.rs' crates
assert_single "suite-classifica-esito" 'fn classifica_esito' \
  'crates/mcp-core/src/suite_verification/mod.rs' crates
assert_single "suite-riconoscimento" 'fn e_suite_playwright' \
  'crates/mcp-core/src/suite_verification/mod.rs' crates
assert_single "suite-chiave-di-stato" 'fn digest_albero' \
  'crates/mcp-core/src/suite_verification/state_key.rs' crates

# Il guard che conta: gli artefatti del runner NON entrano nella chiave di
# stato. Playwright riscrive `test-results/` e `playwright-report/` a ogni
# esecuzione: contarli renderebbe la chiave diversa subito dopo ogni run, la
# memoria non risponderebbe MAI e il presidio sarebbe inerte pur essendo tutto
# scritto e testato — la forma di guasto che non si vede (regola O).
for _dir in test-results playwright-report; do
  if ! grep -q "\"$_dir\"" crates/mcp-core/src/suite_verification/state_key.rs; then
    echo "!! chiave-di-stato: '$_dir' non e' fra le DIRECTORY_ESCLUSE." >&2
    echo "   Il runner riscrive quella directory a ogni esecuzione: senza" >&2
    echo "   l'esclusione la chiave cambia dopo ogni run e la memoria degli" >&2
    echo "   esiti diventa inerte in silenzio." >&2
    fail=1
  fi
done
[[ "$fail" -eq 0 ]] && echo "OK chiave-di-stato: gli artefatti del runner restano fuori dalla chiave"
# Esecutore unico della suite Playwright (2026-08-01). Il riconoscimento della
# riga vive in playwright_cli.rs; la riga `npx playwright test ...` la costruisce
# solo build_playwright_command, dentro il runner. Due esecutori significano due
# contratti per la stessa suite: il secondo (run_command/run_tests, che la
# lanciavano in proprio e ne registravano il job a posteriori) partiva senza
# BASE_URL derivata dalle porte, senza preflight e senza attendere che il
# servizio bersaglio fosse pronto, e nel pannello il suo esito era
# indistinguibile da quello vero.
assert_single "esecutore-suite-playwright" 'fn invocazione_suite' \
  'crates/mcp-core/src/agent_tools/playwright_cli.rs' crates

# ── scomposizione-riga-shell (2026-08-05) ───────────────────────────────────
# La SCOMPOSIZIONE di una riga shell in comandi/parole/env/redirezioni ha un
# punto unico in nexus-agent-graph: playwright_cli (riconoscimento suite),
# avvio_server (avvia un server?) e step_gate (matcher command_token) delegano.
# Prima esistevano DUE scompositori indipendenti che divergevano nel silenzio
# (`2>&1` produceva un comando fantasma `["1"]` nell'ex tokenizzatore di
# step_gate). La `pub fn comandi` deve stare SOLO nel modulo del punto unico.
assert_single "scomposizione-riga-shell" 'pub fn comandi' \
  'crates/nexus-agent-graph/src/decisions/shell_command.rs' crates

# ── portata-del-passo (2026-08-09) ──────────────────────────────────────────
# «Che cosa raggiunge questo passo, e chi lo puo' disfare?» ha UN punto unico:
# e' il pavimento di criticita' del gate duale, al posto del dentro/fuori dal
# vocabolario dei mutatori. Il difetto che ha chiuso: `run_command` cadeva in
# `Mutating` come una `edit_file`, e `Mutating` non convoca in nessuna
# modalita' — misurato il 09/08/2026, `dotnet ef database update` eseguito 5
# volte senza che nessun giudice lo vedesse, e 45 sole righe `step_validation`
# in tutto lo storico del progetto, l'ultima dello sviluppo del gate.
assert_single "portata-del-passo" 'pub fn classifica_portata' \
  'crates/nexus-agent-graph/src/decisions/step_reach.rs' crates

# La collocazione di un path rispetto all'albero e' la stessa domanda per la
# portata e per il declassamento degli irreversibili: due normalizzazioni
# darebbero due idee diverse di «dentro».
assert_single "portata-del-passo" 'pub fn colloca_path' \
  'crates/nexus-agent-graph/src/decisions/step_reach.rs' crates

# Il livello base di classify_step NON torna a nascere dal vocabolario dei
# mutatori: e' esattamente la conflazione che rendeva il gate inerte. Cerca la
# risalita diretta dentro step_gate.rs, dove la delega deve passare da
# step_reach::classifica_portata.
# Il filtro toglie le righe di commento: la documentazione del modulo CITA il
# difetto per spiegarlo, e un guard che confonde la spiegazione con la
# regressione costringerebbe a non documentarla.
if grep -nE 'is_mutator_tool_name' crates/nexus-agent-graph/src/decisions/step_gate.rs \
   | grep -vE '^[0-9]+: *(//|/\*|\*)' >/dev/null 2>&1; then
  echo "!! portata-del-passo: step_gate torna a derivare il livello base dal" >&2
  echo "   vocabolario dei mutatori. E' la conflazione misurata il 09/08/2026:" >&2
  echo "   una run_command che esegue una migrazione di schema finisce nello" >&2
  echo "   stesso livello di una edit_file, e quel livello non convoca mai." >&2
  echo "   Il pavimento deve venire da step_reach::classifica_portata." >&2
  fail=1
else
  echo "OK portata-del-passo: il pavimento di step_gate viene dalla portata"
fi

# La SOGLIA sul costo e' l'unico elenco che ASSOLVE, e vive nel DB (regola G).
# Un vocabolario cablato nel codice sarebbe due verita': l'operatore ne
# modificherebbe una e il gate leggerebbe l'altra. Il criterio non deve nominare
# nessun comando: li riceve dal chiamante.
if grep -nE '"(ls|cat|pwd|git status|git diff|git log)"' \
     crates/nexus-agent-graph/src/decisions/step_reach.rs \
   | grep -vE '^[0-9]+: *(//|/\*|\*)' \
   | awk -v inizio="$(grep -n '#\[cfg(test)\]' crates/nexus-agent-graph/src/decisions/step_reach.rs | head -1 | cut -d: -f1)" \
         -F: '$1 < inizio' | grep . >/dev/null 2>&1; then
  echo "!! vocabolario-osservazione: step_reach nomina un comando concreto fuori" >&2
  echo "   dai test. La soglia sul costo e' un DATO nel DB" >&2
  echo "   (orchestrator.step_reach.observation_commands, mig 0688): cablarla nel" >&2
  echo "   codice creerebbe una seconda verita' che l'operatore non puo' correggere." >&2
  fail=1
else
  echo "OK vocabolario-osservazione: il criterio non nomina nessun comando"
fi

# L'assoluzione e' per RICONOSCIMENTO: un vocabolario vuoto non assolve nessuno.
# E' il verso che distingue questo elenco da critical_step_rules, e senza la
# guardia sul vuoto un DB non seminato renderebbe il gate di nuovo inerte —
# nella direzione opposta, e altrettanto silenziosa.
if grep -q 'if vocabolario.is_empty() {' crates/nexus-agent-graph/src/decisions/step_reach.rs; then
  echo "OK vocabolario-osservazione: il vocabolario vuoto non assolve nessuno"
else
  echo "!! vocabolario-osservazione: sparita la guardia sul vocabolario vuoto." >&2
  echo "   Senza, un DB non seminato deciderebbe da solo chi e' innocuo." >&2
  fail=1
fi

# Regressione diretta: la registrazione a posteriori non deve tornare. Cerca la
# DEFINIZIONE, non il nome: il commento che ne spiega la rimozione lo cita.
if grep -rEln --include='*.rs' --exclude-dir=target 'fn record_playwright_job' crates >/dev/null 2>&1; then
  echo "!! esecutore-suite-playwright: e' ricomparsa una registrazione del job" >&2
  echo "   Playwright fuori dal runner. Il job lo scrive chi ha eseguito la suite," >&2
  echo "   che e' l'unico a sapere quali test sono partiti; registrarlo a valle di" >&2
  echo "   un comando generico produce nel pannello esiti indistinguibili da quelli" >&2
  echo "   veri, e per giunta su comandi che test non sono (install, show-report)." >&2
  fail=1
else
  echo "OK esecutore-suite-playwright: nessuna registrazione job fuori dal runner"
fi

# ── ambiente-dichiarato (2026-08-02) ────────────────────────────────────────
# L'host su cui l'agente eseguira' i comandi e' un FATTO che si rileva in un
# punto solo, e che deve arrivare in ENTRAMBI i contesti di esecuzione. Il
# difetto: la figura `verify` (sub-run a5f7419c) ha bruciato 180s e 16 iterazioni
# per scoprire a tentativi di essere su Windows, e il system prompt le diceva nel
# frattempo di installare con `sudo apt-get`.
#
# 1. La FORMA del blocco vive solo nel rilevatore: un call site che se la
#    riscrivesse produrrebbe una seconda dichiarazione d'ambiente, e la prima
#    volta che divergessero il prompt direbbe due cose diverse sullo stesso host.
assert_single "blocco-ambiente-prompt" '<ambiente_esecuzione>' \
  'crates/nexus-agent-tools/src/ambiente.rs' crates
# 2. La SHELL dichiarata e' quella che esegue davvero: si chiede al punto unico
#    che lancia i comandi, mai a un `cfg!(windows)` scritto qui — un ramo a
#    codice non vedrebbe l'override `NEXUS_SHELL` ne' un percorso
#    d'installazione diverso, e direbbe all'agente una shell che non e' la sua.
if ! grep -q 'nexus_tool_kit::sandbox::agent_shell()' \
  crates/nexus-agent-tools/src/ambiente.rs 2>/dev/null; then
  echo "!! ambiente-dichiarato: la shell del blocco non viene piu' da agent_shell()." >&2
  echo "   E' il punto unico che ESEGUE i comandi dell'agente: dedurla altrimenti" >&2
  echo "   significa dichiarargli una shell diversa da quella che lo eseguira'." >&2
  fail=1
else
  echo "OK ambiente-dichiarato: la shell dichiarata e' quella che esegue"
fi
# 3. L'innesto sta DENTRO i due compositori di system prompt, non nei loro
#    chiamanti (stessa forma del guard `memorie-nel-prompt`): li' comporre un
#    prompt di esecuzione senza il fatto tornerebbe possibile, e un prompt senza
#    quel blocco e' perfettamente valido — la regressione sarebbe invisibile.
amb_mancanti=""
amb_innesto() {
  local file="$1" fn="$2"
  awk -v fn="$fn" '
    index($0, fn) { dentro = 1 }
    dentro && /nexus_prompt::ambiente::con_ambiente/ { print "trovato"; exit }
    dentro && /^}/ { exit }
  ' "$file" 2>/dev/null
}
if [[ -z "$(amb_innesto crates/mcp-core/src/chat_messages/handlers.rs 'async fn compose_chat_system_context')" ]]; then
  amb_mancanti+="  compose_chat_system_context (chat)"$'\n'
fi
if [[ -z "$(amb_innesto crates/mcp-core/src/agent_tools/subagent_native.rs 'async fn resolve_system_text')" ]]; then
  amb_mancanti+="  resolve_system_text (sub-run / figure del consiglio)"$'\n'
fi
# Il terzo: il compositore del run AGENTICO, da cui passano i percorsi FUORI
# chat (process_resume, remediation di servizi e risorse). Sono quelli che
# eseguono comandi di sistema senza che nessuno guardi, quindi i piu' esposti a
# un'indicazione sbagliata sulla piattaforma.
if [[ -z "$(amb_innesto crates/mcp-core/src/chat_messages/agent_run.rs 'async fn compose_agent_system_text')" ]]; then
  amb_mancanti+="  compose_agent_system_text (run agentico, resume e remediation)"$'\n'
fi
if [[ -n "$amb_mancanti" ]]; then
  echo "!! ambiente-dichiarato: un contesto di esecuzione non riceve piu' l'ambiente:" >&2
  printf '%s' "$amb_mancanti" >&2
  echo "   L'innesto deve restare DENTRO il compositore: nel chiamante, comporre" >&2
  echo "   quel system prompt senza il fatto tornerebbe possibile." >&2
  fail=1
else
  echo "OK ambiente-dichiarato: chat e sub-run ricevono entrambi l'ambiente reale"
fi

# ── processo-operativo (2026-08-04) ─────────────────────────────────────────
# Il processo di implementazione standard (mig 0674) e' UN template DB innestato
# dai compositori: appenderlo alle chiavi avrebbe raggiunto solo le figure di
# oggi, non quelle create domani dal wizard. Tre presidi:
#
# 1. La FORMA (tag) vive solo nel modulo: un secondo produttore del tag sarebbe
#    una seconda autorita' sullo stesso processo.
assert_single "blocco-processo-prompt" '<processo_implementazione>' \
  'crates/nexus-prompt/src/processo.rs' crates
# 2. L'innesto sta DENTRO i tre compositori di system prompt (stessa terna di
#    `ambiente-dichiarato`): nel chiamante, comporre un prompt di esecuzione
#    senza processo tornerebbe possibile e la regressione sarebbe invisibile.
proc_mancanti=""
proc_innesto() {
  local file="$1" fn="$2"
  awk -v fn="$fn" '
    index($0, fn) { dentro = 1 }
    dentro && /nexus_prompt::processo::/ { print "trovato"; exit }
    dentro && /^}/ { exit }
  ' "$file" 2>/dev/null
}
if [[ -z "$(proc_innesto crates/mcp-core/src/chat_messages/handlers.rs 'async fn compose_chat_system_context')" ]]; then
  proc_mancanti+="  compose_chat_system_context (chat)"$'\n'
fi
if [[ -z "$(proc_innesto crates/mcp-core/src/agent_tools/subagent_native.rs 'async fn resolve_system_text')" ]]; then
  proc_mancanti+="  resolve_system_text (sub-run / figure)"$'\n'
fi
if [[ -z "$(proc_innesto crates/mcp-core/src/chat_messages/agent_run.rs 'async fn compose_agent_system_text')" ]]; then
  proc_mancanti+="  compose_agent_system_text (run agentico)"$'\n'
fi
if [[ -n "$proc_mancanti" ]]; then
  echo "!! processo-operativo: un contesto di esecuzione non riceve piu' il processo:" >&2
  printf '%s' "$proc_mancanti" >&2
  fail=1
else
  echo "OK processo-operativo: i tre compositori innestano il processo standard"
fi
# 3. Il discriminante advisory resta il punto unico: un elenco di nomi al suo
#    posto e' la regressione del commit 303e1437 (chi da' pareri trattato come
#    chi scrive).
if ! grep -q 'figure_advisory::is_advisory_kind' \
  crates/nexus-prompt/src/processo.rs 2>/dev/null; then
  echo "!! processo-operativo: prompt_processo non discrimina piu' con is_advisory_kind." >&2
  fail=1
else
  echo "OK processo-operativo: le advisory sono discriminate dal punto unico"
fi

# ── causa-del-timeout (2026-08-02) ──────────────────────────────────────────
# «Tempo scaduto» e' vero e non serve a nulla: non distingue un run fermo su una
# strada chiusa da uno che stava lavorando, e solo per il secondo ha senso
# chiedersi se il tetto della figura sia dimensionato bene.
#
# 1. Il vocabolario delle cause vive nel punto unico. Un secondo posto che
#    decidesse "questo timeout e' una ripetizione" darebbe due diagnosi per lo
#    stesso run.
assert_single "causa-timeout" 'fn classifica_causa_timeout' \
  'crates/nexus-agent-graph/src/decisions/timeout_cause.rs' crates
# 2. La chiusura in scadenza deve INTERROGARLO. Senza, `finalize_timeout` torna
#    a scrivere la sola parola "timeout" — con tutti i test verdi, perche' un
#    esito senza causa resta un esito valido.
if [[ -z "$(awk '
  /async fn finalize_timeout/ { dentro = 1 }
  dentro && /subagent_timeout::causa_del_timeout/ { print "trovato"; exit }
  dentro && /^}/ { exit }
' crates/mcp-core/src/agent_tools/subagent_native.rs 2>/dev/null)" ]]; then
  echo "!! causa-timeout: la chiusura in scadenza non chiede piu' la causa." >&2
  echo "   finalize_timeout deve interrogare subagent_timeout::causa_del_timeout:" >&2
  echo "   senza, il pannello torna a dire 'tempo scaduto' e basta." >&2
  fail=1
else
  echo "OK causa-timeout: la chiusura in scadenza dichiara su cosa e' finito il budget"
fi
# Il criterio di TERMINAZIONE di una figura e' il progresso (2026-08-09, ADR 0044)
#
# Il tetto fisso per kind rispondeva alla domanda sbagliata: MISURATO il
# 09/08/2026 su gestione-corsi, quattro figure su nove uccise mentre lavoravano
# (4, 5, 17 e 22 passi persistiti, tutte con causa `NoFailureAtEnd`). Alzare il
# numero e' la toppa che la regola H vieta per nome.
#
# 1. Il criterio vive nel punto unico. Un secondo posto che decidesse "questa
#    figura non sta avanzando" darebbe due verdetti sullo stesso run.
assert_single "avanzamento-figura" 'fn decidi_prosecuzione|fn classifica_avanzamento' \
  'crates/nexus-agent-graph/src/decisions/avanzamento_figura.rs' crates
# 2. Il gate di deadline deve INTERROGARLO. Senza, torna al confronto
#    `elapsed >= budget` — con tutti i test verdi, perche' un run chiuso a tempo
#    resta un run chiuso. E' il difetto misurato, e rientrerebbe in silenzio.
if [[ -z "$(awk '
  /async fn gate_deadline_run/ { dentro = 1 }
  dentro && /prosecuzione_del_run/ { print "trovato"; exit }
  dentro && /^    }/ { exit }
' crates/nexus-agent-graph/src/nodes/executor.rs 2>/dev/null)" ]]; then
  echo "!! avanzamento-figura: il gate di deadline non chiede piu' se la figura avanza." >&2
  echo "   gate_deadline_run deve interrogare prosecuzione_del_run: senza, il" >&2
  echo "   criterio torna a essere l'orologio e una figura che lavora muore al tetto." >&2
  fail=1
else
  echo "OK avanzamento-figura: il gate di deadline chiede se la figura sta avanzando"
fi
# 3. Il tetto assoluto si calcola in UN posto: e' il numero su cui il timeout
#    esterno e il budget del motore devono concordare. Se divergessero, il piu'
#    stretto vincerebbe in silenzio e il criterio sarebbe inerte proprio nel caso
#    per cui esiste.
assert_single "tetto-assoluto-figura" 'fn tetto_assoluto_s' \
  'crates/mcp-core/src/agent_tools/subagent_native.rs' crates

# La RESA dell'elenco servizi si compone in un punto solo (2026-08-02, regole L+Q)
#
# `list_active_services` componeva la riga a mano, un `push_str` per colonna, con
# l'ordine dettato dalle colonne del SELECT invece che dall'importanza per chi
# legge: uuid e PID prima dello stato, `created_at` stampato come RFC3339 con
# microsecondi e fuso (32 caratteri per dire "stamattina"), e uno stato fuori
# vocabolario ridotto a `[?]`, un marcatore che non diceva nemmeno se fosse
# peggio di `[ATTIVO]`. Nel nastro attivita' (font monospaziato, a capo
# automatico, taglio a 500 caratteri) tre servizi in quella forma riempivano il
# riquadro, e QUALE/VIVO/PORTA - le tre cose che si volevano sapere - annegavano.
#
# Il testo lo leggono in DUE, il modello e l'utente, e sono lo STESSO testo: e'
# il vincolo che rende la resa una decisione unica e non una preferenza di chi
# stampa. Confinarla qui non e' cosmesi: e' cio' che impedisce a un secondo
# formattatore di nascere accanto al primo con un'altra idea di cosa venga prima.
#
# Cosa NON misura questo guard: che i CAMPI siano quelli giusti (lo misurano i
# test del modulo, che partono da `ProcessSummary` come lo produce
# `list_processes_from`). Misura solo che la composizione resti una sola.
assert_single "resa-elenco-servizi" 'fn elenco_da_processi\(|fn eta_leggibile\(' \
  'crates/mcp-core/src/agent_tools/service_listing.rs' crates
# L'esito di un tool arriva al MODELLO in un campo (2026-08-02, regola Q)
#
# Il canale strutturato attraversa la catena fino al confine col provider, e
# li' ogni dialetto fa cio' che il suo protocollo consente: Anthropic ha
# `is_error` sul blocco tool_result e l'adapter lo emette nativo; OpenAI-compat
# e Google non hanno un campo equivalente, e il degrado e' DICHIARATO in un
# punto solo, che compone il testo DAL campo. Una seconda composizione altrove
# tornerebbe a spargere il vocabolario dell'esito nella prosa, cioe' il difetto
# da cui la regola Q nasce: quando il marker viveva nel testo, due composizioni
# legittime lo spingevano fuori dalla testa e il fallimento smetteva di essere
# riconosciuto.
assert_single "canale-esito-senza-campo" 'fn testo_con_esito_dichiarato\(' \
  'crates/nexus-gateway/src/providers/tool_error_channel.rs' crates
assert_single "prefisso-esito-senza-campo" 'PREFISSO_FALLIMENTO: &str' \
  'crates/nexus-gateway/src/providers/tool_error_channel.rs' crates

# -- passo-persistito (2026-08-02) -------------------------------------------
#
# Che cosa arriva in colonna su `agent_steps` lo dice UN tipo, e la sola impl
# che scrive lo destruttura senza interpretare. Il difetto nasceva proprio
# dall'assenza del tipo: contratto fatto di due JSON opachi, produttore e
# consumatore con chiavi diverse per la stessa cosa, e lo status derivato dal
# segnale strutturato scartato per un letterale. 8860 step su 8860 anonimi e
# dichiarati riusciti, 536 fallimenti reali compresi. Una seconda derivazione
# dei campi fuori dal produttore, o un secondo vocabolario dello status, ricrea
# la giunzione che nessun tipo sorveglia.
assert_single "passo-persistito" 'pub struct PersistedStep'   'crates/nexus-agent-graph/src/runtime/ports.rs' crates
assert_single "vocabolario-esito-passo" 'pub enum StepStatus'   'crates/nexus-agent-graph/src/runtime/ports.rs' crates

# ── La POSIZIONE di un blocco nel system la decide il punto unico (2026-08-02) ─
#
# Un fornitore riusa il prefisso di una richiesta solo se i primi token sono
# IDENTICI a quelli di una richiesta gia' vista: un blocco RICALCOLATO messo in
# testa taglia il riuso di tutto cio' che lo segue. Il criterio vive in un punto
# solo, `nexus_types::system_prompt` (CONFINE_DI_TURNO / appendi_blocco_di_turno
# / parte_stabile), da cui derivano sia la `prompt_cache_key` di openai_compat
# sia il breakpoint `cache_control` di anthropic.
#
# PERCHE' UN GUARD, e non i soli test: una composizione a mano non fa fallire
# NULLA. Un prompt con la testa instabile e' corretto in tutto tranne che nel
# prezzo, e i test delle singole iniezioni restano verdi — `starts_with("SYS")`
# e la presenza del marker sono veri in entrambe le implementazioni, quindi non
# le distinguono. Senza guard la regressione rientra in silenzio.
#
# MISURATO il 02/08/2026: `inject_verification_directive` componeva il system a
# mano, accodando al termine di QUALUNQUE stringa le arrivasse. Innocua solo
# perche' l'unico chiamante invocava il focus per primo: una correttezza che
# vive nell'ORDINE DELLE CHIAMATE, cioe' la condizione che la regola L vieta.
#
# Tre pattern, perche' uno solo si aggira:
#   A) il letterale del confine si scrive in un posto solo — chi lo ri-digita
#      salterebbe qualunque check basato sull'identificatore;
#   B) nessuno accosta a mano il confine a un blocco;
#   C) nessuno APPENDE al system con un `format!` che parte dalla variabile del
#      system. E' la forma del difetto reale, che NON nominava il confine: un
#      guard sul solo confine non l'avrebbe vista;
#   D) la stessa cosa scritta con `push_str` invece che con `format!`. Senza
#      questa, il difetto rientrerebbe riscritto in tre righe invece che in una.
#      Ristretto a nexus-agent-graph, dove vivono le iniezioni di turno: in
#      mcp-core il meta-reasoner compone col push_str un system a chiamata
#      singola, che non ha parte di turno e non e' l'oggetto di questo guard.
#
# Cosa NON misura: che la posizione scelta sia quella GIUSTA per quel blocco (lo
# dicono i test del modulo). Misura che la scelta sia fatta in un posto solo.
# Fuori portata per costruzione, e volutamente: `inject_language_reminder`, che
# ANTEPONE un testo costante del run (motivato nel suo doc comment), e i system
# a chiamata singola del meta-reasoner, che non hanno una parte di turno.
sysprompt_letterale="$(grep -rIn -F '[[NEXUS_SYSTEM_DI_TURNO]]' --include='*.rs' crates/ 2>/dev/null | grep -v '^crates/nexus-types/src/system_prompt.rs:' || true)"
sysprompt_confine="$(grep -rInE '\{[A-Za-z_0-9:]*CONFINE_DI_TURNO\}|push_str\([^)]*CONFINE_DI_TURNO|(format|write|writeln|concat)!\([^)]*CONFINE_DI_TURNO' --include='*.rs' crates/ 2>/dev/null | grep -v '^crates/nexus-types/src/system_prompt.rs:' || true)"
sysprompt_append="$(grep -rInE 'format!\("\{(system|system_text|sys)\}[^"]' --include='*.rs' crates/ 2>/dev/null | grep -v '^crates/nexus-types/src/system_prompt.rs:' || true)"
sysprompt_pushstr="$(grep -rInE '(system|system_text|sys)\.push_str\(' --include='*.rs' crates/nexus-agent-graph/ 2>/dev/null || true)"
if [[ -n "$sysprompt_letterale" || -n "$sysprompt_confine" || -n "$sysprompt_append" || -n "$sysprompt_pushstr" ]]; then
  echo "!! composizione-system-prompt: il system e' composto fuori dal punto unico:" >&2
  if [[ -n "$sysprompt_letterale" ]]; then echo "$sysprompt_letterale" >&2; fi
  if [[ -n "$sysprompt_confine" ]]; then echo "$sysprompt_confine" >&2; fi
  if [[ -n "$sysprompt_append" ]]; then echo "$sysprompt_append" >&2; fi
  if [[ -n "$sysprompt_pushstr" ]]; then echo "$sysprompt_pushstr" >&2; fi
  echo "   La POSIZIONE di un blocco nel system la decide nexus_types::system_prompt" >&2
  echo "   (regola L): appendi_blocco_di_turno per cio' che si ricalcola a ogni turno," >&2
  echo "   componi_system_di_run per cio' che cambia fra un run e l'altro. Una" >&2
  echo "   composizione a mano e' corretta o sbagliata a seconda di CHI CHIAMA PRIMA," >&2
  echo "   e non fa fallire nulla: costa il prefisso che il fornitore non riusa piu'." >&2
  fail=1
else
  echo "OK composizione-system-prompt: la posizione dei blocchi resta al punto unico"
fi


# -- istruzioni-apprese-nel-prompt (2026-08-03) -------------------------------
#
# QUALI regole apprese entrano nel prompt, e COME si rendono, lo decide UN
# modulo. Il difetto era l'assenza del consumo, non la sua duplicazione: il
# distillatore scriveva 68 regole attive, il pannello le mostrava, il template
# esisteva col suo placeholder, e nessun compositore leggeva la tabella. Un
# secondo lettore che si componesse il blocco per conto proprio riaprirebbe la
# strada a due idee diverse di "quali regole valgono" e a un ordine non
# deterministico, che nella parte stabile del system costa il prefisso.
assert_single "istruzioni-apprese-nel-prompt" 'pub struct LearnedInstructions'   'crates/nexus-prompt/src/learned.rs' crates
# La tabella si legge da UN modulo per il prompt: chi la interroga altrove per
# comporre contesto sta ricreando il punto unico. Restano legittimi il
# distillatore che la scrive e le rotte admin che la mostrano.
apprese_fuori="$(grep -rIn -F 'FROM nexus_learned_instructions' --include='*.rs' crates/ 2>/dev/null | grep -v '^crates/nexus-prompt/src/learned.rs:' | grep -v '^crates/mcp-core/src/learned_instructions.rs:' || true)"
if [[ -n "$apprese_fuori" ]]; then
  echo "!! istruzioni-apprese-nel-prompt: la tabella e' letta fuori dai due punti ammessi:" >&2
  echo "$apprese_fuori" >&2
  echo "   Per il PROMPT esiste crates/nexus-prompt/src/learned.rs (regola L)." >&2
  fail=1
else
  echo "OK istruzioni-apprese-nel-prompt: la tabella resta ai due punti ammessi"
fi

# -- bind-come-domanda (2026-08-03) ------------------------------------------
#
# Un `TcpListener::bind(..).is_ok()` usato come DOMANDA («questa porta e'
# libera?») appiattisce tre casi in due: «libera», «occupata da un processo» e
# «il sistema non puo' rispondere» (WSAENOBUFS a pool di porte effimere
# esaurito, misurato in questo repo). Il terzo diventa indistinguibile dal
# secondo, e chi decide su quel booleano sceglie male: le due funzioni che
# SCELGONO la porta del bucket esaurivano tutte le candidate credendole occupate,
# e chi CANCELLA un'allocazione lo faceva su un esito che non sapeva leggere.
#
# Il punto unico e' `port_recovery::probe_bind -> PortBind{Libera|Occupata|
# NonInterrogabile}`. Bindare per SERVIRE (un server che si mette in ascolto)
# non e' una domanda e resta legittimo: il guard cerca il bind seguito da
# `.is_ok()`/`.is_err()`, cioe' la forma in cui il bind e' un test.
bind_domanda="$(grep -rIn -A 2 'TcpListener::bind' --include='*.rs' crates/ 2>/dev/null | grep -E '\.is_(ok|err)\(\)' | grep -v 'port_recovery.rs' || true)"
if [[ -n "$bind_domanda" ]]; then
  echo "!! bind-come-domanda: un bind e' usato come test fuori dal punto unico:" >&2
  echo "$bind_domanda" >&2
  echo "   Usa port_recovery::probe_bind, che distingue Occupata da NonInterrogabile." >&2
  fail=1
else
  echo "OK bind-come-domanda: nessun bind usato come test fuori dal punto unico"
fi

# -- listener-protetto-prima-del-kill (2026-08-04) ---------------------------
#
# «Questa porta e' autorizzata a QUESTO progetto?» e «questo listener si puo'
# uccidere?» sono due domande diverse. La seconda ha il suo punto unico,
# `is_protected_nexus_listener`, e il port_enforcer non gliela poneva: decideva
# sulla sola autorizzazione e sparava.
#
# MISURATO su `nexus_resource_audit`: 3.916 righe `port_violation_kill`, fra cui
# officina-veicoli che ha ucciso MCP-CORE — il processo che esegue l'enforcer —
# 845 volte su ciascuna delle porte 4000/50500/50501, e bacheca-attivita che ha
# ucciso i tre cluster Postgres e i servizi di sistema Windows. Basta un pid
# attribuito al progetto sbagliato perche' l'enforcer spari sull'infrastruttura.
#
# Il guard pretende che chi termina un processo per violazione di porta passi
# prima da quel punto: e' la difesa che regge anche quando l'attribuzione
# sbaglia.
enforcer_file="crates/mcp-core/src/security/port_enforcer.rs"
if [[ -f "$enforcer_file" ]]; then
  if grep -q 'kill_pid' "$enforcer_file" && ! grep -q 'is_protected_nexus_listener' "$enforcer_file"; then
    echo "!! listener-protetto-prima-del-kill: il port_enforcer termina processi senza chiedere" >&2
    echo "   a is_protected_nexus_listener se il listener e' infrastruttura Nexus." >&2
    echo "   Ha gia' ucciso mcp-core 845 volte: interponi il punto unico prima di kill_pid." >&2
    fail=1
  else
    echo "OK listener-protetto-prima-del-kill: l'enforcer interroga il punto unico"
  fi
fi

# ── scadenza-sospensione (2026-08-05) ───────────────────────────────────────
# «Questa sospensione la sciogliera' qualcuno?» ha UN punto di risposta. Il
# difetto (rilievo A4): il gate duale sospende in HITL anche in Automatic, dove
# nessun umano esiste; il run_reaper esclude awaiting_confirmation per contratto
# e ACTIVE_RUN_STATUSES lo conta fra i run che occupano la sessione, quindi il
# run notturno restava appeso per sempre.
#
# 1. Il CRITERIO (chi e' atteso, e per quanto) vive solo nel punto unico: un
#    call site che se lo riscrivesse deciderebbe con una seconda idea di quando
#    una sospensione muore, e la prima a divergere chiuderebbe i run di Confirm
#    mentre l'utente sta per approvare.
assert_single "scadenza-sospensione" 'pub fn classify_suspension' \
  'crates/nexus-agent-graph/src/decisions/suspension_watch.rs' crates
# 2. La SCRITTURA della scadenza sta in un punto solo, chiamato da entrambi i
#    percorsi che sospendono (chiusura del run e resume che si risospende): con
#    due scritture, il percorso dimenticato riproduce la sospensione eterna.
assert_single "scrittura-sospensione" 'async fn persist_suspension_watch' \
  'crates/mcp-core/src/chat_messages/agent_run.rs' crates
# 3. Una sospensione scaduta chiude con l'esito STRUTTURATO, mai come guasto:
#    `interrupted` direbbe "e' morto qualcosa" di un run che si e' fermato
#    esattamente dove doveva, e nasconderebbe la causa (regole M/Q).
reaper_file="crates/mcp-core/src/run_reaper.rs"
if [[ -f "$reaper_file" ]]; then
  if grep -q 'fn expire_on_pool' "$reaper_file" \
    && ! grep -q "blocked_needs_input" "$reaper_file"; then
    echo "!! scadenza-sospensione: la maturazione non chiude piu' con l'esito strutturato." >&2
    echo "   Una sospensione scaduta e' un run BLOCCATO con una causa dichiarata," >&2
    echo "   non un guasto: chiuderlo 'interrupted' perde il perche'." >&2
    fail=1
  else
    echo "OK scadenza-sospensione: la maturazione chiude blocked_needs_input"
  fi
fi

# ── identita-allocazione-porta (2026-08-06) ─────────────────────────────────
# «Di CHI e' questa porta nel registro?» ha UN punto di risposta, e la risposta
# non e' mai il numero di porta: l'identita' di un'allocazione e' la coppia
# (project_id, label).
#
# Il difetto, MISURATO su agenda-medica il 2026-08-06 e gia' documentato su
# bacheca-attivita (ADR 0042): il rilevamento porta-da-output risolveva il
# conflitto con `ON CONFLICT (port) DO UPDATE SET label`, cioe' chiunque
# stampasse quel numero nel proprio stdout rinominava l'allocazione di un altro
# servizio. `service_unit`, che dalla label DISCENDE, restava quella di prima:
# la riga 31926 e' passata da `backend` a `Service` conservando
# `agenda-medica-backend.service`, e 46 secondi dopo il backend — che non
# trovava piu' la propria label — ne ha allocata un'altra sulla stessa unit.
assert_single "identita-allocazione-porta" 'pub fn classify_port_claim' \
  'crates/nexus-tool-kit/src/ports.rs' crates
# La forma SQL che lo produceva non deve poter rientrare da nessuna parte: e' un
# UPDATE dell'identita' chiavato sulla porta. I commenti che la citano come
# difetto storico restano leciti (`grep` sulle sole righe non di commento).
ruba_identita="$(grep -rn --include='*.rs' --exclude-dir=target \
  -E 'ON CONFLICT \(port\) DO UPDATE' crates 2>/dev/null \
  | grep -vE ':[[:space:]]*(///|//|\*)' || true)"
if [[ -n "$ruba_identita" ]]; then
  echo "!! identita-allocazione-porta: un UPSERT chiavato sulla PORTA riscrive l'identita'" >&2
  echo "   di un'allocazione altrui (regola L, ADR 0042). Classifica con" >&2
  echo "   nexus_tool_kit::ports::classify_port_claim e non scrivere su DiUnAltro:" >&2
  printf '%s\n' "$ruba_identita" >&2
  fail=1
else
  echo "OK identita-allocazione-porta: nessun upsert chiavato sulla porta"
fi

# -- vocabolario-booleano-settings (2026-08-07) -------------------------------
#
# Il vocabolario che decide se una setting e' accesa o spenta vive in UN posto.
# Prima girava in cinque copie su quattro file, con DUE semantiche opposte: una
# allowlist (`true|1|yes|on`) e una denylist (`false|0|no`). Su `off` — la
# simmetrica di `on`, che nessuna delle due elencava — davano risposte
# CONTRARIE, e le chiavi coinvolte erano interruttori di worker: chi avesse
# scritto `off` per spegnere `optimizer_auto_promote` se la sarebbe ritrovata
# accesa. Nessun test poteva vederlo, perche' ogni copia era coerente con se
# stessa.
assert_single "vocabolario-booleano-settings" 'pub fn parse_setting_bool' 'crates/nexus-auth/src/lib.rs' crates
vocabolario_bool_sparso="$(grep -rIn -E '"true"[[:space:]]*\|[[:space:]]*"1"[[:space:]]*\|[[:space:]]*"yes"' --include='*.rs' crates/ 2>/dev/null | grep -v '^crates/nexus-auth/src/lib.rs:' || true)"
if [[ -n "$vocabolario_bool_sparso" ]]; then
  echo "!! vocabolario-booleano-settings: il vocabolario e' ricopiato fuori dal punto unico:" >&2
  echo "$vocabolario_bool_sparso" >&2
  echo "   Usare nexus_auth::parse_setting_bool (o get_bool_setting/_or)." >&2
  fail=1
else
  echo "OK vocabolario-booleano-settings: un solo vocabolario acceso/spento"
fi

# -- nome-agente-snake-to-pascal (2026-08-07) --------------------------------
#
# La conversione snake->Pascal dei nomi agente vive accanto all'enum di cui
# riconosce la grafia: le sigle (SRE, API, ML, QA, UI, ETL, PRManager, GitHub)
# NON sono una lista arbitraria, sono le varianti di AgentType. Separarle
# dall'enum e' cio' che aveva permesso a due copie di divergere — una con otto
# sigle, una con una sola — mentre il commento dichiarava «logica identica».
# Chi usava la povera otteneva Custom("SreEngineer") al posto di SREEngineer,
# senza che niente fallisse.
# Il pattern cerca il CORPO, non la firma: le viste locali che DELEGANO hanno la
# stessa firma e sono legittime — e' la reimplementazione che va fermata.
assert_single "nome-agente-snake-to-pascal" 'let mut capitalize_next = true;' 'crates/nexus-orchestrator/src/agent_types.rs' crates
sigle_sparse="$(grep -rIn -F '.replace("Sre", "SRE")' --include='*.rs' crates/ 2>/dev/null | grep -v '^crates/nexus-orchestrator/src/agent_types.rs:' || true)"
if [[ -n "$sigle_sparse" ]]; then
  echo "!! nome-agente-snake-to-pascal: le sigle dell'enum sono riallineate fuori dal punto unico:" >&2
  echo "$sigle_sparse" >&2
  fail=1
else
  echo "OK nome-agente-snake-to-pascal: le sigle restano accanto all'enum"
fi

# -- enum-dichiarato-e-accettato (2026-08-07) ---------------------------------
#
# Un `enum` nello schema di un tool E' un contratto: promette al modello che
# quei valori sono accettati. Se l'esecutore ne rifiuta uno, il modello sbaglia
# SEGUENDO le regole — e l'errore che riceve lo accusa di un errore che non ha
# commesso.
#
# MISURATO il 07/08/2026: lo schema di `nexus_verify_change` dichiarava
# `["quick","full","typecheck","build","lint","test"]`, il profilo di verifica
# del progetto aveva `lint-frontend` / `typecheck-backend` / `build-frontend`,
# e un agente che chiedeva `lint` — uno dei valori PROMESSI — otteneva
# `invalid_scope`. Il profilo e' inferito per progetto: un enum statico non
# poteva che mentire.
#
# Il rimedio strutturale c'era gia' nel repo (l'enum `kind` di
# dispatch_subagent, generato a runtime dal registry DB): questo guard chiede
# che i tool i cui valori dipendono dal PROGETTO passino di li'.
verify_scope_statico="$(grep -n '"scope"' apps/../crates/nexus-agent-tools/src/tool_schema.rs 2>/dev/null | grep -E '"(typecheck|lint|build|test)"' || true)"
if [[ -n "$verify_scope_statico" ]]; then
  if ! grep -q 'apply_verify_scope_enum' crates/mcp-core/src/agent_turn_setup.rs 2>/dev/null; then
    echo "!! enum-dichiarato-e-accettato: lo scope di nexus_verify_change e' statico e nessuno lo rigenera dal profilo:" >&2
    echo "$verify_scope_statico" >&2
    echo "   Il profilo di verifica e' per-progetto: l'enum va generato a runtime (apply_verify_scope_enum)." >&2
    fail=1
  else
    echo "OK enum-dichiarato-e-accettato: lo scope viene rigenerato dal profilo del progetto"
  fi
fi
# Un valore che l'esecutore rifiuta perche' NON IMPLEMENTATO non deve stare
# nell'enum: promette una capacita' che non esiste. MISURATO il 07/08/2026 con
# un censimento di tutti i 95 tool (33 enum): `knowledge_import_graph.format`
# dichiarava `mermaid` e `dot` mentre l'handler rispondeva «non supportato in
# questa versione ... non ancora portato». Due difetti su 33 enum, entrambi
# della stessa forma — lo schema promette, l'esecutore rifiuta.
importatore="crates/nexus-agent-tools/src/knowledge.rs"
schema_tool="crates/nexus-agent-tools/src/tool_schema.rs"
if [[ -f "$importatore" && -f "$schema_tool" ]]; then
  if grep -q 'formato .* non supportato in questa versione' "$importatore" 2>/dev/null; then
    if grep -q '"format": {"type": "string", "enum": \["json", "mermaid"' "$schema_tool" 2>/dev/null; then
      echo "!! enum-dichiarato-e-accettato: knowledge_import_graph promette mermaid/dot che l'handler rifiuta" >&2
      fail=1
    else
      echo "OK enum-dichiarato-e-accettato: l'importatore grafi non promette formati che rifiuta"
    fi
  fi
fi

# L'esecutore che rifiuta un valore DEVE elencare quelli accettati: senza,
# l'errore non e' rimediabile nemmeno quando lo dichiara.
for rifiuto in $(grep -rln '"invalid_scope"' --include='*.rs' crates/ 2>/dev/null); do
  if ! grep -q 'available_steps' "$rifiuto"; then
    echo "!! enum-dichiarato-e-accettato: $rifiuto rifiuta un valore senza elencare gli accettati" >&2
    fail=1
  fi
done

# ── riassunto di un run: una domanda, un punto unico ─────────────────────────
# Due presidi.
#
# 1. Nessuno si prende il `summary` di un finalizzatore per conto proprio per
#    farne il riassunto di un run. E' il modo in cui il difetto era nato: il run
#    principale aveva il suo `declared_outcome -> get("summary")` scritto a mano
#    e copriva UN solo finalizzatore su quattro; la chiusura del sub-run non
#    aveva nulla. Chi ne aggiungesse un terzo coprirebbe il proprio caso e
#    lascerebbe scoperti gli altri, di nuovo in silenzio.
# 2. I DUE finalizzatori delegano davvero. Un `unwrap_or_default()` sul solo
#    `final_answer` in una chiusura significa che quel percorso torna a scrivere
#    la stringa vuota quando la figura chiude con la sola dichiarazione — cioe'
#    il comportamento di prima, su una strada sola: 30 riassunti vuoti su 148
#    sub-run, tutti con il campo `summary` compilato (MISURATO 08/08/2026).
#
# PERIMETRO: `crates/mcp-core/`, cioe' dove il riassunto di un run si COMPONE
# alla chiusura. I nodi di `nexus-agent-graph/src/nodes/` restano fuori di
# proposito: li' il summary dichiarato viene letto per SCRIVERE `state.result`
# (chiusura d'autorita' su done ripetuto, meta-reasoner DeclareBlocked,
# enforcement del panel), cioe' per alimentare il testo libero che il punto
# unico poi preferisce. Sono a MONTE della domanda, non una seconda risposta, e
# un perimetro largo li segnalerebbe come regressioni facendo disinnestare la
# guard al primo che ci sbatte contro.
riassunto_hits="$(grep -rn "declared_outcome\|advisory_verdict\|review_verdict\|debate_position" \
  --include='*.rs' crates/mcp-core/ 2>/dev/null \
  | grep -E 'get\("summary"\)' \
  | grep -vE '^[^:]+:[0-9]+:\s*(//|\*)' \
  || true)"
for chiusura in crates/mcp-core/src/agent_tools/subagent_native.rs \
                crates/mcp-core/src/chat_messages/agent_run.rs; do
  if [[ -f "$chiusura" ]] && ! grep -q 'riassunto()' "$chiusura"; then
    riassunto_hits="${riassunto_hits}
$chiusura: finalizzatore che NON delega a NativeRunOutcome::riassunto()"
  fi
done
if [[ -n "${riassunto_hits// /}" ]]; then
  echo "!! riassunto-del-run: il riassunto di un run e' deciso fuori dal punto unico:" >&2
  printf '%s\n' "$riassunto_hits" >&2
  echo "   La domanda 'qual e' il riassunto di questo run?' vive in" >&2
  echo "   decisions/run_summary.rs (riassunto_del_run); le fonti nascono dal" >&2
  echo "   produttore NativeRunOutcome::fonti_riassunto, mai ricomposte al call" >&2
  echo "   site. Il campo 'summary' e' OBBLIGATORIO in tutti e quattro i tool:" >&2
  echo "   una figura che chiude con la sola dichiarazione ha comunque parlato." >&2
  fail=1
else
  echo "OK riassunto-del-run: la domanda ha un punto unico e i due finalizzatori vi delegano"
fi
# -- vita-processo (2026-08-08) ----------------------------------------------
#
# «Questo processo registrato e' vivo?» ha UN criterio. Prima ne aveva tre, e
# sbagliavano in direzioni opposte: `process_alive` da solo diceva VIVO su un pid
# riciclato (l'08/08 il pidfile dello stack dev elencava nove processi morti da
# ore, e dev-start.ps1 si rifiutava di ripartire); `process_alive &&
# pid_identity_confirmed` diceva MORTO ogni volta che l'identita' non era
# CONFERMABILE, e i consumatori che persistono scrivevano quel non-verdetto in DB
# come 'stopped'/'failed'.
#
# Il criterio ha tre esiti perche' i casi sono tre: `e_vivo()` e
# `autorizza_a_dichiararlo_morto()` non sono l'uno la negazione dell'altro, e un
# `bool` non poteva esprimerlo.
assert_single "vita-processo" 'pub\(crate\) enum StatoProcesso' \
  'crates/mcp-core/src/process_liveness.rs' crates
assert_single "vita-processo-criterio" 'pub\(crate\) fn classifica\(' \
  'crates/mcp-core/src/process_liveness.rs' crates
# «Questo SERVIZIO e' vivo?» e' un'ALTRA domanda, e ha il suo punto unico: il pid
# registrato e' la shell, il server e' un suo discendente che le sopravvive
# (misurato su gestione-corsi il 07/08, pid 20728 morto e porta 34859 in ascolto
# dal pid 3860). Delegano il pannello Servizi e l'observer.
assert_single "vita-servizio" 'pub\(crate\) fn classifica_servizio' \
  'crates/mcp-core/src/project_workspace/service_liveness.rs' crates
# E la RACCOLTA delle due prove sta con il criterio: e' la meta' della domanda che
# si sbaglia per omissione. Chi raccoglie da se' le porte allocate finisce per
# porre la domanda a meta' — o per non porla affatto, che e' cio' che il
# task_watchdog faceva con `process_alive` grezzo mentre il servizio rispondeva.
assert_single "prove-vita-servizio" 'struct ProveDiVita' \
  'crates/mcp-core/src/project_workspace/service_liveness.rs' crates
assert_single "porte-allocate-per-label" 'fn porte_allocate_per_label' \
  'crates/mcp-core/src/project_workspace/service_liveness.rs' crates
# Ogni punto che scrive la morte di un servizio come VERDETTO deve aver posto la
# domanda del servizio, non quella del processo. MISURATO l'08/08/2026: il
# pannello delegava gia', il watchdog no — e bastava lui a scrivere `failed` su
# un servizio che rispondeva HTTP 200 sulla porta allocata alla sua label.
#
# Il criterio e' il FILE che scrive, non la singola query. Un pattern SQL piu'
# stretto sarebbe inerte: le stringhe sono spezzate su piu' righe con `\`, e
# grep, che lavora per riga, vedrebbe solo `UPDATE agent_processes \` — mancando
# proprio il watchdog, cioe' l'unico punto in cui il difetto e' stato misurato
# (provato prima di scrivere questo guard).
#
# Chi scrive su `agent_processes` o interroga il punto unico, o compare qui sotto
# con il motivo: sono i punti che scrivono una CONSEGUENZA e non un verdetto —
# hanno appena ucciso i processi loro stessi, o non toccano lo stato di vita.
morte_servizio_esenti=(
  # stop esplicito: uccide i pid e poi ne registra l'esito, non deduce nulla
  'crates/mcp-core/src/project_workspace/service_manager.rs'
  # marca solo `resume_dispatched_at`: non scrive stato di vita
  'crates/mcp-core/src/process_resume.rs'
)
morte_servizio_muta=""
while IFS= read -r f; do
  [[ -z "$f" ]] && continue
  esente=""
  for e in "${morte_servizio_esenti[@]}"; do [[ "$f" == "$e" ]] && esente=1; done
  [[ -n "$esente" ]] && continue
  grep -q 'service_liveness' "$f" || morte_servizio_muta+="  $f"$'\n'
done <<< "$(grep -rl --include='*.rs' --exclude-dir=target \
  'UPDATE agent_processes' crates 2>/dev/null || true)"
if [[ -n "$morte_servizio_muta" ]]; then
  echo "!! morte-servizio: si scrive lo stato terminale di un servizio senza" >&2
  echo "   interrogare il punto unico service_liveness (il pid registrato e' la" >&2
  echo "   shell: un servizio vivo verrebbe scritto 'failed'):" >&2
  printf '%s' "$morte_servizio_muta" >&2
  fail=1
else
  echo "OK morte-servizio: chi scrive la morte di un servizio interroga il punto unico"
fi
# Il predicato vecchio non deve poter rientrare da nessuna parte: era il canale a
# due valori da cui nascevano entrambe le direzioni del difetto.
identita_bool="$(grep -rn --include='*.rs' --exclude-dir=target \
  -E 'fn pid_identity_(ok|confirmed)' crates 2>/dev/null || true)"
if [[ -n "$identita_bool" ]]; then
  echo "!! vita-processo: l'identita' del pid torna a essere un booleano" >&2
  echo "   (l'ignoto degraderebbe di nuovo a 'morto'). Usare" >&2
  echo "   mcp_core::process_liveness::stato_del_pid / stato_da_riga:" >&2
  printf '%s\n' "$identita_bool" >&2
  fail=1
else
  echo "OK vita-processo: nessun predicato booleano d'identita' del pid"
fi
# Gemello PowerShell: l'istante d'avvio E' il discriminante d'identita', e ha un
# solo lettore. Un altro script che se lo rilegga per conto proprio e' un secondo
# criterio, che divergera' da questo come i tre di prima.
avvio_ps="$(grep -rln --include='*.ps1' -E '\.StartTime' deploy/ 2>/dev/null \
  | grep -v 'deploy/lib/nexus-liveness.ps1' || true)"
if [[ -n "$avvio_ps" ]]; then
  echo "!! vita-processo: l'istante d'avvio si rilegge fuori dal punto unico:" >&2
  printf '%s\n' "$avvio_ps" >&2
  echo "   Delegare a Get-NexusProcessLiveness (deploy/lib/nexus-liveness.ps1)." >&2
  fail=1
else
  echo "OK vita-processo: un solo lettore dell'istante d'avvio negli script"
fi
# E la guardia dello stack non torna a decidere dall'ESISTENZA DEL FILE: e' cio'
# che il 08/08/2026 ha bloccato il riavvio di uno stack gia' morto.
if grep -qE '^\s*if \(Test-Path \$PIDFILE\) \{\s*$' deploy/dev-start.ps1 \
  && ! grep -q 'Get-NexusStackLiveness' deploy/dev-start.ps1; then
  echo "!! vita-processo: dev-start.ps1 decide dal solo Test-Path del pidfile," >&2
  echo "   senza chiedere al sistema operativo chi e' ancora vivo." >&2
  fail=1
else
  echo "OK vita-processo: la guardia dello stack interroga il SO"
fi

# premesse-dei-gate — «il gate ha bocciato il codice, o non e' mai partito?»
#
# Due regressioni possibili, entrambe silenziose. (1) Un secondo posto in cui un
# gate dichiara di essersi fermato: divergerebbe dal primo, e il fail_text di
# lefthook tornerebbe ad accusare il codice per una causa che non conosce.
# (2) Un gate che smette di PRETENDERE la propria premessa: non e' un errore
# visibile — lo script resta valido e i test restano verdi — semplicemente
# fallisce piu' avanti col messaggio sbagliato, che era il difetto dell'08/08.
# La definizione, non la menzione: ancorata a inizio riga, cosi' il pattern non
# matcha se stesso ne' i commenti che nominano la funzione per spiegarla.
premesse_hits="$(grep -rlnE '^ *gate_stop_configurazione\(\)' scripts/ 2>/dev/null \
  | grep -v '^scripts/gate-premesse\.sh$' || true)"
for coppia in "scripts/precommit-cargo-check.sh:gate_pretende_database_url" \
              "scripts/precommit-turbo.sh:gate_pretende_turbo" \
              "scripts/verify.sh:gate_pretende_turbo" \
              "scripts/verify.sh:gate_pretende_node" \
              "scripts/verify.sh:gate_pretende_nextest"; do
  file="${coppia%%:*}"; pretesa="${coppia##*:}"
  if [[ -f "$file" ]] && ! grep -q "^ *${pretesa}\$" "$file"; then
    premesse_hits="${premesse_hits}
$file: non pretende piu' la propria premessa ($pretesa)"
  fi
done
if [[ -n "${premesse_hits// /}" ]]; then
  echo "!! premesse-dei-gate: un gate dichiara il proprio esito fuori dal punto unico:" >&2
  printf '%s\n' "$premesse_hits" >&2
  echo "   'gate non eseguito' e 'codice bocciato' sono due valori del codice" >&2
  echo "   d'uscita (78 vs 1), non due frasi: il punto unico e'" >&2
  echo "   scripts/gate-premesse.sh, e i gate vi delegano la pretesa." >&2
  fail=1
else
  echo "OK premesse-dei-gate: l'esito 'non eseguito' ha un punto unico e i gate lo pretendono"
fi

# versione-node — la versione di Node ha UN posto solo: .nvmrc.
#
# Il difetto che presidia e' gia' costato cinque settimane di CI cieca. Due
# workflow dichiaravano `node-version: "20"` scritto a mano, il locale girava su
# 24, e nessuno dei due posti diceva che erano due. Quando i test della web-ide
# sono diventati file .ts eseguiti da `node --test` (2026-07-03), il CI ha
# iniziato a morire nella PRIMA fase — e siccome il gate era fail-fast, clippy e
# nextest non sono piu' stati eseguiti fino all'08/08/2026.
#
# La regressione da impedire e' precisamente il ritorno del numero scritto a
# mano: `node-version:` con un valore, invece di `node-version-file: .nvmrc`.
# Un workflow che lo reintroducesse non fallirebbe — girerebbe, con una versione
# diversa da quella di tutti gli altri.
node_hits=""
if [[ ! -f .nvmrc ]]; then
  node_hits=".nvmrc assente: la versione di Node non ha piu' un punto unico"
fi
# La CHIAVE con un valore letterale, non la menzione: `^[^#]*` esclude le righe
# di commento (compresi quelli che spiegano questo guard), e il valore atteso
# dopo i due punti impedisce di matchare `node-version-file:`. Stessa disciplina
# del guard premesse-dei-gate: un pattern che matcha la propria spiegazione
# rende il guard inservibile al primo commento.
node_hardcoded="$(grep -rnE '^[^#]*[{[:space:]-]node-version:[[:space:]]*["'"'"'0-9]' \
  .github/workflows/ 2>/dev/null || true)"
if [[ -n "$node_hardcoded" ]]; then
  node_hits="${node_hits}
$node_hardcoded"
fi
if [[ -n "${node_hits// /}" ]]; then
  echo "!! versione-node: la versione di Node e' dichiarata fuori da .nvmrc:" >&2
  printf '%s\n' "$node_hits" >&2
  echo "   Usa 'node-version-file: .nvmrc' nei workflow. Il MINIMO tollerato sta" >&2
  echo "   in package.json (engines.node) e lo pretende gate_pretende_node." >&2
  fail=1
else
  echo "OK versione-node: .nvmrc e' l'unico posto in cui la versione e' scritta"
fi

# ── carico-per-fornitore (2026-08-08) ───────────────────────────────────────
# «Quante chiamate sono in volo verso questo fornitore?» e' UNA domanda, e prima
# non ne esisteva nessuna: il rischio non e' che la risposta diverga, e' che
# qualcuno si costruisca il proprio contatore accanto. Tre presidi.
#
# 1. Il conteggio e la coda vivono in un modulo solo. Un secondo registro
#    conterebbe un sottoinsieme delle chiamate e, credendo scarico un fornitore
#    che non lo e', rifarebbe esattamente il difetto dell'08/08.
assert_single "registro-carico" 'struct RegistroCarico' \
  'crates/mcp-core/src/provider_inflight.rs' crates
# 2. La GUARDIA e' l'unica forma che regge alla cancellazione del task (ogni
#    figura che scade). Un decremento esplicito scritto altrove non verrebbe
#    eseguito su quel percorso, e il fornitore resterebbe saturo per sempre
#    proprio dopo un'ondata di timeout.
assert_single "permesso-chiamata" 'struct PermessoChiamata' \
  'crates/mcp-core/src/provider_inflight.rs' crates
# 3. L'innesto sta DENTRO `NexusGatewayClient::complete`, che e' il confine da
#    cui passa ogni chiamata al modello di mcp-core. Spostarlo piu' in alto (nel
#    fan-out) misurerebbe una convocazione invece di un fornitore, e le chiamate
#    delle altre sessioni — che quella sera pesavano sugli stessi cinque —
#    tornerebbero invisibili.
if ! awk '
  /pub async fn complete\(&self, req: GwRequest\)/ { dentro = 1 }
  dentro && /governo_del_carico/ { trovato = 1; exit }
  dentro && /^    }/ { exit }
  END { exit !trovato }
' crates/mcp-core/src/nexus_gateway.rs 2>/dev/null; then
  echo "!! carico-per-fornitore: complete() non chiede piu' il permesso al registro." >&2
  echo "   Il conteggio deve stare sul CONFINE (ogni chiamata al modello passa di" >&2
  echo "   li'), non nel fan-out: altrimenti misura una convocazione, non il" >&2
  echo "   carico vero del fornitore." >&2
  fail=1
else
  echo "OK carico-per-fornitore: il confine col gateway chiede il permesso"
fi

# contatore-di-spesa — «quanto e' costato cio' che sto guardando?»
#
# Il contatore sotto la chat ha UN SOLO scrittore, `refreshSessionUsage`, che
# legge dal ledger. Non e' una preferenza di stile: MISURATO l'08/08/2026 su
# gestione-corsi, quel contatore mostrava «639 token - $2.14» su una sessione da
# 27.813.580 token e $2,6024, perche' QUATTRO produttori scrivevano lo stesso
# stato con quattro perimetri diversi (il ledger, i token del TURNO dall'evento
# SSE, i totali del turno singolo da ChatMessageAdded, il costo sommato dai
# metadata dei messaggi da ChatSessionCompacted).
#
# Il difetto non si vedeva perche' ogni singolo produttore, preso da solo, era
# plausibile. Quindi il guard non cerca un valore sbagliato: cerca la SECONDA
# penna. Chi vuole aggiornare il contatore chiama il refresh (o il suo innesco
# throttled), mai `setTokenUsage` per conto proprio.
scrittori="$(grep -rn 'setTokenUsage(' apps/web-ide --include='*.ts' --include='*.tsx' \
  --exclude-dir=node_modules --exclude-dir=.next 2>/dev/null || true)"
scrittori_fuori="$(printf '%s\n' "$scrittori" | grep -v '^apps/web-ide/lib/use-chat\.ts:' || true)"
# Dentro use-chat.ts le scritture ammesse sono solo tre: la dichiarazione dello
# stato, il ramo «noto» e il ramo «non disponibile» di refreshSessionUsage, piu'
# il reset a `in_attesa` di `clear`. Un `setTokenUsage` che scriva NUMERI presi
# da altro (un payload di evento, un accumulo `prev + ...`) e' la regressione.
scrittori_sospetti="$(printf '%s\n' "$scrittori" \
  | grep -E 'setTokenUsage\(\(prev|setTokenUsage\(\{ *total(Tokens|CostUsd)' || true)"
if [[ -n "${scrittori_fuori// /}" || -n "${scrittori_sospetti// /}" ]]; then
  echo "!! contatore-di-spesa: un secondo produttore scrive il contatore di chat:" >&2
  [[ -n "${scrittori_fuori// /}" ]] && printf '%s\n' "$scrittori_fuori" >&2
  [[ -n "${scrittori_sospetti// /}" ]] && printf '%s\n' "$scrittori_sospetti" >&2
  echo "   Il contatore ha un solo scrittore (refreshSessionUsage, che legge dal" >&2
  echo "   ledger). Gli eventi dicono QUANDO rileggere, mai QUANTO: usa" >&2
  echo "   richiediUsageFresco(). Vedi components/chat/token-usage-bar-logic.ts." >&2
  fail=1
else
  echo "OK contatore-di-spesa: il contatore di chat ha un solo scrittore"
fi

# E la fixture del confine wire non deve restare orfana: se sparisce, il test
# Rust non compila piu' (include_str!) ma quello TS fallirebbe a runtime con un
# errore di file mancante, che e' un modo confuso di dire «il contratto non e'
# piu' verificato».
for lato in "crates/mcp-core/src/billing.rs:corpo_session_usage" \
            "apps/web-ide/lib/api/session-usage-wire.ts:sessionUsageDalWire"; do
  file="${lato%%:*}"; fn="${lato##*:}"
  if [[ ! -f "$file" ]] || ! grep -q "$fn" "$file"; then
    echo "!! confine-wire-session-usage: manca $fn in $file" >&2
    echo "   I due lati del wire si verificano contro UNA fixture condivisa" >&2
    echo "   (apps/web-ide/lib/api/__wire__/session-usage.json): se un lato" >&2
    echo "   sparisce, il contratto non e' piu' misurato da nessuno." >&2
    fail=1
  fi
done
if [[ ! -f "apps/web-ide/lib/api/__wire__/session-usage.json" ]]; then
  echo "!! confine-wire-session-usage: fixture condivisa assente" >&2
  fail=1
else
  echo "OK confine-wire-session-usage: fixture condivisa e i suoi due lati"
fi

if [[ "$fail" -ne 0 ]]; then
  echo "!! check-single-source: regressione su un punto unico (regola L / ADR 0026)." >&2
  exit 1

fi
echo "OK check-single-source: nessuna regressione sui punti unici attivi."
