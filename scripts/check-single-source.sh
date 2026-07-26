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
# reale (nexus_test_schema::PROJECT_MIGRATOR, il migrator del set
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
    echo "     #[sqlx::test(migrator = \"nexus_test_schema::PROJECT_MIGRATOR\")]" >&2
    echo "   e semina le righe coi seeder (mcp_core::test_support::seed_*)." >&2
    fail=1
  else
    echo "OK schema-di-test: nessuna tabella del set project ricreata a mano"
  fi
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
stub_hits=""
for f in db/migrations/*.sql db/migrations/project/*.sql; do
  [[ -f "$f" ]] || continue
  case "$(basename "$f")" in 010[4-7]_*) continue ;; esac
  # corpo = tutto tranne righe vuote e commenti `--`
  body="$(sed 's/--.*//' "$f" | tr -d '[:space:]' | tr '[:upper:]' '[:lower:]')"
  [[ "$body" == "select1;" ]] && stub_hits+="$f"$'\n'
done
if [[ -n "$stub_hits" ]]; then
  echo "!! migrazione-stub: migrazione con corpo 'SELECT 1;' (nessuno schema dentro):" >&2
  printf '%s' "$stub_hits" | sed 's/^/     /' >&2
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
# Il pattern esclude le righe di commento (`^[^/]*`): le intestazioni citano
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
done < <(grep -rnE '^[^/]*eprintln!\("skip' "${crate_opportunistici[@]}" 2>/dev/null || true)

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

if [[ "$fail" -ne 0 ]]; then
  echo "!! check-single-source: regressione su un punto unico (regola L / ADR 0026)." >&2
  exit 1
fi
echo "OK check-single-source: nessuna regressione sui punti unici attivi."
