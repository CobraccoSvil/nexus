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
  local dirs=("$@"); [[ ${#dirs[@]} -eq 0 ]] && dirs=(crates brain apps packages)
  local hits bad=""
  hits="$(grep -rEl --include='*.rs' --include='*.py' --include='*.ts' --include='*.tsx' \
    --exclude-dir=target --exclude-dir=node_modules --exclude-dir=.next \
    --exclude-dir=.turbo --exclude-dir=dist --exclude-dir=__pycache__ \
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

# Wave 4 (capability: la fonte DATI e' gia' unica via ADR 0024, vista
# v_model_capabilities; qui si protegge il classificatore di scrittura).
assert_single "classify_capabilities" 'fn classify_capabilities' 'crates/mcp-core/src/model_catalog_sync.rs' crates
assert_single "infer_capabilities_from_name" 'fn infer_capabilities_from_name' 'crates/mcp-core/src/model_catalog_sync.rs' crates

# Wave 5 (registry default: statici nella migrazione 0325; parte dinamica unica):
assert_single "ensure_projects_base_root" 'fn ensure_projects_base_root' 'crates/nexus-types/src/lib.rs' crates

# Wave 6a + Residuo R1: get_db_url() e' il punto unico della DB URL. Niente
# default hardcoded "postgres://..." altrove (regola G). I 28 call site con
# psycopg2.connect convergono via questa URL.
assert_single "get_db_url" 'def get_db_url' 'brain/utils/db_pool.py' brain

# Wave 5 (perf): db_pool.connect() e' un ThreadedConnectionPool — psycopg2.connect
# diretto fuori dal punto unico riapre una connessione TCP per chiamata (baseline
# misurata: ~14 ms/lettura). Esclusi: brain/tests/** (fixture con DB effimeri) e
# postgres_checkpointer.py (pool asyncpg proprio di LangGraph). Per i DSN
# applicativi di progetto usare db_pool.connect_external.
psycopg2_connect_hits="$(grep -rEln --include='*.py' \
  --exclude-dir=__pycache__ \
  -e 'psycopg2\.connect\(' brain 2>/dev/null \
  | grep -v '^brain/tests/' \
  | grep -v 'postgres_checkpointer\.py' \
  | grep -v '^brain/utils/db_pool\.py' || true)"
if [[ -n "$psycopg2_connect_hits" ]]; then
  echo "!! single-source [psycopg2.connect]: connessione diretta fuori dal pool (brain/utils/db_pool.py):" >&2
  printf '  %s\n' $psycopg2_connect_hits >&2
  fail=1
else
  echo "OK single-source [psycopg2.connect]"
fi

# Wave 6b (cache TTL Python, paritetica a nexus-cache lato Rust):
assert_single "TtlCache python" 'class TtlCache' 'brain/utils/ttl_cache.py' brain

# Wave 6c (estrazione JSON da output LLM):
assert_single "extract_json_block" 'def extract_json_block' 'brain/utils/json_extract.py' brain

# Wave 6e (intent canonici di routing):
assert_single "ALLOWED_INTENTS" '^ALLOWED_INTENTS' 'brain/router/intents.py' brain

# Wave 8a (chunker testo, punto unico Python paritetico a rag/chunker.rs Rust):
assert_single "python chunk_text" '^def chunk_text' 'brain/utils/text_chunk.py' brain

# Wave 8b (error classifier testuale Rust, paritetico a brain error_handler):
assert_single "rust classify_text" 'pub fn classify_text' 'crates/mcp-core/src/provider_error_classifier.rs' crates

# Wave 8a:
# assert_single "python chunker" 'def _?chunk_text' 'brain/utils/text_chunk.py' brain

# Wave 4+5 (2026-06-11): punti unici del consolidamento E1-E6
assert_single "walk FS nexus_tools" 'pub fn walk_project_files' 'crates/mcp-core/src/nexus_tools/fs_scan.rs' crates
assert_single "catalog query Postgres" 'pub fn list_catalog_rows' 'crates/mcp-core/src/nexus_tools/db_helper.rs' crates
assert_single "registrazione progetto" 'pub async fn register_project_records' 'crates/mcp-core/src/nexus_tools/project_register_common.rs' crates
assert_single "endpoint MCP server condivisi" 'pub async fn list_servers_core' 'crates/nexus-mcp-client/src/server_endpoints.rs' crates
assert_single "coda generate provider OpenAI-compat" '^def build_generate_result' 'brain/providers/_response_parsers.py' brain

if [[ "$fail" -ne 0 ]]; then
  echo "!! check-single-source: regressione su un punto unico (regola L / ADR 0026)." >&2
  exit 1
fi
echo "OK check-single-source: nessuna regressione sui punti unici attivi."
