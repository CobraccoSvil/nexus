use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde_json::json;
use std::path::PathBuf;

// Tipi DTO: punto unico in nexus_types::settings_dto (regola L / ADR 0026, S8).
pub use nexus_types::settings_dto::{
    BulkUpdateRequest, CreateDirectoryRequest, FsBrowseQuery, Setting, UpdateSettingRequest,
};

// FS browse: punto unico in nexus_types::fs_browse (regola L / ADR 0026).
use nexus_types::fs_browse::{list_directories, list_root_candidates};
// Tipi e helper API: punto unico in nexus_types (regola L / ADR 0026, cluster E6).
// Prima `ApiError`/`ApiResult`/`api_error`/`validate_directory_name` erano
// ri-implementati identici qui e in crates/admin-service/src/settings.rs.
use nexus_types::{
    api_error, validate_directory_name_api as validate_directory_name, ApiError, ApiResult,
};

fn map_create_dir_error(error: std::io::Error) -> ApiError {
    let status = match error.kind() {
        std::io::ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
        std::io::ErrorKind::AlreadyExists => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    api_error(status, error.to_string())
}

async fn ensure_required_settings(state: &super::AppState) {
    // Default statici: seed via migrazione 0325 (regola G/H), niente piu' env
    // var (regola G) ne' INSERT ad-hoc all'avvio (regola H). Qui resta solo la
    // parte dinamica (projects_base_root da working dir), il cui punto unico e'
    // in nexus-types (prima era duplicata anche in admin-service).
    nexus_types::ensure_projects_base_root(&state.db).await;
}

/// GET /api/admin/fs/directories — browse server filesystem (admin only)
pub async fn browse_directories(Query(query): Query<FsBrowseQuery>) -> ApiResult {
    let roots = list_root_candidates();
    let target = if let Some(path) = query.path {
        let trimmed = path.trim().to_string();
        if trimmed.is_empty() {
            roots[0].clone()
        } else {
            PathBuf::from(trimmed)
                .canonicalize()
                .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Percorso directory non valido"))?
        }
    } else {
        roots[0].clone()
    };

    if !target.is_dir() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il percorso selezionato non e' una directory",
        ));
    }

    let target_str = target.to_string_lossy().to_string();
    let parent_path = target.parent().and_then(|parent| {
        let parent_str = parent.to_string_lossy().to_string();
        if parent_str == target_str {
            None
        } else {
            Some(parent_str)
        }
    });

    Ok(Json(json!({
        "roots": roots
            .iter()
            .map(|root| root.to_string_lossy().to_string())
            .collect::<Vec<_>>(),
        "currentPath": target_str,
        "parentPath": parent_path,
        "directories": list_directories(&target),
    })))
}

/// POST /api/admin/fs/directories/create — create directory on server filesystem (admin only)
pub async fn create_directory(Json(body): Json<CreateDirectoryRequest>) -> ApiResult {
    let parent = PathBuf::from(body.parent_path.trim())
        .canonicalize()
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Percorso directory non valido"))?;

    if !parent.is_dir() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il percorso parent non e' una directory",
        ));
    }

    let dir_name = validate_directory_name(&body.name)?;
    let target = parent.join(dir_name);
    if target.exists() {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Esiste gia' una directory con questo nome",
        ));
    }

    std::fs::create_dir(&target).map_err(map_create_dir_error)?;

    Ok(Json(json!({
        "ok": true,
        "path": target.to_string_lossy().to_string(),
    })))
}

/// Mascheramento valori secret per la response JSON: prima/ultime 4 lettere + `****`.
/// Punto unico (regola L, S23) per i 2 handler `list_settings` e `list_by_category`
/// che applicavano lo stesso identico mapping.
fn mask_settings(settings: Vec<Setting>) -> Vec<serde_json::Value> {
    settings
        .into_iter()
        .map(|s| {
            let display_value = if s.is_secret && !s.value.is_empty() {
                format!("{}...{}", &s.value[..4.min(s.value.len())], "****")
            } else if s.is_secret {
                String::new()
            } else {
                s.value.clone()
            };
            serde_json::json!({
                "key": s.key,
                "value": display_value,
                "category": s.category,
                "description": s.description,
                "is_secret": s.is_secret,
                "updated_at": s.updated_at,
                "has_value": !s.value.is_empty(),
            })
        })
        .collect()
}

/// GET /api/settings — all settings (secrets are masked)
pub async fn list_settings(State(state): State<super::AppState>) -> Json<serde_json::Value> {
    ensure_required_settings(&state).await;

    // Fix S87: prima .unwrap_or_default() mostrava "0 settings" su DB down,
    // l'admin pensava di dover ripopolare. Ora logga + ritorna lista vuota
    // ma con flag che il chiamante puo' tracciare (regola H pragmatica:
    // signature Json<Value> non puo' diventare ApiResult senza rompere il
    // router; almeno l'errore appare nei log con livello WARN).
    let settings = match sqlx::query_as::<_, Setting>(
        "SELECT key, value, category, description, is_secret, updated_at FROM settings ORDER BY category, key",
    )
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("list_settings: SELECT settings fallito: {}", e);
            Vec::new()
        }
    };

    let masked = mask_settings(settings);

    Json(serde_json::json!({ "settings": masked }))
}

/// GET /api/settings/:category — settings filtered by category
pub async fn list_by_category(
    State(state): State<super::AppState>,
    Path(category): Path<String>,
) -> Json<serde_json::Value> {
    ensure_required_settings(&state).await;

    // Fix S87: vedi list_settings.
    let settings = match sqlx::query_as::<_, Setting>(
        "SELECT key, value, category, description, is_secret, updated_at FROM settings WHERE category = $1 ORDER BY key",
    )
    .bind(&category)
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("list_by_category({}): SELECT fallito: {}", category, e);
            Vec::new()
        }
    };

    let masked = mask_settings(settings);

    Json(serde_json::json!({ "settings": masked }))
}

/// GET /api/admin/settings-categories — categorie distinte con conteggio.
///
/// Fonte per la sidebar admin dinamica (regola L): le voci di navigazione
/// derivano dai DATI, non da una lista hardcoded nel frontend. Prima del
/// fix le categorie fuori dalla lista statica erano invisibili (160 chiavi
/// non amministrabili da UI).
pub async fn list_categories(State(state): State<super::AppState>) -> Json<serde_json::Value> {
    let rows: Vec<(String, i64)> = match sqlx::query_as(
        "SELECT category, count(*) FROM settings WHERE category <> '' GROUP BY category ORDER BY category",
    )
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("list_categories: SELECT fallito: {}", e);
            Vec::new()
        }
    };
    let categories: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(category, count)| serde_json::json!({ "category": category, "count": count }))
        .collect();
    Json(serde_json::json!({ "categories": categories }))
}

/// PUT /api/admin/setting/:key — aggiorna una singola impostazione.
///
/// E' l'endpoint che serve le pagine admin (la route Next.js proxya qui, non su
/// admin-service, che ne ha una copia gemella).
///
/// L'esito e' lo STATUS HTTP (regola M): 200 aggiornata, 404 chiave assente,
/// 500 se il DB rifiuta. Prima rispondeva 200 in ogni caso, con l'esito nel solo
/// campo `status` del body: `fetchJson` decide sullo status e quindi non
/// sollevava, e ogni pagina admin mostrava "salvato" su una scrittura che il DB
/// aveva rifiutato. Il caso non e' teorico: il trigger
/// `trg_settings_guard_protected` (mig 0499) nega gli UPDATE sui setting
/// protetti proprio contando sul fatto che l'errore risalga al client.
///
/// La scrittura delega al punto unico `nexus_auth::update_setting_value`
/// (regola L), che e' anche il posto dove vive il divieto di creare chiavi
/// implicitamente.
pub async fn update_setting(
    State(state): State<super::AppState>,
    Path(key): Path<String>,
    Json(body): Json<UpdateSettingRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    nexus_auth::update_setting_value(&state.db, &key, &body.value)
        .await
        .map_err(|e| api_error(e.status_code(), e.to_string()))?;

    // Notifica tutti i client connessi (evento system-wide)
    let ns = key.split('_').next().unwrap_or("admin").to_string();
    nexus_events::dispatcher::broadcast_all_global(nexus_events::ProjectEvent::SettingChanged {
        namespace: ns,
        key: key.clone(),
    });

    // Invalida cache DLP se è cambiata una chiave di configurazione DLP
    if matches!(
        key.as_str(),
        "dlp_enabled" | "dlp_allow_cloud_tier2" | "dlp_allow_cloud_tier3"
    ) {
        crate::dlp::invalidate_dlp_cache();
    }

    // Propaga il proxy come variabile d'ambiente di processo (effetto immediato
    // per tutti i nuovi client nexus-http, senza riavvio).
    //
    // Qui partiva anche un POST `{NEURAL_CORE_REST_URL}/reload-settings` per
    // avvisare il brain Python di rileggere la configurazione — su
    // `nexus_external_proxy` e su `network_dns_servers`. Il brain e' stato
    // rimosso: quella notifica non arrivava a nessuno (e l'URL veniva da una env
    // var con fallback hardcoded a 127.0.0.1:8001, regola G). Il DNS override
    // era una prerogativa del brain: senza brain non c'e' piu' nessuno da
    // notificare.
    if key.as_str() == "nexus_external_proxy" {
        if body.value.is_empty() {
            std::env::remove_var("NEXUS_PROXY");
        } else {
            std::env::set_var("NEXUS_PROXY", &body.value);
        }
    }

    Ok(Json(serde_json::json!({ "status": "ok", "key": key })))
}

/// Effetti collaterali di una scrittura in blocco: invalidazione della cache DLP.
///
/// Estratti da `bulk_update`, che altrimenti mescola la scrittura, gli effetti
/// e la costruzione della risposta in una funzione sola. Prendeva anche `all_ok`
/// per non notificare il brain su una scrittura parziale: il brain non c'e' piu'.
fn apply_bulk_side_effects(body: &BulkUpdateRequest) {
    // Se sono state cambiate chiavi DLP, invalida la cache in-process.
    let has_dlp_key = body.settings.iter().any(|e| {
        matches!(
            e.key.as_str(),
            "dlp_enabled" | "dlp_allow_cloud_tier2" | "dlp_allow_cloud_tier3"
        )
    });
    if has_dlp_key {
        crate::dlp::invalidate_dlp_cache();
    }

    // Al salvataggio di una `*_api_key` partiva anche un POST
    // `{NEURAL_CORE_REST_URL}/reload-settings` per far rileggere le chiavi al
    // brain Python, che le teneva in memoria. Il brain e' stato rimosso: quella
    // chiamata non arrivava a nessuno e logava "Brain reload-settings failed" a
    // ogni salvataggio di chiave. Chi consuma le API key oggi (gateway, provider)
    // le legge dal DB, quindi non c'e' nessuna cache remota da invalidare.
}

/// PUT /api/admin/settings — aggiorna piu' impostazioni in un colpo.
///
/// Aggiorna, non crea: come il PUT singolo, delega al punto unico
/// `nexus_auth::update_setting_value` (regola L). Prima faceva un `INSERT ...
/// VALUES ($1, $2, 'custom', '', FALSE) ON CONFLICT (key) DO UPDATE`, cioe' il
/// secondo vettore per le stesse scritture inefficaci in categoria 'custom'. Le
/// chiavi che i chiamanti scrivono (routing, gerarchia provider, budget) sono
/// tutte seedate da migrazione.
///
/// L'esito e' lo STATUS HTTP (regola M): 200 se tutte le chiavi sono passate,
/// 500 se anche una sola e' stata rifiutata. Prima rispondeva 200 in ogni caso e
/// l'esito viveva nel solo `status`/`errors` del body: `saveRouting` in
/// routing-config leggeva solo `res.ok` e mostrava "Salvato" con il DB
/// invariato. Il campo `error` porta il messaggio pronto per il display; il
/// dettaglio per chiave resta in `errors`.
pub async fn bulk_update(
    State(state): State<super::AppState>,
    Json(body): Json<BulkUpdateRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    ensure_required_settings(&state).await;

    let mut updated = 0;
    let mut errors = Vec::new();

    for entry in &body.settings {
        match nexus_auth::update_setting_value(&state.db, &entry.key, &entry.value).await {
            Ok(()) => {
                updated += 1;
                // Notifica per ogni setting aggiornato
                let ns = entry.key.split('_').next().unwrap_or("admin").to_string();
                nexus_events::dispatcher::broadcast_all_global(
                    nexus_events::ProjectEvent::SettingChanged {
                        namespace: ns,
                        key: entry.key.clone(),
                    },
                );
            }
            Err(e) => errors.push(format!("{}: {}", entry.key, e)),
        }
    }

    apply_bulk_side_effects(&body);

    if errors.is_empty() {
        return (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "ok", "updated": updated, "errors": [] })),
        );
    }

    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({
            "status": "partial",
            "updated": updated,
            "errors": errors,
            "error": format!(
                "{} chiave/i su {} non salvate: {}",
                errors.len(),
                body.settings.len(),
                errors.join(" | ")
            ),
        })),
    )
}

/// GET /internal/settings/:key — valore non mascherato delle chiavi NON segrete.
///
/// La rotta e' montata FUORI dal layer di auth (`routes/public.rs`) e il
/// servizio ascolta su `0.0.0.0`: qualunque client che raggiunga la porta la
/// interroga senza credenziali. Prima leggeva il valore RAW di qualsiasi
/// chiave, quindi restituiva in chiaro `jwt_secret` e le API key dei provider —
/// e con la chiave di firma si conia un token di amministratore. Ora il
/// predicato "esponibile senza auth" sta nel punto unico
/// `nexus_auth::get_setting_public` (regola L), che rifiuta `is_secret = TRUE`.
pub async fn get_raw_value(
    State(state): State<super::AppState>,
    Path(key): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    match nexus_auth::get_setting_public(&state.db, &key).await {
        Ok(nexus_auth::PublicSettingRead::Value(value)) => (
            StatusCode::OK,
            Json(serde_json::json!({ "key": key, "value": value })),
        ),
        // 403 e non 404: la chiave esiste, ma non e' leggibile da qui. Chi ha
        // bisogno di un segreto passa dal percorso autenticato.
        Ok(nexus_auth::PublicSettingRead::Redacted) => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "key": key,
                "error": "chiave segreta: non leggibile da una rotta senza autenticazione",
            })),
        ),
        Ok(nexus_auth::PublicSettingRead::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "key": key, "error": "chiave inesistente" })),
        ),
        Err(e) => {
            tracing::warn!("get_raw_value({}): lettura fallita: {}", key, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "key": key, "error": "lettura setting fallita" })),
            )
        }
    }
}

/// Lettura setting: punto unico in nexus-auth (regola L / ADR 0026).
/// Re-export con la firma storica (Result, valore raw, propaga l'errore DB).
pub use nexus_auth::get_setting_checked as get_setting;

/// Punto unico (regola L) per risolvere l'URL REST di Qdrant.
///
/// Ordine: setting DB `qdrant_url` -> env `QDRANT_URL` -> default REST
/// `http://localhost:6333`. La 6333 e' la porta REST usata da TUTTI i client
/// Nexus (RAG, doc-service, health probe); la 6334 e' gRPC (protocollo diverso).
/// Prima il watchdog leggeva solo l'env mentre i client leggevano il DB: due
/// fonti divergenti per la stessa decisione (un env errato a 6334 dava
/// `qdrant=False` pur con il setting DB corretto a 6333). Ora c'e' una sola
/// risoluzione condivisa.
pub async fn resolve_qdrant_url(db: &sqlx::PgPool) -> String {
    let raw = nexus_auth::get_setting(db, "qdrant_url")
        .await
        .or_else(|| std::env::var("QDRANT_URL").ok())
        .unwrap_or_else(|| "http://localhost:6333".to_string());
    disambigua_loopback(&raw)
}

/// Sostituisce l'host `localhost` con `127.0.0.1` in un URL di servizio LOCALE.
///
/// `localhost` non e' un indirizzo, e' un nome con DUE risposte: su Windows la
/// risoluzione restituisce `::1` (IPv6) prima di `127.0.0.1`. Qdrant ascolta su
/// `0.0.0.0:6333`, cioe' solo IPv4: il client tentava dunque `::1:6333`, restava
/// in SynSent fino allo scadere del timeout TCP e mcp-core si appendeva
/// nell'avvio per minuti. Effetto a catena misurato il 2026-07-23: web-ide
/// partiva, non riusciva a raggiungere mcp-core (`ECONNREFUSED` ripetuti) e
/// crashava.
///
/// Per un servizio sulla macchina locale `127.0.0.1` e' sempre corretto e non ha
/// quell'ambiguita'. Gli host remoti e gli indirizzi gia' espliciti non vengono
/// toccati: si disambigua solo il nome che ha due risposte possibili.
///
/// Sostituisce il workaround che viveva nel `.env` (regola H: la causa sta nel
/// codice che risolve l'host, non nella configurazione di una macchina).
pub fn disambigua_loopback(url: &str) -> String {
    url.replace("://localhost:", "://127.0.0.1:")
        .replace("://localhost/", "://127.0.0.1/")
}

#[cfg(test)]
mod tests {
    use super::disambigua_loopback;

    /// `localhost` viene disambiguato, il resto no.
    ///
    /// Il difetto che cattura: con `http://localhost:6333` il client tentava
    /// `::1` (IPv6) mentre Qdrant ascolta su IPv4, restava in SynSent e mcp-core
    /// si appendeva nell'avvio per minuti, facendo crashare web-ide a catena.
    #[test]
    fn loopback_disambiguato_solo_dove_serve() {
        // Il caso reale: e' l'unico host con due risposte possibili.
        assert_eq!(
            disambigua_loopback("http://localhost:6333"),
            "http://127.0.0.1:6333"
        );
        assert_eq!(
            disambigua_loopback("http://localhost:6333/collections"),
            "http://127.0.0.1:6333/collections"
        );

        // Gia' esplicito: nulla da disambiguare.
        assert_eq!(
            disambigua_loopback("http://127.0.0.1:6333"),
            "http://127.0.0.1:6333"
        );
        // Un host REMOTO non si tocca: la sua risoluzione non e' ambigua per noi.
        assert_eq!(
            disambigua_loopback("http://qdrant.interno:6333"),
            "http://qdrant.interno:6333"
        );
        // `localhost` come sottostringa di un altro nome non e' l'host loopback.
        assert_eq!(
            disambigua_loopback("http://localhost.example.com:6333"),
            "http://localhost.example.com:6333"
        );
    }
}
