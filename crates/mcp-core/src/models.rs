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
    /// Tariffa dei token letti da cache (mig 0130, popolata dalla 0403 con i
    /// rapporti per provider). `None` = il catalog non la conosce per questo
    /// modello: chi prezza deve dirlo, non stimarla. Senza questo campo nel wire
    /// il frontend compensava con un `input * 0.1` scritto a mano, che e' la
    /// stima giusta per Anthropic e sbagliata per tutti gli altri.
    #[sqlx(default)]
    pub cache_read_cost_per_million_tokens: Option<f64>,
    /// Tariffa dei token SCRITTI in cache (mig 0403). `None` = il catalog non la
    /// conosce. Serve accanto alla tariffa di lettura perche' sono due
    /// sottoinsiemi distinti del prompt, con due prezzi (vedi
    /// `nexus_gateway::LlmUsage`): senza questa riga il frontend non ha modo di
    /// scorporare i token di cache_creation e li paga a tariffa piena di input.
    #[sqlx(default)]
    pub cache_creation_cost_per_million_tokens: Option<f64>,
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
             cache_read_cost_per_million_tokens::float8 AS cache_read_cost_per_million_tokens, \
             cache_creation_cost_per_million_tokens::float8 \
                 AS cache_creation_cost_per_million_tokens, \
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

/// Legge una tariffa dal JSON LiteLLM e la porta nell'unita' del catalog
/// (dollari per milione di token). Punto unico della conversione (regola L):
/// upstream ogni prezzo e' per-token, in tabella e' per-milione, e la
/// moltiplicazione scritta a mano in piu' punti e' il modo in cui una delle
/// copie resta indietro di un fattore mille.
///
/// Ritorna `None` quando il campo manca o non e' un numero. La distinzione fra
/// assente e zero e' il punto: per le tariffe di CACHE zero significherebbe
/// "servito gratis", mentre l'assenza significa "non so" — due cose che il
/// listino tratta in modo opposto (vedi `nexus_pricing::calculate_cost_breakdown`).
fn tariffa_per_milione(entry: &Value, campo: &str) -> Option<f64> {
    entry
        .get(campo)
        .and_then(Value::as_f64)
        .map(|c| c * 1_000_000.0)
}

/// I campi che il sync porta da una voce del JSON upstream a una riga di catalog.
/// Struct e non nove argomenti sciolti: `input_cost` e `output_cost` hanno lo
/// stesso tipo e si scambiano senza che il compilatore se ne accorga.
pub(crate) struct VoceCatalog<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub input_cost: f64,
    pub output_cost: f64,
    pub context_window: i32,
    pub caps: &'a crate::model_catalog_sync::ClassifiedCaps,
    /// `None` = la fonte non dichiara la tariffa. NON zero: vedi
    /// [`tariffa_per_milione`].
    pub cache_read_cost: Option<f64>,
    pub cache_creation_cost: Option<f64>,
}

/// Scrive (o aggiorna) una riga del catalog. Ritorna `true` se la riga e' NUOVA.
///
/// Estratta da [`run_catalog_sync`] per essere raggiungibile da un test: finche'
/// la query viveva dentro il loop, l'unico modo di verificarla era ricopiarla
/// nel test, cioe' misurare una sua imitazione (regola O).
///
/// L'UPDATE dei flag avviene SOLO se `capability_source='auto'` (le righe
/// `manual` curate da admin/migrazioni sono protette, ADR 0024). I costi si
/// aggiornano dove `price_locked` e' false (mig 0715: le righe col listino
/// curato — deepseek v4, il cui prezzo base e' l'off-peak delle finestre orarie
/// — non si fanno riscrivere dalla fonte); la finestra di contesto si aggiorna
/// sempre. `pricing_state` non e' toccato da questa query (il trigger della
/// 0583 promuove solo 'unknown' -> 'priced' e non degrada mai).
///
/// Il TIER non compare: lo scrive `refresh_tier_prior` (punto unico) dopo questo
/// upsert, dall'agentic_index della riga (unico seme, mig 0608). All'INSERT
/// resta NULL — un modello nuovo non ha ancora fatti su cui fondare una fascia,
/// e NULL e' la verita' (la riga nasce comunque `is_enabled=false` +
/// `unqualified`, quindi fuori dal pool agentico).
pub(crate) async fn upsert_voce_catalog(
    db: &sqlx::PgPool,
    voce: &VoceCatalog<'_>,
) -> Result<bool, sqlx::Error> {
    let row = sqlx::query(SQL_UPSERT_VOCE_CATALOG)
        .bind(voce.provider)
        .bind(voce.model)
        .bind(voce.input_cost)
        .bind(voce.output_cost)
        .bind(voce.context_window)
        .bind(voce.caps.supports_tool_use)
        .bind(voce.caps.supports_vision)
        .bind(voce.caps.uses_thinking_mode)
        .bind(voce.caps.agentic_thinking_policy)
        .bind(voce.cache_read_cost)
        .bind(voce.cache_creation_cost)
        .fetch_one(db)
        .await?;
    Ok(row.try_get("inserted").unwrap_or(false))
}

/// La query di [`upsert_voce_catalog`]. Costante e non letterale inline perche'
/// da sola supera la soglia di lunghezza del gate di qualita': tenerla dentro la
/// funzione farebbe crescere una funzione che di logica non ne ha.
const SQL_UPSERT_VOCE_CATALOG: &str = r#"INSERT INTO ai_price_catalog (
                provider, model, input_cost_per_million_tokens, output_cost_per_million_tokens,
                currency, context_window, supports_tool_use, supports_vision,
                uses_thinking_mode, agentic_thinking_policy, capability_source, is_enabled, display_name,
                performance_tier, tier_source,
                cache_read_cost_per_million_tokens, cache_creation_cost_per_million_tokens
              ) VALUES ($1, $2, $3, $4, 'USD', $5, $6, $7, $8, $9, 'auto', FALSE, $2, NULL, NULL,
                        $10, $11)
              ON CONFLICT (provider, model) DO UPDATE SET
                -- Il lucchetto dei prezzi curati (mig 0715): dove price_locked e'
                -- true i 4 campi prezzo vengono da una migrazione (es. il listino
                -- OFF-PEAK deepseek, il cui peak lo produce ai_price_window) e la
                -- fonte NON li sovrascrive — LiteLLM non conosce le fasce orarie
                -- e il suo numero cancellerebbe il prezzo base. Stesso pattern di
                -- capability_source='manual' qui sotto.
                input_cost_per_million_tokens = CASE WHEN ai_price_catalog.price_locked
                    THEN ai_price_catalog.input_cost_per_million_tokens
                    ELSE EXCLUDED.input_cost_per_million_tokens END,
                output_cost_per_million_tokens = CASE WHEN ai_price_catalog.price_locked
                    THEN ai_price_catalog.output_cost_per_million_tokens
                    ELSE EXCLUDED.output_cost_per_million_tokens END,
                -- La fonte ARRICCHISCE, non cancella: si aggiorna solo quando
                -- LiteLLM porta la tariffa. Assegnare EXCLUDED secco azzererebbe a
                -- NULL i valori curati dalle migrazioni 0130/0403 ogni volta che la
                -- fonte tace su quel modello, e quei token tornerebbero a tariffa
                -- piena senza che nessuno lo abbia deciso.
                cache_read_cost_per_million_tokens = CASE WHEN ai_price_catalog.price_locked
                    THEN ai_price_catalog.cache_read_cost_per_million_tokens
                    ELSE COALESCE(EXCLUDED.cache_read_cost_per_million_tokens,
                    ai_price_catalog.cache_read_cost_per_million_tokens) END,
                cache_creation_cost_per_million_tokens = CASE WHEN ai_price_catalog.price_locked
                    THEN ai_price_catalog.cache_creation_cost_per_million_tokens
                    ELSE COALESCE(EXCLUDED.cache_creation_cost_per_million_tokens,
                    ai_price_catalog.cache_creation_cost_per_million_tokens) END,
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
              RETURNING (xmax = 0) AS inserted"#;

/// Esegue la sync del catalogo modelli da LiteLLM. Riusabile sia dall'handler
/// REST che da un task background schedulato.
///
/// Le tariffe di prompt-cache (`cache_read_input_token_cost` /
/// `cache_creation_input_token_cost`) arrivano da QUI, cioe' dalla stessa fonte
/// da cui gia' arrivavano input e output. Prima venivano solo da migrazioni
/// scritte a mano che selezionavano per pattern di NOME (`0403`, clausola
/// `model LIKE 'gemini-2.5%'`): ogni famiglia nuova nasceva senza tariffa e i
/// suoi token serviti da cache venivano fatturati a prezzo pieno finche'
/// qualcuno non se ne accorgeva e scriveva la migrazione successiva. Misurato il
/// 29/07/2026 su `gemini-3.1-flash-lite`: 65.595 token letti da cache in 7
/// giorni, tutti a tariffa piena, mentre upstream la tariffa c'era (0,025 $/M
/// contro 0,25 $/M di input). Inseguire i nomi a colpi di migrazione e' la
/// toppa; leggere il campo dalla fonte e' il fix.
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

        // Per input/output l'assenza vale 0.0: la coppia a zero fa saltare la riga
        // subito sotto, quindi il "non so" si dichiara li' e non entra in tabella.
        let input_cost = tariffa_per_milione(entry, "input_cost_per_token").unwrap_or(0.0);
        let output_cost = tariffa_per_milione(entry, "output_cost_per_token").unwrap_or(0.0);
        // Le tariffe di cache restano `Option`: qui l'assenza NON e' zero. Zero
        // direbbe "servito gratis" e sottostimerebbe la chiamata; NULL dice "non
        // so", e il listino lo tratta per quello che e' rimettendo quei token nel
        // monte a tariffa piena (`nexus_pricing::calculate_cost_breakdown`, campo
        // `cache_tokens_billed_as_input`).
        let cache_read_cost = tariffa_per_milione(entry, "cache_read_input_token_cost");
        let cache_creation_cost = tariffa_per_milione(entry, "cache_creation_input_token_cost");

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

        let result = upsert_voce_catalog(
            db,
            &VoceCatalog {
                provider,
                model: model_id,
                input_cost,
                output_cost,
                context_window,
                caps: &caps,
                cache_read_cost,
                cache_creation_cost,
            },
        )
        .await;

        match result {
            Ok(inserted) => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_catalog_sync::ClassifiedCaps;
    use nexus_pricing::{calculate_cost_breakdown, PriceSnapshot, TokenUsage};
    use sqlx::PgPool;

    /// Frammento VERBATIM di `model_prices_and_context_window.json` (letto da
    /// upstream il 29/07/2026). Non un JSON inventato: l'assunto da verificare e'
    /// proprio come sono fatti i campi di quella fonte, e riscriverli a mano
    /// fisserebbe l'assunto invece di misurarlo (regola O).
    fn voce_gemini_3_1_flash_lite() -> Value {
        serde_json::json!({
            "input_cost_per_token": 2.5e-07,
            "output_cost_per_token": 1.5e-06,
            "cache_read_input_token_cost": 2.5e-08,
            "max_input_tokens": 1048576
        })
    }

    fn quasi(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn caps() -> ClassifiedCaps {
        ClassifiedCaps {
            supports_tool_use: true,
            supports_vision: false,
            uses_thinking_mode: false,
            agentic_thinking_policy: "none",
        }
    }

    #[test]
    fn tariffa_dal_json_litellm_arriva_in_dollari_per_milione() {
        let voce = voce_gemini_3_1_flash_lite();
        assert!(quasi(
            tariffa_per_milione(&voce, "input_cost_per_token").unwrap(),
            0.25
        ));
        // La tariffa che il catalog non aveva: 0,025 $/M contro 0,25 di input,
        // cioe' il decimo. E' il numero che rendeva sovrastimata ogni chiamata
        // gemini-3 servita da cache.
        assert!(quasi(
            tariffa_per_milione(&voce, "cache_read_input_token_cost").unwrap(),
            0.025
        ));
    }

    /// Il campo ASSENTE deve dare `None`, mai `Some(0.0)`.
    ///
    /// E' la distinzione su cui poggia il fix: `cache_creation` non c'e' per
    /// nessun modello Gemini (il caching implicito non fattura la scrittura), e
    /// uno zero li' direbbe "scrivere in cache e' gratis" — un'affermazione che
    /// la fonte non fa.
    #[test]
    fn campo_assente_e_ignoranza_non_gratuita() {
        let voce = voce_gemini_3_1_flash_lite();
        assert_eq!(
            tariffa_per_milione(&voce, "cache_creation_input_token_cost"),
            None
        );
        assert_eq!(tariffa_per_milione(&voce, "campo_inesistente"), None);
    }

    /// La CONSEGUENZA, non la stringa: la tariffa estratta sopra, messa nel
    /// listino, cambia il costo della chiamata. Senza, i token serviti da cache
    /// rientrano nel monte a tariffa piena e la stessa chiamata costa oltre
    /// cinque volte tanto — cio' che il ledger ha registrato per
    /// `gemini-3.1-flash-lite` finche' la tariffa non c'era.
    #[test]
    fn la_tariffa_estratta_sconta_davvero_la_chiamata() {
        let voce = voce_gemini_3_1_flash_lite();
        let listino = |cache_read| PriceSnapshot {
            input_cost_per_million_tokens: tariffa_per_milione(&voce, "input_cost_per_token")
                .unwrap(),
            output_cost_per_million_tokens: tariffa_per_milione(&voce, "output_cost_per_token")
                .unwrap(),
            cache_read_cost_per_million_tokens: cache_read,
            cache_creation_cost_per_million_tokens: None,
            currency: "USD".to_string(),
        };
        // Forma misurata sul campo il 29/07/2026: prefisso lungo, quasi tutto
        // servito da cache, risposta di una riga.
        let usage = TokenUsage {
            prompt_tokens: 10_000,
            completion_tokens: 1,
            cache_read_tokens: 9_000,
            cache_creation_tokens: 0,
        };

        let con = calculate_cost_breakdown(
            &listino(tariffa_per_milione(&voce, "cache_read_input_token_cost")),
            &usage,
        );
        let senza = calculate_cost_breakdown(&listino(None), &usage);

        assert_eq!(con.cache_tokens_billed_as_input, 0);
        assert!(quasi(con.cache_read_cost, 9_000.0 / 1e6 * 0.025));
        // Senza tariffa i token di cache rientrano tutti a prezzo pieno, ed e' il
        // listino stesso a dichiararlo.
        assert_eq!(senza.cache_tokens_billed_as_input, 9_000);
        assert_eq!(senza.cache_read_cost, 0.0);
        assert!(
            senza.total_cost > con.total_cost * 5.0,
            "senza tariffa la stessa chiamata deve costare molto di piu': \
             con={} senza={}",
            con.total_cost,
            senza.total_cost
        );
    }

    /// La tariffa deve arrivare IN TABELLA, non solo essere estratta.
    ///
    /// Gira sulle migrazioni REALI (`META_MIGRATOR`) e non sulla fixture
    /// `test_support::create_ai_price_catalog_table`, che e' una copia a mano
    /// dello schema e le due colonne di cache non le ha nemmeno: un test scritto
    /// su quella passerebbe descrivendo una tabella che in produzione non esiste
    /// (regola O). Ed esercita [`upsert_voce_catalog`], cioe' la query VERA:
    /// ricopiarla qui vorrebbe dire misurare una sua imitazione.
    ///
    /// MUTAZIONE: togliere le due colonne dall'INSERT — che e' esattamente lo
    /// stato in cui il sync e' vissuto finora — fa rosseggiare la prima assert.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_tariffa_di_cache_finisce_in_tabella(pool: PgPool) {
        let caps = caps();
        let voce = |cache_read| VoceCatalog {
            provider: "google",
            model: "gemini-3.1-flash-lite",
            input_cost: 0.25,
            output_cost: 1.5,
            context_window: 1_048_576,
            caps: &caps,
            cache_read_cost: cache_read,
            cache_creation_cost: None,
        };
        let leggi = |pool: PgPool| async move {
            sqlx::query_scalar::<_, Option<f64>>(
                "SELECT cache_read_cost_per_million_tokens::float8 FROM ai_price_catalog \
                 WHERE provider = 'google' AND model = 'gemini-3.1-flash-lite'",
            )
            .fetch_one(&pool)
            .await
            .expect("riga di catalog")
        };

        assert!(
            upsert_voce_catalog(&pool, &voce(Some(0.025)))
                .await
                .expect("insert"),
            "la prima scrittura crea la riga"
        );
        assert_eq!(leggi(pool.clone()).await, Some(0.025));

        // La fonte tace su questo modello al giro dopo: il valore gia' in tabella
        // NON va cancellato. Senza il COALESCE nell'UPDATE, qui si leggerebbe
        // `None` e quei token tornerebbero a tariffa piena da soli.
        assert!(
            !upsert_voce_catalog(&pool, &voce(None)).await.expect("update"),
            "la seconda scrittura aggiorna, non inserisce"
        );
        assert_eq!(leggi(pool.clone()).await, Some(0.025));

        // La fonte parla di nuovo, con un valore diverso: quello vince.
        upsert_voce_catalog(&pool, &voce(Some(0.011)))
            .await
            .expect("update");
        assert_eq!(leggi(pool.clone()).await, Some(0.011));
    }

    /// Una riga NUOVA di cui la fonte non dichiara la tariffa nasce con NULL, mai
    /// con zero: il listino distingue "non so" da "gratis", e il DB deve poter
    /// portare quella distinzione.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn senza_tariffa_la_riga_nasce_con_null(pool: PgPool) {
        let caps = caps();
        upsert_voce_catalog(
            &pool,
            &VoceCatalog {
                provider: "google",
                model: "gemini-3-pro-image",
                input_cost: 2.0,
                output_cost: 12.0,
                context_window: 32_768,
                caps: &caps,
                cache_read_cost: None,
                cache_creation_cost: None,
            },
        )
        .await
        .expect("insert");

        let letto = sqlx::query_scalar::<_, Option<f64>>(
            "SELECT cache_read_cost_per_million_tokens::float8 FROM ai_price_catalog \
             WHERE provider = 'google' AND model = 'gemini-3-pro-image'",
        )
        .fetch_one(&pool)
        .await
        .expect("riga di catalog");
        assert_eq!(letto, None, "l'assenza di tariffa non e' uno zero");
    }

    /// Il lucchetto dei prezzi curati (mig 0715): la riga deepseek v4 porta il
    /// listino OFF-PEAK — il peak lo produce `ai_price_window`, non una tariffa
    /// diversa in tabella — e il sync NON deve riscriverne i 4 campi prezzo il
    /// giorno in cui LiteLLM matchasse quei modelli: il suo numero, che le
    /// fasce orarie non le conosce, cancellerebbe il prezzo base.
    ///
    /// La riga arriva LOCKED dalla migrazione reale via META_MIGRATOR: il test
    /// non la crea e non la marca (regola O).
    ///
    /// MUTAZIONE: togliere i `CASE WHEN price_locked` da
    /// `SQL_UPSERT_VOCE_CATALOG` fa rosseggiare le quattro asserzioni sui
    /// prezzi (0.22 diventa 9.9); un CASE scritto al contrario (tutto locked)
    /// fa rosseggiare il controllo sulla riga non locked in coda.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_sync_non_riscrive_i_prezzi_di_una_riga_locked(pool: PgPool) {
        let caps = caps();
        let voce_sync = |model| VoceCatalog {
            provider: "deepseek",
            model,
            input_cost: 9.9,
            output_cost: 9.9,
            context_window: 65_536,
            caps: &caps,
            cache_read_cost: Some(9.9),
            cache_creation_cost: Some(9.9),
        };

        // La riga esiste gia' (seed 0715): questo e' l'UPDATE del sync.
        assert!(
            !upsert_voce_catalog(&pool, &voce_sync("deepseek-v4-flash"))
                .await
                .expect("upsert riga locked"),
            "la riga deve arrivare dalla migrazione, non da questo test"
        );

        let (input, output, cache_read, cache_creation, locked) =
            sqlx::query_as::<_, (f64, f64, Option<f64>, Option<f64>, bool)>(
                "SELECT input_cost_per_million_tokens::float8, \
                        output_cost_per_million_tokens::float8, \
                        cache_read_cost_per_million_tokens::float8, \
                        cache_creation_cost_per_million_tokens::float8, \
                        price_locked \
                   FROM ai_price_catalog \
                  WHERE provider = 'deepseek' AND model = 'deepseek-v4-flash'",
            )
            .fetch_one(&pool)
            .await
            .expect("riga di catalog");

        assert!(locked, "il lucchetto viene dalla mig 0715");
        assert!(quasi(input, 0.22), "input riscritto dal sync: {input}");
        assert!(quasi(output, 0.66), "output riscritto dal sync: {output}");
        assert!(quasi(cache_read.expect("tariffa curata"), 0.007));
        assert!(quasi(cache_creation.expect("tariffa curata"), 0.0));

        // Il controllo: una riga NON locked riceve i prezzi del sync come
        // sempre. Senza, un CASE invertito passerebbe le asserzioni sopra.
        upsert_voce_catalog(&pool, &voce_sync("deepseek-chat"))
            .await
            .expect("upsert riga non locked");
        let input_libero = sqlx::query_scalar::<_, f64>(
            "SELECT input_cost_per_million_tokens::float8 FROM ai_price_catalog \
              WHERE provider = 'deepseek' AND model = 'deepseek-chat'",
        )
        .fetch_one(&pool)
        .await
        .expect("riga di catalog");
        assert!(
            quasi(input_libero, 9.9),
            "la riga non locked deve aggiornarsi: {input_libero}"
        );
    }
}
