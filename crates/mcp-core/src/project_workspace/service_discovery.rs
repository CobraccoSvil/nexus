//! Rilevamento agentico dei servizi del progetto.
//!
//! PUNTO UNICO (regola L) della scoperta agentica usata da
//! `wizard::wizard_detect_services`. La fase di RILEVAMENTO passa da euristiche
//! testuali fisse a un agent LLM PRIMARIO; l'euristica resta come fallback.
//!
//! Confine architetturale: l'agent fa solo la COMPRENSIONE (quali servizi, con
//! quale comando, quante porte e con quali nomi-variabile). L'AZIONE critica
//! resta deterministica: l'allocazione porte
//! (`services::deterministic_project_port_for_key`) e la generazione unit
//! (`wizard::wizard_install_service`) NON cambiano. L'agent NON inventa numeri di
//! porta: dichiara solo i NOMI delle variabili (`port_vars`); qui il codice
//! alloca i numeri dal registro (regola I).
//!
//! Tutta la config e' DB-driven (regola G): `agent.service_discovery.*` (mig
//! 0361). Il modello e' risolto via purpose tier-only (`service_discovery`),
//! nessun nome modello hardcoded. Se l'agent non e' disponibile/valido la
//! funzione ritorna `None` e il chiamante usa l'euristica.

use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::OnceCell;
use uuid::Uuid;

use nexus_cache::TtlCache;

use crate::AppState;

/// Nomi esatti dei file di configurazione rilevanti per dedurre i servizi.
/// `find_files_named` cerca per nome esatto (niente glob); i `.csproj` sono
/// coperti indirettamente da `launchSettings.json`/`Dockerfile`.
const CONFIG_FILES: &[&str] = &[
    "package.json",
    "pnpm-lock.yaml",
    "docker-compose.yml",
    "docker-compose.yaml",
    "docker-compose.dev.yml",
    "docker-compose.dev.yaml",
    "Cargo.toml",
    "pyproject.toml",
    "requirements.txt",
    "launchSettings.json",
    "Dockerfile",
    "Dockerfile.dev",
    "vite.config.ts",
    "vite.config.js",
    "next.config.js",
    "next.config.mjs",
    "nuxt.config.ts",
    "astro.config.mjs",
    "Makefile",
    "README.md",
];

/// `kind` ammessi (devono combaciare con quelli gestiti da
/// `wizard_install_service`).
const VALID_KINDS: &[&str] = &[
    "npm", "pnpm", "dotnet", "cargo", "python", "shell", "static",
];

/// Profondita' massima di ricerca dei file di config (coerente con l'euristica).
const SCAN_DEPTH: usize = 6;

/// Servizio validato e mappato dal JSON dell'agent, PRIMA dell'allocazione porte.
/// Tipo intermedio: tiene `port_vars` (nomi) senza numeri, cosi' la validazione
/// e' una funzione pura testabile e l'allocazione resta separata.
#[derive(Debug, Clone, PartialEq)]
struct MappedService {
    short: String,
    label: String,
    kind: String,
    command: String,
    args: Vec<String>,
    cwd: String,
    port_vars: Vec<String>,
    needs_install: bool,
    pkg_manager: Value,
}

/// Punto di ingresso: prova a rilevare i servizi via agent. Ritorna:
/// - `Some(vec)` con i suggerimenti (formato identico all'euristica) se l'agent
///   produce almeno un servizio valido;
/// - `None` se disabilitato, agent non disponibile, output non valido o 0
///   servizi -> il chiamante usa l'euristica come fallback.
///
/// I suggerimenti ritornati hanno `existing=false`; la marcatura "gia'
/// installato" e' applicata dal chiamante (stato volatile, non cachato).
pub(super) async fn discover_services_agentic(
    state: &AppState,
    project_id: &Uuid,
    root: &str,
    project_name: &str,
    slug: &str,
) -> Option<Vec<Value>> {
    let db = &state.db;

    if !load_bool(db, "agent.service_discovery.enabled", true).await {
        return None;
    }

    // 1. Raccolta contenuti dei file di config (con budget).
    let max_chars = load_u64(
        db,
        "agent.service_discovery.max_config_bytes",
        60_000,
        1_000,
    )
    .await as usize;
    let payload = collect_config_payload(root, max_chars).await;
    if payload.trim().is_empty() {
        // Nessun file di config: lascia decidere all'euristica.
        return None;
    }

    // 2. Cache (chiave = project + hash dei contenuti): evita una chiamata LLM a
    //    ogni poll del pannello (60s) quando i file non cambiano.
    let ttl = load_u64(db, "agent.service_discovery.cache_ttl_seconds", 600, 5).await;
    let cache = shared_cache(ttl).await;
    let cache_key = format!("{}:{:016x}", project_id, fnv1a(&payload));
    if let Some(hit) = cache.get(&cache_key) {
        tracing::debug!("service_discovery: cache hit per progetto {project_id}");
        return Some(hit);
    }

    // 3. Risoluzione modello dal purpose (tier-only, regola G).
    let (provider, model) =
        match crate::internal_routing::resolve_purpose_model(state, "service_discovery")
            .await
            .into_model("service_discovery")
        {
            Ok(pm) => pm,
            Err(e) => {
                tracing::warn!("service_discovery: purpose non risolto ({e}); fallback euristica");
                return None;
            }
        };

    // 4. Prompt (template DB) con placeholder sostituiti.
    let template = crate::prompt_templates::get_template_or_default(
        db,
        &state.template_cache,
        "agent.service_discovery",
    )
    .await;
    if template.trim().is_empty() {
        tracing::warn!("service_discovery: template prompt vuoto; fallback euristica");
        return None;
    }
    let system = template
        .replace("{{project_name}}", project_name)
        .replace("{{project_root}}", root)
        .replace("{{config_files_payload}}", &payload);
    let messages = json!([{
        "role": "user",
        "content": "Analizza i file di configurazione forniti e restituisci esclusivamente il JSON dei servizi."
    }])
    .to_string();
    let max_tokens = load_u64(db, "agent.service_discovery.max_tokens", 2000, 256).await as u32;

    // 5. Completion one-shot (nessun tool).
    let raw = match state
        .orchestrator
        .neural
        .generate_agent_turn(&provider, &model, &messages, "[]", max_tokens, &system)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                "service_discovery: completion fallita ({provider}/{model}): {e}; fallback euristica"
            );
            return None;
        }
    };
    let content = raw.get("content").and_then(Value::as_str).unwrap_or("");

    // 6. Parsing fail-loud (punto unico llm_json).
    let parsed = match crate::llm_json::parse_llm_json(content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("service_discovery: output non JSON ({e}); fallback euristica");
            return None;
        }
    };
    let services = parsed
        .get("services")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // 7. Validazione + mappatura + allocazione porte deterministica.
    let mut out: Vec<Value> = Vec::new();
    let mut seen_short: std::collections::HashSet<String> = std::collections::HashSet::new();
    for svc in &services {
        let Some(m) = validate_and_map_service(svc) else {
            continue;
        };
        if !seen_short.insert(m.short.clone()) {
            continue; // short duplicato: tieni il primo
        }
        let mut env = serde_json::Map::new();
        for var in &m.port_vars {
            // L'agent dichiara i NOMI; il numero lo alloca il registro (regola I).
            let key = if var == "PORT" {
                m.short.clone()
            } else {
                format!("{}-{}", m.short, var.to_lowercase())
            };
            let port = super::services::deterministic_project_port_for_key(
                project_id,
                &key,
                &state.port_registry,
            )
            .await;
            env.insert(var.clone(), Value::String(port.to_string()));
        }
        out.push(json!({
            "short":         m.short,
            "unit":          format!("{}-{}.service", slug, m.short),
            "label":         m.label,
            "kind":          m.kind,
            "command":       m.command,
            "args":          m.args,
            "cwd":           m.cwd,
            "env":           Value::Object(env),
            "existing":      false,
            "needs_install": m.needs_install,
            "pkg_manager":   m.pkg_manager,
        }));
    }

    if out.is_empty() {
        tracing::warn!("service_discovery: 0 servizi validi dall'agent; fallback euristica");
        return None;
    }

    cache.insert(cache_key, out.clone());
    tracing::info!(
        "service_discovery: {} servizi rilevati via agent ({provider}/{model}) per progetto {project_id}",
        out.len()
    );
    Some(out)
}

/// Estrae i BASENAME dei file-entrypoint (sorgenti) citati in una command line.
/// Parte PURA e testabile della signature di esecuzione: i token che terminano
/// con una estensione sorgente nota sono il "cosa esegue" il servizio,
/// indipendentemente dal runner (nodemon, tsx, node, ts-node). Si usa il basename
/// (non il path relativo) perche' l'agent puo' allucinare cwd diversi per la
/// stessa variante (es. cwd root vs root/backend con lo stesso `src/app.ts`).
pub(super) fn entrypoint_files(cmdline: &str) -> std::collections::BTreeSet<String> {
    const EXTS: [&str; 7] = [".ts", ".tsx", ".js", ".mjs", ".cjs", ".jsx", ".py"];
    let mut sig = std::collections::BTreeSet::new();
    for tok in cmdline.split_whitespace() {
        let t = tok.trim_matches(|c| c == '"' || c == '\'');
        if EXTS.iter().any(|e| t.ends_with(e)) {
            let base = t.rsplit('/').next().unwrap_or(t);
            sig.insert(base.to_string());
        }
    }
    sig
}

/// Signature di esecuzione di un candidato: l'insieme dei file-entrypoint
/// (basename) che il servizio avvia. Per i candidati che invocano uno script del
/// package manager (`pnpm/npm/yarn run <script>`) ESPANDE lo script leggendo il
/// package.json in `cwd`, cosi' il confronto e' sul PROCESSO reale e non sulla
/// forma di invocazione: "pnpm run dev:backend" e l'equivalente diretto
/// "nodemon ... src/app.ts" producono la stessa signature. Best-effort: se il
/// package.json non e' leggibile/parsabile usa la cmdline grezza. Set vuoto (es.
/// "vite", "docker compose up") = il servizio NON partecipa alla dedup
/// per-entrypoint.
pub(super) async fn execution_signature(
    command: &str,
    args: &[String],
    cwd: &str,
) -> std::collections::BTreeSet<String> {
    let mut cmdline = format!("{} {}", command, args.join(" "));
    let base = command.rsplit('/').next().unwrap_or(command);
    if matches!(base, "pnpm" | "npm" | "yarn" | "npx") {
        // Nome script: il primo arg che non sia "run" ne' un flag.
        let script = args
            .iter()
            .find(|a| a.as_str() != "run" && !a.starts_with('-'))
            .cloned();
        if let Some(name) = script {
            if let Ok(content) =
                tokio::fs::read_to_string(format!("{cwd}/package.json")).await
            {
                if let Ok(pkg) = serde_json::from_str::<Value>(&content) {
                    if let Some(v) = pkg
                        .get("scripts")
                        .and_then(|s| s.get(&name))
                        .and_then(Value::as_str)
                    {
                        cmdline = v.to_string();
                    }
                }
            }
        }
    }
    entrypoint_files(&cmdline)
}

/// Signature di esecuzione di un suggerimento (Value con campi command/args/cwd).
async fn suggestion_signature(s: &Value) -> std::collections::BTreeSet<String> {
    let command = s.get("command").and_then(Value::as_str).unwrap_or("");
    let cwd = s.get("cwd").and_then(Value::as_str).unwrap_or("");
    let args: Vec<String> = s
        .get("args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    execution_signature(command, &args, cwd).await
}

/// Scarta i suggerimenti che sono VARIANTI DI AVVIO di un servizio gia' gestito.
/// PUNTO UNICO (regola L) della de-duplicazione delle varianti, applicato DOPO
/// `mark_existing_services` (serve il flag `existing`). Criterio robusto: un
/// candidato NON gestito (`existing=false`) il cui entrypoint coincide con quello
/// di un candidato GESTITO (`existing=true`) e' lo stesso servizio in un'altra
/// forma di invocazione (es. "nodemon src/app.ts" vs lo script "pnpm run
/// dev:backend" che espande allo stesso `app.ts`). I modelli (es. mistral-large)
/// generano queste varianti nonostante il prompt (mig 0454), talvolta con `cwd`
/// allucinati: confrontare i basename degli entrypoint e gating sul flag
/// `existing` evita falsi collassi tra servizi legittimi (che sarebbero entrambi
/// gestiti o entrambi non gestiti, e comunque con entrypoint diversi).
pub(super) async fn drop_managed_variants(suggestions: &mut Vec<Value>) {
    // Entrypoint dei servizi GIA' gestiti.
    let mut managed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for s in suggestions.iter() {
        if s.get("existing").and_then(Value::as_bool) == Some(true) {
            managed.extend(suggestion_signature(s).await);
        }
    }
    if managed.is_empty() {
        return;
    }
    let mut kept: Vec<Value> = Vec::with_capacity(suggestions.len());
    for s in std::mem::take(suggestions) {
        if s.get("existing").and_then(Value::as_bool) != Some(true) {
            let sig = suggestion_signature(&s).await;
            if !sig.is_empty() && sig.iter().any(|e| managed.contains(e)) {
                tracing::info!(
                    "wizard: variante non gestita '{}' scartata (entrypoint {:?} \
                     gia' coperto da un servizio gestito)",
                    s.get("short").and_then(|v| v.as_str()).unwrap_or("?"),
                    sig
                );
                continue;
            }
        }
        kept.push(s);
    }
    *suggestions = kept;
}

/// Cache condivisa in-process. TTL letto dal DB alla PRIMA inizializzazione
/// (cambio del setting a runtime richiede restart). Punto unico cache TTL:
/// `nexus_cache::TtlCache` (regola L).
async fn shared_cache(ttl_secs: u64) -> &'static TtlCache<String, Vec<Value>> {
    static CACHE: OnceCell<TtlCache<String, Vec<Value>>> = OnceCell::const_new();
    CACHE
        .get_or_init(|| async move { TtlCache::new(Duration::from_secs(ttl_secs)) })
        .await
}

/// Valida un singolo servizio dal JSON dell'agent e lo mappa al tipo intermedio.
/// Funzione PURA (testabile): nessuna allocazione porte, nessun IO. Scarta
/// servizi con campi obbligatori mancanti, `kind` fuori whitelist o comando
/// no-op (riusa `wizard::FORBIDDEN_NOOP`).
fn validate_and_map_service(svc: &Value) -> Option<MappedService> {
    let short = svc.get("short").and_then(Value::as_str)?.trim().to_string();
    if short.is_empty() {
        return None;
    }
    let kind = svc.get("kind").and_then(Value::as_str)?.trim().to_string();
    if !VALID_KINDS.contains(&kind.as_str()) {
        return None;
    }
    let command = svc
        .get("command")
        .and_then(Value::as_str)?
        .trim()
        .to_string();
    if command.is_empty() {
        return None;
    }
    // Rifiuta comandi no-op (stessa lista di wizard_install_service, regola L).
    let basename = std::path::Path::new(&command)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(command.as_str());
    if super::wizard::FORBIDDEN_NOOP.contains(&basename) {
        return None;
    }
    let cwd = svc.get("cwd").and_then(Value::as_str)?.trim().to_string();
    if cwd.is_empty() {
        return None;
    }
    let label = svc
        .get("label")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(short.as_str())
        .to_string();
    let args: Vec<String> = svc
        .get("args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    // port_vars: solo NOMI (scarta numeri o vuoti: l'agent non deve dare porte).
    let port_vars: Vec<String> = svc
        .get("port_vars")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty() && !s.chars().all(|c| c.is_ascii_digit()))
                .collect()
        })
        .unwrap_or_default();
    let needs_install = svc
        .get("needs_install")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let pkg_manager = match svc.get("pkg_manager") {
        Some(Value::String(s)) if !s.trim().is_empty() => Value::String(s.clone()),
        _ => Value::Null,
    };

    Some(MappedService {
        short,
        label,
        kind,
        command,
        args,
        cwd,
        port_vars,
        needs_install,
        pkg_manager,
    })
}

/// Concatena i contenuti dei file di config (path relativo + corpo) entro un
/// budget di caratteri. Tronca per-file e globalmente su confine di carattere
/// (niente panic UTF-8). Resta dentro `root` (regola E).
async fn collect_config_payload(root: &str, max_chars: usize) -> String {
    let per_file_cap = (max_chars / 4).max(2_000);
    let mut out = String::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    'outer: for name in CONFIG_FILES {
        for p in super::wizard::find_files_named(root, name, SCAN_DEPTH).await {
            if out.chars().count() >= max_chars {
                break 'outer;
            }
            if !seen.insert(p.clone()) {
                continue;
            }
            let Ok(content) = tokio::fs::read_to_string(&p).await else {
                continue;
            };
            let rel = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .trim_start_matches('/')
                .to_string();
            let body: String = content.chars().take(per_file_cap).collect();
            let trunc = if content.chars().count() > per_file_cap {
                "\n...(troncato)"
            } else {
                ""
            };
            out.push_str(&format!("### {rel}\n{body}{trunc}\n\n"));
        }
    }

    if out.chars().count() > max_chars {
        out = out.chars().take(max_chars).collect();
    }
    out
}

/// Hash FNV-1a 64-bit del payload, per la chiave di cache (cambia coi contenuti).
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

async fn load_bool(db: &sqlx::PgPool, key: &str, default: bool) -> bool {
    crate::settings::get_setting(db, key)
        .await
        .ok()
        .flatten()
        .map(|v| {
            !matches!(
                v.trim().to_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(default)
}

async fn load_u64(db: &sqlx::PgPool, key: &str, default: u64, min: u64) -> u64 {
    crate::settings::get_setting(db, key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
        .max(min)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn entrypoint_files_usa_il_basename() {
        // Il runner diretto espone l'entrypoint reale (basename).
        let sig = entrypoint_files("nodemon --watch src/app.ts --exec tsx src/app.ts");
        assert!(sig.contains("app.ts"));
        assert_eq!(sig.len(), 1);
        // Lo stesso processo via cross-env mantiene la stessa signature -> la dedup
        // riconosce "pnpm run dev:backend" (espanso) e "nodemon ... src/app.ts".
        let sig2 =
            entrypoint_files("cross-env PORT=21976 nodemon --watch src/app.ts --exec tsx src/app.ts");
        assert_eq!(sig, sig2);
        // cwd allucinato (backend/src/app.ts): il basename collassa lo stesso, cosi'
        // la variante con cwd inventato resta riconoscibile come duplicato.
        assert_eq!(entrypoint_files("nodemon backend/src/app.ts"), sig);
        // vite/docker non hanno entrypoint sorgente -> signature vuota (no dedup).
        assert!(entrypoint_files("vite --port 21954").is_empty());
        assert!(entrypoint_files("docker compose up").is_empty());
    }

    fn valid_svc() -> Value {
        json!({
            "short": "frontend",
            "label": "pnpm dev (web)",
            "kind": "pnpm",
            "command": "pnpm",
            "args": ["run", "dev"],
            "cwd": "/home/user/proj/web",
            "port_vars": ["PORT"],
            "needs_install": true,
            "pkg_manager": "pnpm install"
        })
    }

    #[test]
    fn servizio_valido_mappato() {
        let m = validate_and_map_service(&valid_svc()).expect("servizio valido");
        assert_eq!(m.short, "frontend");
        assert_eq!(m.kind, "pnpm");
        assert_eq!(m.command, "pnpm");
        assert_eq!(m.args, vec!["run".to_string(), "dev".to_string()]);
        assert_eq!(m.port_vars, vec!["PORT".to_string()]);
        assert!(m.needs_install);
        assert_eq!(m.pkg_manager, Value::String("pnpm install".into()));
    }

    #[test]
    fn comando_no_op_scartato() {
        let mut s = valid_svc();
        s["command"] = json!("true");
        assert!(validate_and_map_service(&s).is_none());
        s["command"] = json!("/bin/sleep");
        assert!(validate_and_map_service(&s).is_none());
    }

    #[test]
    fn kind_fuori_whitelist_scartato() {
        let mut s = valid_svc();
        s["kind"] = json!("ruby");
        assert!(validate_and_map_service(&s).is_none());
    }

    #[test]
    fn campi_obbligatori_mancanti_scartati() {
        let mut s = valid_svc();
        s["command"] = json!("");
        assert!(validate_and_map_service(&s).is_none());

        let mut s = valid_svc();
        s["short"] = json!("   ");
        assert!(validate_and_map_service(&s).is_none());

        let mut s = valid_svc();
        s.as_object_mut().unwrap().remove("cwd");
        assert!(validate_and_map_service(&s).is_none());
    }

    #[test]
    fn port_vars_numerici_filtrati() {
        let mut s = valid_svc();
        // L'agent NON deve dare numeri: "8080" va scartato, "PORT_BACKEND" tenuto.
        s["port_vars"] = json!(["PORT_FRONTEND", "8080", "", "PORT_BACKEND"]);
        let m = validate_and_map_service(&s).expect("valido");
        assert_eq!(
            m.port_vars,
            vec!["PORT_FRONTEND".to_string(), "PORT_BACKEND".to_string()]
        );
    }

    #[test]
    fn pkg_manager_assente_diventa_null() {
        let mut s = valid_svc();
        s.as_object_mut().unwrap().remove("pkg_manager");
        let m = validate_and_map_service(&s).expect("valido");
        assert_eq!(m.pkg_manager, Value::Null);
    }

    #[test]
    fn label_assente_usa_short() {
        let mut s = valid_svc();
        s.as_object_mut().unwrap().remove("label");
        let m = validate_and_map_service(&s).expect("valido");
        assert_eq!(m.label, "frontend");
    }

    #[test]
    fn hash_stabile_e_sensibile() {
        assert_eq!(fnv1a("abc"), fnv1a("abc"));
        assert_ne!(fnv1a("abc"), fnv1a("abd"));
    }
}
