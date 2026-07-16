use crate::AppState;
use axum::{
    extract::{Query, State},
    Json,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;

// ── Types ──────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogEntry {
    pub provider: String,
    pub model: String,
    #[sqlx(default)]
    pub display_name: String,
    pub input_cost_per_million_tokens: f64,
    pub output_cost_per_million_tokens: f64,
    pub currency: String,
    /// `None` = tier ignoto: nessuna fonte si e' espressa (mig 0599/0608).
    /// Prima era `String` con `#[sqlx(default)]`, che rendeva un NULL
    /// indistinguibile da una stringa vuota — cioe' l'esatta ambiguita' che la
    /// 0599 ha eliminato dal DB.
    #[sqlx(default)]
    pub performance_tier: Option<String>,
    /// Chi ha stabilito il tier: `synced` (indice esterno) | `measured`
    /// (batteria) | `manual` (curatela) | `None` (fonte ignota: il valore c'e'
    /// ma e' un fossile, e chiunque puo' rimpiazzarlo). Senza questo campo
    /// l'admin vede un tier e non sa se fidarsi.
    #[sqlx(default)]
    pub tier_source: Option<String>,
    /// L'indice della classificazione esterna (Artificial Analysis via
    /// OpenRouter): il numero su cui il tier `synced` si fonda. `None` = il
    /// servizio non copre questo modello (43 su 116, il 16/07).
    #[sqlx(default)]
    pub agentic_index: Option<f64>,
    /// Stato della batteria: `qualified` | `unqualified` | `disqualified` |
    /// `probing` | `quarantined`. Col gate acceso solo i `qualified` non scaduti
    /// entrano nel routing agentico: e' la prima cosa da guardare quando un
    /// modello "non viene mai scelto".
    #[sqlx(default)]
    pub qualification_state: Option<String>,
    #[sqlx(default)]
    pub speed_tier: String,
    #[sqlx(default)]
    pub capabilities: Value,
    #[sqlx(default)]
    pub context_window: i32,
    #[sqlx(default)]
    pub supports_tool_use: bool,
    #[sqlx(default)]
    pub batch_discount_pct: i32,
    #[sqlx(default)]
    pub is_featured: bool,
    pub is_enabled: bool,
}

/// La `SELECT` che idrata [`ModelCatalogEntry`], con le colonne in UN posto solo
/// (regola L). `$coda` e' la parte variabile (`WHERE ... ORDER BY ...`) e deve
/// essere un LETTERALE: la query si compone a compile-time con `concat!`, quindi
/// non esiste il modo di interpolarci dentro un valore a runtime (niente
/// SQL injection possibile per costruzione, non per diligenza).
///
/// Perche' esiste: le query che elencavano le colonne a mano erano quattro, su
/// due file — `/api/models` (con e senza filtro provider) e
/// `/api/admin/provider-models` (idem). Aggiungere un campo alla struct
/// significava ricordarsi di tutte e quattro, e dimenticarne una NON e' un
/// errore che il compilatore veda: `#[sqlx(default)]` riempie il campo mancante
/// col default e la riga arriva silenziosamente sbagliata. E' la stessa forma
/// del difetto che ha tenuto la batteria in panic per un giorno
/// (`load_profiles` leggeva una colonna che la sua SELECT non chiedeva).
macro_rules! catalog_select {
    ($coda:literal) => {
        concat!(
            "SELECT provider, model, display_name, \
             input_cost_per_million_tokens::float8 AS input_cost_per_million_tokens, \
             output_cost_per_million_tokens::float8 AS output_cost_per_million_tokens, \
             currency, performance_tier, tier_source, \
             agentic_index::float8 AS agentic_index, qualification_state, speed_tier, \
             capabilities, context_window, supports_tool_use, batch_discount_pct, \
             is_featured, is_enabled \
             FROM ai_price_catalog ",
            $coda
        )
    };
}
pub(crate) use catalog_select;

#[derive(Debug, Deserialize)]
pub struct ModelsQuery {
    pub provider: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RoutingPreviewQuery {
    pub mode: Option<String>,
}

// La matrice di routing e' stata spostata in DB (tabella nexus_routing_matrix,
// migrazione 0101). Vedi crate::routing_matrix per il loader con cache 60s.

// ── Handlers ──────────────────────────────────────────────────────────────

/// GET /api/models[?provider=xxx]
pub async fn list_models(
    State(state): State<AppState>,
    Query(params): Query<ModelsQuery>,
) -> Json<Value> {
    let result: Result<Vec<ModelCatalogEntry>, _> = if let Some(ref provider) = params.provider {
        sqlx::query_as(catalog_select!(
            "WHERE provider = $1 AND is_enabled = TRUE \
             ORDER BY is_featured DESC, input_cost_per_million_tokens ASC"
        ))
        .bind(provider)
        .fetch_all(&state.db)
        .await
    } else {
        sqlx::query_as(catalog_select!(
            "WHERE is_enabled = TRUE \
             ORDER BY provider, is_featured DESC, input_cost_per_million_tokens ASC"
        ))
        .fetch_all(&state.db)
        .await
    };

    match result {
        Ok(models) => Json(json!({ "models": models })),
        Err(e) => Json(json!({ "error": e.to_string(), "models": Value::Array(vec![]) })),
    }
}

/// GET /api/models/routing-preview?mode=bilanciata
pub async fn routing_preview(
    State(state): State<AppState>,
    Query(params): Query<RoutingPreviewQuery>,
) -> Json<Value> {
    let mode = params.mode.as_deref().unwrap_or("bilanciata").to_string();
    let valid_modes = ["veloce", "economica", "bilanciata", "approfondita"];
    let mode = if valid_modes.contains(&mode.as_str()) {
        mode
    } else {
        "bilanciata".to_string()
    };

    // Legge la matrice da DB (cache 60s). Se non disponibile ritorna preview vuota
    // con error: il chiamante (admin UI) mostra il messaggio.
    let matrix_arc = match state.orchestrator.routing_matrix.current_async().await {
        Ok(m) => m,
        Err(e) => {
            return Json(json!({
                "mode": mode,
                "routing": [],
                "error": format!("routing_matrix non disponibile: {e}"),
            }));
        }
    };
    let entries: Vec<(String, String, String)> = matrix_arc
        .by_intent_mode
        .iter()
        .filter(|((_, m), _)| m == &mode)
        .map(|((intent, _mode), (provider, model))| {
            (intent.clone(), provider.clone(), model.clone())
        })
        .collect();

    // Fetch prices from DB for each model in the preview
    let mut routing = Vec::new();
    let mut total_cost = 0.0f64;
    let mut count = 0usize;

    for (intent, provider, model) in &entries {
        let price_row: Option<(f64, String)> = sqlx::query_as::<_, (f64, String)>(
            "SELECT input_cost_per_million_tokens, speed_tier FROM ai_price_catalog WHERE provider = $1 AND model = $2 LIMIT 1"
        )
        .bind(provider)
        .bind(model)
        .fetch_optional(&state.db)
        .await
        .unwrap_or(None);

        let (input_cost, speed) = price_row.unwrap_or((0.0, "medium".to_string()));
        total_cost += input_cost;
        count += 1;

        routing.push(json!({
            "intent": intent,
            "provider": provider,
            "model": model,
            "inputCost": input_cost,
            "speed": speed,
        }));
    }

    let avg_cost = if count > 0 {
        total_cost / count as f64
    } else {
        0.0
    };

    Json(json!({
        "mode": mode,
        "estimatedAvgCostInputPerMillion": (avg_cost * 100.0).round() / 100.0,
        "routing": routing,
    }))
}

/// Esegue la sync del catalogo modelli da LiteLLM. Riusabile sia dall'handler
/// REST che da un task background schedulato.
pub async fn run_catalog_sync(db: &sqlx::PgPool) -> Result<(i32, i32, i32), String> {
    const LITELLM_URL: &str = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("client build: {e}"))?;

    let resp = client
        .get(LITELLM_URL)
        .send()
        .await
        .map_err(|e| format!("fetch: {e}"))?;
    let data: Value = resp.json().await.map_err(|e| format!("parse: {e}"))?;
    let obj = data
        .as_object()
        .ok_or_else(|| "JSON non oggetto".to_string())?;

    // Prefissi di matching + politica insert dal registry (data-driven, T5):
    // era una mappa hardcoded a 5 provider (models.rs).
    #[derive(sqlx::FromRow)]
    struct SyncPrefixRow {
        name: String,
        litellm_prefixes: Vec<String>,
        litellm_sync_inserts: bool,
    }
    let sync_rows: Vec<SyncPrefixRow> = sqlx::query_as(
        "SELECT name, litellm_prefixes, litellm_sync_inserts
           FROM nexus_provider_registry
          WHERE litellm_prefixes IS NOT NULL AND array_length(litellm_prefixes, 1) > 0",
    )
    .fetch_all(db)
    .await
    .map_err(|e| format!("registry litellm_prefixes: {e}"))?;
    // Lista piatta (prefix, provider, allow_insert).
    let prefix_map: Vec<(String, String, bool)> = sync_rows
        .iter()
        .flat_map(|r| {
            let name = r.name.clone();
            let allow = r.litellm_sync_inserts;
            r.litellm_prefixes
                .iter()
                .map(move |p| (p.clone(), name.clone(), allow))
        })
        .collect();
    if prefix_map.is_empty() {
        return Err(
            "nexus_provider_registry senza litellm_prefixes: applica la migrazione 0575".to_string(),
        );
    }

    // Modelli gia' presenti nel catalog: usati per la protezione no-insert dei
    // provider a listino curato (litellm_sync_inserts=false).
    let existing: std::collections::HashSet<(String, String)> =
        sqlx::query_as::<_, (String, String)>("SELECT provider, model FROM ai_price_catalog")
            .fetch_all(db)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();

    let mut updated = 0i32;
    let mut added = 0i32;
    let mut skipped = 0i32;

    for (key, entry) in obj {
        let Some((provider_owned, allow_insert)) = prefix_map
            .iter()
            .find(|(prefix, _, _)| key.starts_with(prefix.as_str()))
            .map(|(_, p, allow)| (p.clone(), *allow))
        else {
            skipped += 1;
            continue;
        };
        let provider: &str = &provider_owned;

        let input_cost = entry
            .get("input_cost_per_token")
            .and_then(Value::as_f64)
            .map(|c| c * 1_000_000.0)
            .unwrap_or(0.0);
        let output_cost = entry
            .get("output_cost_per_token")
            .and_then(Value::as_f64)
            .map(|c| c * 1_000_000.0)
            .unwrap_or(0.0);

        if input_cost == 0.0 && output_cost == 0.0 {
            skipped += 1;
            continue;
        }

        let model_id = if let Some(pos) = key.find('/') {
            &key[pos + 1..]
        } else {
            key.as_str()
        };

        // Protezione anti-inquinamento (T5): per i provider a listino curato
        // (litellm_sync_inserts=false) aggiorna solo i modelli GIA' presenti, mai
        // auto-inserisci modelli nuovi (LiteLLM espone centinaia di openrouter/*).
        if !allow_insert && !existing.contains(&(provider.to_string(), model_id.to_string())) {
            skipped += 1;
            continue;
        }

        let context_window = entry
            .get("max_input_tokens")
            .and_then(Value::as_i64)
            .or_else(|| entry.get("max_tokens").and_then(Value::as_i64))
            .unwrap_or(8192) as i32;

        // Classificazione capability UNICA (ADR 0024): metadata LiteLLM quando
        // presenti (function_calling, vision, reasoning), altrimenti euristica
        // per famiglia. Aggiornare i modelli aggiorna la classificazione.
        let meta_tool_use = entry
            .get("supports_function_calling")
            .and_then(Value::as_bool);
        let meta_vision = entry.get("supports_vision").and_then(Value::as_bool);
        let meta_reasoning = entry.get("supports_reasoning").and_then(Value::as_bool);
        // Punto unico DB-driven della lista vision-routable (mig 0373): qui
        // classify_capabilities scarta i falsi positivi vision di LiteLLM (es.
        // mistral-small con supports_vision=true) per i provider senza ramo
        // /vision/describe.
        let vision_routable = crate::model_catalog_sync::load_vision_routable(db).await;
        let caps = crate::model_catalog_sync::classify_capabilities(
            provider,
            model_id,
            meta_tool_use,
            meta_vision,
            meta_reasoning,
            &vision_routable,
        );

        // performance_tier: qui NON si deriva. Il tier ha UN SOLO punto di
        // derivazione (regola L): `refresh_tier_prior`, chiamato sotto dopo
        // l'upsert.
        //
        // Perche' (difetto misurato il 16/07, introdotto proprio da questo blocco).
        // Qui erano noti solo prezzo e finestra, NON l'agentic_index (che vive
        // nella riga e lo popola sync_agentic_index): il tier veniva derivato dal
        // solo prezzo e scritto, e `refresh_tier_prior` — l'altro punto, quello
        // che l'indice ce l'ha — girava su un path diverso (la discovery API) e
        // non lo correggeva. Due punti per la stessa domanda, e vinceva quello
        // MENO informato: mistral-large-2512 (agentic 5.5) classificato 'heavy'
        // perche' costa $0.50 con 262k di finestra, e le inversioni salite a 90
        // (peggio del nome, che ne faceva 64). Era il difetto che stavamo curando,
        // rifatto mentre lo curavamo.

        // UPSERT: l'UPDATE dei flag avviene SOLO se capability_source='auto'
        // (le righe 'manual' curate da admin/migrazioni sono protette, ADR 0024).
        // I costi/context si aggiornano sempre.
        //
        // Il TIER non compare: lo scrive `refresh_tier_prior` (punto unico) dopo
        // questo upsert, dall'agentic_index della riga (unico seme, mig 0608).
        // All'INSERT resta NULL — un modello nuovo non ha ancora fatti su cui
        // fondare una fascia, e NULL e' la verita' (la riga nasce comunque
        // is_enabled=false + unqualified, quindi fuori dal pool agentico).
        let result = sqlx::query(
            r#"INSERT INTO ai_price_catalog (
                provider, model, input_cost_per_million_tokens, output_cost_per_million_tokens,
                currency, context_window, supports_tool_use, supports_vision,
                uses_thinking_mode, agentic_thinking_policy, capability_source, is_enabled, display_name,
                performance_tier, tier_source
              ) VALUES ($1, $2, $3, $4, 'USD', $5, $6, $7, $8, $9, 'auto', FALSE, $2, NULL, NULL)
              ON CONFLICT (provider, model) DO UPDATE SET
                input_cost_per_million_tokens = EXCLUDED.input_cost_per_million_tokens,
                output_cost_per_million_tokens = EXCLUDED.output_cost_per_million_tokens,
                context_window = EXCLUDED.context_window,
                supports_tool_use = CASE WHEN ai_price_catalog.capability_source = 'auto'
                                         THEN EXCLUDED.supports_tool_use
                                         ELSE ai_price_catalog.supports_tool_use END,
                supports_vision = CASE WHEN ai_price_catalog.capability_source = 'auto'
                                       THEN EXCLUDED.supports_vision
                                       ELSE ai_price_catalog.supports_vision END,
                uses_thinking_mode = CASE WHEN ai_price_catalog.capability_source = 'auto'
                                          THEN EXCLUDED.uses_thinking_mode
                                          ELSE ai_price_catalog.uses_thinking_mode END,
                agentic_thinking_policy = CASE WHEN ai_price_catalog.capability_source = 'auto'
                                          THEN EXCLUDED.agentic_thinking_policy
                                          ELSE ai_price_catalog.agentic_thinking_policy END,
                updated_at = NOW()
              RETURNING (xmax = 0) AS inserted"#,
        )
        .bind(provider)
        .bind(model_id)
        .bind(input_cost)
        .bind(output_cost)
        .bind(context_window)
        .bind(caps.supports_tool_use)
        .bind(caps.supports_vision)
        .bind(caps.uses_thinking_mode)
        .bind(caps.agentic_thinking_policy)
        .fetch_one(db)
        .await;

        match result {
            Ok(row) => {
                let inserted: bool = row.try_get("inserted").unwrap_or(false);
                if inserted {
                    added += 1;
                } else {
                    updated += 1;
                }
                // PUNTO UNICO del tier (regola L): la riga ora ha prezzo, finestra,
                // capability provate E agentic_index. Il prior si esprime con TUTTI
                // i fatti, non solo col prezzo — che era il difetto di questo blocco.
                crate::model_catalog_sync::refresh_tier_prior(db, provider, model_id).await;
            }
            Err(_) => {
                skipped += 1;
            }
        }
    }

    let _ = sqlx::query(
        "INSERT INTO settings (key, value) VALUES ('model_catalog_last_sync', $1)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(db)
    .await;

    tracing::info!(
        "run_catalog_sync: added={} updated={} skipped={}",
        added,
        updated,
        skipped
    );

    // Fix M53-auto: post-sync auto-promotion. Identifica per ogni "famiglia"
    // di modelli (es. gpt-5-mini, claude-opus-4-7, mistral-large-3) il piu
    // recente nel catalog, lo abilita e lo propaga in nexus_routing_matrix
    // + nexus_provider_default_model. Idempotente: se la famiglia e gia al
    // top, no-op.
    if let Err(e) = auto_upgrade_models_and_routing(db).await {
        tracing::warn!("auto_upgrade_models_and_routing fallito: {e}");
    }

    // Reconciliation policy->catalog (regola H/L): il sync LiteLLM e' un upsert
    // di pricing/capabilities ma non riallinea is_enabled alla policy. Senza
    // questo passo i modelli importati staticamente (non re-elencati dal
    // discovery API) restano col loro is_enabled iniziale per sempre. La
    // funzione e' il punto unico che applica la nexus_model_selection_policy a
    // tutto il catalog.
    if let Err(e) = crate::model_catalog_sync::reconcile_catalog_with_policy(db).await {
        tracing::warn!("reconcile_catalog_with_policy fallito: {e}");
    }

    Ok((added, updated, skipped))
}

/// Fix M53-auto: regole "famiglia modello" per auto-promotion. La regex
/// matcha tutti i modelli appartenenti alla stessa categoria funzionale
/// (es. mini, nano, opus, ecc.). L'ordinamento semantico individua il piu
/// recente: vengono confrontate le parti numeriche separate da "-" o ".".
const FAMILY_RULES: &[(
    &str, /* provider */
    &str, /* regex */
    &str, /* label */
)] = &[
    // OpenAI
    ("openai", r"^gpt-\d+(\.\d+)?$", "gpt-frontier"),
    ("openai", r"^gpt-\d+(\.\d+)?-pro$", "gpt-pro"),
    ("openai", r"^gpt-\d+(\.\d+)?-mini$", "gpt-mini"),
    ("openai", r"^gpt-\d+(\.\d+)?-nano$", "gpt-nano"),
    ("openai", r"^gpt-\d+(\.\d+)?-codex$", "gpt-codex"),
    ("openai", r"^gpt-\d+(\.\d+)?-codex-mini$", "gpt-codex-mini"),
    // Anthropic
    ("anthropic", r"^claude-opus-\d+-\d+$", "claude-opus"),
    ("anthropic", r"^claude-sonnet-\d+-\d+$", "claude-sonnet"),
    ("anthropic", r"^claude-haiku-\d+-\d+$", "claude-haiku"),
    // Google
    // Famiglie stable (suffisso vuoto). I preview/customtools sono famiglie
    // separate: tipicamente piu' capable ma instabili (possono sparire),
    // quindi vengono promossi solo all'interno della propria famiglia, mai
    // sovrascrivono lo stable a parita' di versione major.
    ("google", r"^gemini-\d+(\.\d+)?-flash$", "gemini-flash"),
    (
        "google",
        r"^gemini-\d+(\.\d+)?-flash-lite$",
        "gemini-flash-lite",
    ),
    ("google", r"^gemini-\d+(\.\d+)?-pro$", "gemini-pro"),
    // Preview families: includono -preview / -preview-customtools / -preview-NN-YYYY
    (
        "google",
        r"^gemini-\d+(\.\d+)?-pro-preview(-[a-z0-9-]+)?$",
        "gemini-pro-preview",
    ),
    (
        "google",
        r"^gemini-\d+(\.\d+)?-flash-preview(-[a-z0-9-]+)?$",
        "gemini-flash-preview",
    ),
    (
        "google",
        r"^gemini-\d+(\.\d+)?-flash-lite-preview(-[a-z0-9-]+)?$",
        "gemini-flash-lite-preview",
    ),
    // Latest aliases (Google rolling alias)
    ("google", r"^gemini-pro-latest$", "gemini-pro-latest-alias"),
    (
        "google",
        r"^gemini-flash-latest$",
        "gemini-flash-latest-alias",
    ),
    (
        "google",
        r"^gemini-flash-lite-latest$",
        "gemini-flash-lite-latest-alias",
    ),
    // Mistral: matcha sia il formato data abbreviata (large-2411) sia
    // la nuova nomenclatura semantica (large-3). parse_version skippa
    // i YYMM date, quindi "large-3" [3] vince su "large-2411" [].
    ("mistral", r"^mistral-large-\d+$", "mistral-large"),
    (
        "mistral",
        r"^mistral-medium-\d+(-\d+-\d+)?$",
        "mistral-medium",
    ),
    (
        "mistral",
        r"^mistral-small-\d+(-\d+-\d+)?$",
        "mistral-small",
    ),
    (
        "mistral",
        r"^magistral-medium-\d+(-\d+-\d+)?$",
        "magistral-medium",
    ),
    ("mistral", r"^codestral-\d+$", "codestral"),
    ("mistral", r"^devstral-medium-\d+$", "devstral-medium"),
    // DeepSeek
    ("deepseek", r"^deepseek-v\d+(\.\d+)?$", "deepseek-v"),
];

/// Parsa una versione embedded in un nome modello in una tupla di interi.
/// Skippa i suffissi data (YYYYMMDD, 8 cifre) per evitare di confonderli
/// con sub-versioni semantiche. Es:
/// - "gpt-5.4-mini"               -> [5, 4]
/// - "claude-opus-4-7"            -> [4, 7]
/// - "claude-opus-4-7-20260416"   -> [4, 7]   (skip 20260416)
/// - "claude-opus-4-20250514"     -> [4]      (skip 20250514)
/// - "mistral-large-2411"         -> [2411]   (4 cifre, non skippato — e "data abbreviata" mistral pattern)
/// - "deepseek-v3.2"              -> [3, 2]
fn parse_version(name: &str) -> Vec<i64> {
    // Numeri da skippare come "date abbreviate o complete":
    // - 8 cifre = YYYYMMDD (es. 20260416)
    // - 6 cifre = YYYYMM   (es. 202604)
    // - 4 cifre = YYMM     (formato Mistral: es. 2411 = nov 2024)
    fn looks_like_date(s: &str, n: i64) -> bool {
        match s.len() {
            8 => (20200101..=20351231).contains(&n),
            6 => (202001..=203512).contains(&n),
            4 => {
                // YYMM: anno 24-35, mese 01-12. Es. 2411 = nov 2024.
                let yy = n / 100;
                let mm = n % 100;
                (24..=35).contains(&yy) && (1..=12).contains(&mm)
            }
            _ => false,
        }
    }
    let mut nums = Vec::new();
    let mut cur = String::new();
    for c in name.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else {
            if !cur.is_empty() {
                if let Ok(n) = cur.parse::<i64>() {
                    if !looks_like_date(&cur, n) {
                        nums.push(n);
                    }
                }
                cur.clear();
            }
        }
    }
    if !cur.is_empty() {
        if let Ok(n) = cur.parse::<i64>() {
            if !looks_like_date(&cur, n) {
                nums.push(n);
            }
        }
    }
    nums
}

/// Auto-promotion modelli: per ogni famiglia trova il modello piu recente,
/// lo abilita in ai_price_catalog, e aggiorna nexus_routing_matrix +
/// nexus_provider_default_model dove esiste un modello "vecchio" della stessa
/// famiglia. Idempotente.
pub async fn auto_upgrade_models_and_routing(db: &sqlx::PgPool) -> Result<(), String> {
    use regex::Regex;
    let mut promotions: Vec<(String, String, String)> = Vec::new(); // (provider, family, top_model)
    let mut enabled_count = 0_usize;

    for (provider, pattern, family_label) in FAMILY_RULES {
        let re = match Regex::new(pattern) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("auto_upgrade: regex {pattern} invalida: {e}");
                continue;
            }
        };
        // Carica tutti i modelli del provider che matchano la famiglia.
        // IMPORTANTE (chiusura residuo probe attivo): escludiamo i modelli con
        // `auto_disabled_reason` esplicito (es. `failed_initial_probe:model_not_found`
        // settato dal probe-on-insert in `model_catalog_sync.rs`, oppure
        // `model_not_found`/`hollow_completion` dal `model_health_probe` worker,
        // oppure `manual:*` impostato dall'admin). Senza questo filtro, la
        // riabilitazione di massa alla riga sottostante annulla qualsiasi
        // decisione del probe e i modelli "fantasma" tornerebbero enabled +
        // verrebbero promossi come "top family" -> default_provider_model.
        let candidates: Vec<String> = sqlx::query_scalar(
            "SELECT model FROM ai_price_catalog \
             WHERE provider = $1 \
               AND (auto_disabled_reason IS NULL OR auto_disabled_reason = '')",
        )
        .bind(provider)
        .fetch_all(db)
        .await
        .map_err(|e| format!("query candidates: {e}"))?
        .into_iter()
        .filter(|m: &String| re.is_match(m))
        .collect();
        if candidates.is_empty() {
            continue;
        }
        // Ordina semanticamente:
        // 1) parse_version desc (versione semantica)
        // 2) presenza 'customtools' → vince (Google: tool_use_optimized variant)
        // 3) nome piu' corto (alias non-dated, es. claude-opus-4-7 < claude-opus-4-7-20260416)
        //
        // Il punto 2 e' necessario per la famiglia gemini-pro-preview: la
        // variant `-customtools` ha lo STESSO numero di versione del base
        // `gemini-3.1-pro-preview` ma e' la versione tool-use-optimized, da
        // preferire quando serve l'escalation per task tool-heavy.
        let mut sorted = candidates.clone();
        sorted.sort_by(|a, b| {
            let va = parse_version(a);
            let vb = parse_version(b);
            let cmp_ver = vb.cmp(&va);
            if cmp_ver != std::cmp::Ordering::Equal {
                return cmp_ver;
            }
            // Tiebreaker: customtools vince
            let a_ct = a.contains("customtools");
            let b_ct = b.contains("customtools");
            match (a_ct, b_ct) {
                (true, false) => return std::cmp::Ordering::Less, // a vince
                (false, true) => return std::cmp::Ordering::Greater, // b vince
                _ => {}
            }
            // Tiebreaker finale: nome piu' corto
            a.len().cmp(&b.len())
        });
        let top = sorted[0].clone();
        promotions.push((provider.to_string(), family_label.to_string(), top.clone()));

        // Abilita i modelli della famiglia AMMESSI dalla policy (ADR 0025): un
        // family-label legacy non deve ri-abilitare i suoi modelli (pruned dalla
        // 0320). La regola di ammissione e' il punto unico model_passes_selection_policy.
        for m in &candidates {
            if !crate::model_catalog_sync::model_passes_selection_policy(db, provider, m).await {
                continue;
            }
            // Prezzo ignoto -> non routabile, quindi non abilitabile nemmeno per
            // famiglia (punto unico del predicato: regola L). Senza questo guard,
            // un solo membro della famiglia a listino noto ne trascinerebbe dentro
            // altri a costo placeholder.
            let res = sqlx::query(&format!(
                "UPDATE ai_price_catalog SET is_enabled = true \
                 WHERE provider = $1 AND model = $2 AND is_enabled = false \
                   AND NOT {price_unknown}",
                price_unknown = crate::model_catalog_sync::price_unknown_sql(""),
            ))
            .bind(provider)
            .bind(m)
            .execute(db)
            .await;
            if let Ok(r) = res {
                if r.rows_affected() > 0 {
                    enabled_count += 1;
                }
            }
        }

        // NB (ADR 0030, regola L): la materializzazione del `model_id` della
        // routing matrix e' ESCLUSIVA di
        // `routing_matrix_auto_promoter::run_one_round` (via il selettore unico
        // di model_selection.rs). auto_upgrade NON scrive piu' `model_id` sulla
        // matrix: prima lo sostituiva per nome-famiglia (FAMILY_RULES), in
        // conflitto con lo scoring di run_one_round -> ping-pong non
        // deterministico sulle righe non-manual (chi gira per ultimo vince).
        // L'upgrade alla versione nuova avviene comunque: il modello nuovo e'
        // gia' stato abilitato qui sopra (is_enabled=true) e run_one_round lo
        // seleziona via scoring; le righe stale (modello deprecato) sono gestite
        // da heal_orphan_pinned_models + cleanup_stale_rows. Verificato: tutte le
        // righe non-manual attive sono coperte da un requirement, quindi
        // run_one_round le materializza al 100% (nessun buco di copertura).
        //
        // Il default-per-provider (nexus_provider_default_model) NON e'
        // materializzato da run_one_round: resta aggiornato qui per famiglia,
        // rispettando i pin manuali.
        let to_replace: Vec<String> = candidates.iter().filter(|m| **m != top).cloned().collect();
        if !to_replace.is_empty() {
            // default_model_for_provider (se il default e vecchio
            // della stessa famiglia). NB: la tabella nexus_provider_default_model
            // non ha un campo manual_override; come heuristica, rispettiamo
            // le note che iniziano con "manual fix" o "pin:" come marker
            // admin (utente puo' impostarlo con UPDATE in console SQL admin).
            let _ = sqlx::query(
                "UPDATE nexus_provider_default_model \
                 SET model_id = $1, updated_at = NOW(), notes = COALESCE(notes,'') || ' | auto-upgrade ' || $2 \
                 WHERE provider = $3 AND model_id = ANY($4) \
                   AND COALESCE(notes,'') !~* '(manual fix|^pin:|admin-pinned)'",
            )
            .bind(&top)
            .bind(family_label.to_string())
            .bind(provider)
            .bind(&to_replace)
            .execute(db)
            .await;
        }
    }

    tracing::info!(
        "auto_upgrade_models_and_routing: enabled={} promotions={}",
        enabled_count,
        promotions.len()
    );
    for (p, fam, top) in &promotions {
        tracing::debug!("  {} / {} -> top = {}", p, fam, top);
    }

    // ── Auto-popolamento escalation_* (LIVELLO A) ─────────────────────────
    // Mig 0120 popolo' solo alcuni intent legacy con threshold 100k. I nuovi
    // modelli scoperti via catalog_sync e i nuovi intent non avevano mai
    // escalation valorizzata, quindi `lookup_with_budget` non escalava mai.
    // Dalla mig 0475 il target dell'escalation budget-aware e' DERIVATO dalla
    // vista v_model_escalation_chain (stessa fonte del LIVELLO B, regola L):
    // niente piu' coppie hardcoded, niente piu' dipendenza da `promotions`.
    if let Err(e) = auto_populate_escalations(db).await {
        tracing::warn!("auto_populate_escalations failed: {e}");
    }

    Ok(())
}

/// Popola `escalation_*` (LIVELLO A budget-aware) nella routing matrix per le
/// righe dove `escalation_model_id` e' NULL, DERIVANDO il target dalla vista
/// `v_model_escalation_chain` (mig 0471/0475) — un solo punto di verita',
/// regola L. Niente piu' coppie hardcoded (vecchia const ESCALATION_PAIRS):
/// e' la stessa fonte dati del LIVELLO B (loop intra-provider), cosi' un nuovo
/// modello sincronizzato nel catalog entra automaticamente anche qui.
///
/// Per ogni riga eleggibile, il target e' il modello dello STESSO provider con
/// `performance_tier` STRETTAMENTE superiore al modello corrente, il piu'
/// economico tra quelli (`escalation_rank ASC`), tool-capable
/// (`supports_tool_use = TRUE`, perche' l'escalation serve a uscire da loop
/// agentici). La soglia di token e' DB-driven (regola G), letta dal setting
/// `routing.escalation_budget_threshold_tokens` (valore di bootstrap 16000 in
/// mig 0475; il default qui sotto e' lo stesso valore di bootstrap del setting,
/// NON un magic-fallback di model-id).
///
/// Rispetta i pin admin: salta le righe con `manual_override = true`.
async fn auto_populate_escalations(db: &sqlx::PgPool) -> Result<(), String> {
    // Soglia DB-driven (regola G). Default = valore di bootstrap del setting
    // (mig 0475), usato solo se la chiave manca o non e' parsabile come i32.
    const DEFAULT_THRESHOLD_TOKENS: i32 = 16000;
    let threshold: i32 = nexus_auth::get_setting(db, "routing.escalation_budget_threshold_tokens")
        .await
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or(DEFAULT_THRESHOLD_TOKENS);

    // UNA query: deriva il target dalla vista. Il sub-select sceglie il modello
    // dello stesso provider con tier strettamente superiore, tool-capable, il
    // piu' economico (escalation_rank ASC). Se il modello corrente NON e' (piu')
    // enabled nel catalog la subquery del tier ritorna NULL e la riga NON viene
    // popolata: il routing non userebbe comunque un base disabilitato, quindi
    // l'escalation sarebbe irrilevante (evita de-escalation insensate).
    let res = sqlx::query(
        "UPDATE nexus_routing_matrix m \
         SET escalation_threshold_tokens = $1, \
             escalation_provider = m.provider, \
             escalation_model_id = ( \
                 SELECT v.model FROM v_model_escalation_chain v \
                 WHERE v.provider = m.provider AND v.supports_tool_use = TRUE \
                   AND v.performance_tier_ord > ( \
                       SELECT b.performance_tier_ord FROM v_model_escalation_chain b \
                       WHERE b.provider = m.provider AND b.model = m.model_id) \
                 ORDER BY v.escalation_rank ASC LIMIT 1), \
             updated_at = NOW() \
         WHERE m.escalation_model_id IS NULL \
           AND (m.manual_override IS NULL OR m.manual_override = false) \
           AND EXISTS ( \
               SELECT 1 FROM v_model_escalation_chain v \
               WHERE v.provider = m.provider AND v.supports_tool_use = TRUE \
                 AND v.performance_tier_ord > ( \
                     SELECT b.performance_tier_ord FROM v_model_escalation_chain b \
                     WHERE b.provider = m.provider AND b.model = m.model_id))",
    )
    .bind(threshold)
    .execute(db)
    .await
    .map_err(|e| format!("auto_populate_escalations UPDATE: {e}"))?;

    if res.rows_affected() > 0 {
        tracing::info!(
            "auto_populate_escalations: {} righe popolate dalla vista (soglia {} token)",
            res.rows_affected(),
            threshold
        );
    }
    Ok(())
}

/// POST /api/admin/sync-model-catalog
/// Scarica il JSON LiteLLM da GitHub e aggiorna i prezzi in ai_price_catalog
pub async fn sync_model_catalog(State(state): State<AppState>) -> Json<Value> {
    match run_catalog_sync(&state.db).await {
        Ok((added, updated, skipped)) => Json(json!({
            "added": added, "updated": updated, "skipped": skipped,
            "source": "LiteLLM GitHub",
        })),
        Err(e) => Json(json!({
            "error": e, "added": 0, "updated": 0, "skipped": 0,
        })),
    }
}

/// POST /api/admin/probe-models
/// Esegue un round one-shot del model_health_probe: pinga ogni modello
/// enabled, applica counter / auto-disable. Usa la soglia configurata in
/// settings.model_health_probe_failure_threshold (default 3).
pub async fn probe_models_now(State(state): State<AppState>) -> Json<Value> {
    let threshold = crate::settings::get_setting(&state.db, "model_health_probe_failure_threshold")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i32>().ok())
        .unwrap_or(3);
    let orchestrator = std::sync::Arc::new(state.orchestrator.clone());
    let stats = crate::model_health_probe::run_one_round(&orchestrator, &state.db, threshold).await;
    Json(json!({
        "ok": true,
        "total": stats.total,
        "healthy": stats.healthy,
        "provider_wide_errors": stats.provider_wide_errors,
        "model_errors": stats.model_errors,
        "auto_disabled": stats.auto_disabled,
        "skipped_provider_cooldown": stats.skipped_provider_cooldown,
        "failure_threshold": threshold,
    }))
}
