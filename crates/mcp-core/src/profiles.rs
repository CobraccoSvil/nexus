//! User-owned and system chat profiles.
//!
//! GET    /api/profiles                  → lista profili utente + profili di sistema
//! POST   /api/profiles                  → crea profilo utente
//! PUT    /api/profiles/:id              → aggiorna profilo utente (non sistema)
//! DELETE /api/profiles/:id              → elimina profilo utente (non sistema)
//! POST   /api/profiles/:id/default      → imposta come default (solo profili utente)
//! POST   /api/profiles/:id/fork         → crea copia personale di un profilo di sistema
//!
//! Admin:
//! GET    /api/admin/profiles            → lista tutti i profili di sistema
//! POST   /api/admin/profiles            → crea profilo di sistema
//! PUT    /api/admin/profiles/:id        → aggiorna qualsiasi profilo (inclusi sistema)
//! DELETE /api/admin/profiles/:id        → elimina profilo di sistema

use axum::{
    extract::{Extension, Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    auth::Claims,
    chat_learning::{api_error, parse_user_id, ApiError, ApiResult},
    AppState,
};

// ── Request structs ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProfileRequest {
    pub name: String,
    pub description: Option<String>,
    pub avatar_emoji: Option<String>,
    pub system_prompt: Option<String>,
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub default_automation: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProfileRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub avatar_emoji: Option<String>,
    pub system_prompt: Option<String>,
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub default_automation: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetProfileMcpServersRequest {
    pub mcp_server_ids: Vec<String>,
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Valori trimmati e normalizzati di un `UpdateProfileRequest`: chi e' empty
/// dopo il trim diventa `None`. Punto unico (regola L / ADR 0026, step S15) per
/// la logica di binding usata sia in `update_profile` (user-scoped) sia in
/// `admin_update_profile` (admin-scoped): prima i 7 blocchi `.bind(...)` con
/// stessa `as_deref().map(trim).filter(non_empty)` erano duplicati pari-pari.
struct ProfileUpdateBinds<'a> {
    name: Option<&'a str>,
    description: Option<&'a str>,
    avatar_emoji: Option<&'a str>,
    system_prompt: Option<&'a str>,
    default_provider: Option<&'a str>,
    default_model: Option<&'a str>,
    default_automation: Option<&'a str>,
}

/// Valori trimmati di un INSERT profilo (sia user che system): name + 6 campi.
/// Punto unico (regola L / S64) per il pattern di binding duplicato fra
/// `create_profile` e `admin_create_profile`.
struct ProfileInsertBinds<'a> {
    description: Option<&'a str>,
    avatar_emoji: &'a str,
    system_prompt: &'a str,
    default_provider: Option<&'a str>,
    default_model: Option<&'a str>,
    default_automation: Option<&'a str>,
}

impl<'a> ProfileInsertBinds<'a> {
    /// Costruisce i bind a partire dai 6 campi opzionali grezzi dell'INSERT
    /// (usato sia da CreateProfileRequest che da admin CreateProfileRequest:
    /// hanno gli stessi nomi/tipi sui 6 campi `Option<String>`).
    fn from_fields(
        description: Option<&'a String>,
        avatar_emoji: Option<&'a String>,
        system_prompt: Option<&'a String>,
        default_provider: Option<&'a String>,
        default_model: Option<&'a String>,
        default_automation: Option<&'a String>,
    ) -> Self {
        let non_empty = |s: &'a str| -> Option<&'a str> {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        };
        Self {
            description: description.map(|s| s.as_str().trim()),
            avatar_emoji: avatar_emoji.map(|s| s.as_str()).unwrap_or("\u{1F916}").trim(),
            system_prompt: system_prompt.map(|s| s.as_str()).unwrap_or("").trim(),
            default_provider: default_provider.and_then(|s| non_empty(s.as_str())),
            default_model: default_model.and_then(|s| non_empty(s.as_str())),
            default_automation: default_automation.and_then(|s| non_empty(s.as_str())),
        }
    }
}

/// Mappa errore sqlx unique-violation -> 409 CONFLICT con messaggio
/// personalizzato; altri errori -> 500. Punto unico per i blocchi
/// `if e.to_string().contains("unique") ...` ripetuti nei create profile.
fn map_profile_insert_error(e: sqlx::Error, conflict_msg: &'static str) -> ApiError {
    if e.to_string().contains("unique") || e.to_string().contains("duplicate") {
        api_error(StatusCode::CONFLICT, conflict_msg)
    } else {
        api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}

impl<'a> ProfileUpdateBinds<'a> {
    fn from_body(body: &'a UpdateProfileRequest) -> Self {
        let non_empty = |s: &'a str| -> Option<&'a str> {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        };
        Self {
            name: body.name.as_deref().and_then(non_empty),
            description: body.description.as_deref().map(str::trim),
            avatar_emoji: body.avatar_emoji.as_deref().and_then(non_empty),
            system_prompt: body.system_prompt.as_deref().map(str::trim),
            default_provider: body.default_provider.as_deref().and_then(non_empty),
            default_model: body.default_model.as_deref().and_then(non_empty),
            default_automation: body.default_automation.as_deref().and_then(non_empty),
        }
    }
}

fn row_to_json(r: &sqlx::postgres::PgRow) -> Value {
    json!({
        "id": r.try_get::<Uuid, _>("id").ok().map(|v| v.to_string()),
        "userId": r.try_get::<Option<Uuid>, _>("user_id").ok().flatten().map(|v| v.to_string()),
        "name": r.try_get::<String, _>("name").unwrap_or_default(),
        "description": r.try_get::<Option<String>, _>("description").unwrap_or(None),
        "avatarEmoji": r.try_get::<String, _>("avatar_emoji").unwrap_or_else(|_| "\u{1F916}".to_string()),
        "systemPrompt": r.try_get::<String, _>("system_prompt").unwrap_or_default(),
        "defaultProvider": r.try_get::<Option<String>, _>("default_provider").unwrap_or(None),
        "defaultModel": r.try_get::<Option<String>, _>("default_model").unwrap_or(None),
        "defaultAutomation": r.try_get::<Option<String>, _>("default_automation").unwrap_or(None),
        "isDefault": r.try_get::<bool, _>("is_default").unwrap_or(false),
        "isSystem": r.try_get::<bool, _>("is_system").unwrap_or(false),
        "sourceTemplateKey": r.try_get::<Option<String>, _>("source_template_key").unwrap_or(None),
        "createdAt": r.try_get::<DateTime<Utc>, _>("created_at").ok().map(|v| v.to_rfc3339()),
        "updatedAt": r.try_get::<DateTime<Utc>, _>("updated_at").ok().map(|v| v.to_rfc3339()),
    })
}

// ── Handlers ───────────────────────────────────────────────────────────────

/// GET /api/profiles — lista profili utente + profili di sistema (condivisi)
pub async fn list_profiles(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let rows = sqlx::query(
        r#"
        SELECT * FROM user_profiles
        WHERE user_id = $1 OR is_system = TRUE
        ORDER BY is_system ASC, is_default DESC, created_at ASC
        "#,
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let profiles: Vec<Value> = rows.iter().map(row_to_json).collect();
    Ok(Json(json!({ "profiles": profiles })))
}

/// POST /api/profiles — crea profilo utente
pub async fn create_profile(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<CreateProfileRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il nome del profilo e' obbligatorio",
        ));
    }
    let binds = ProfileInsertBinds::from_fields(
        body.description.as_ref(),
        body.avatar_emoji.as_ref(),
        body.system_prompt.as_ref(),
        body.default_provider.as_ref(),
        body.default_model.as_ref(),
        body.default_automation.as_ref(),
    );
    let row = sqlx::query(
        r#"
        INSERT INTO user_profiles
          (user_id, name, description, avatar_emoji, system_prompt,
           default_provider, default_model, default_automation)
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
        RETURNING *
        "#,
    )
    .bind(user_id)
    .bind(&name)
    .bind(binds.description)
    .bind(binds.avatar_emoji)
    .bind(binds.system_prompt)
    .bind(binds.default_provider)
    .bind(binds.default_model)
    .bind(binds.default_automation)
    .fetch_one(&state.db)
    .await
    .map_err(|e| map_profile_insert_error(e, "Un profilo con questo nome esiste gia'"))?;

    Ok(Json(row_to_json(&row)))
}

/// PUT /api/profiles/:id — aggiorna profilo (solo profili utente, non di sistema)
pub async fn update_profile(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(profile_id): AxumPath<String>,
    Json(body): Json<UpdateProfileRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let profile_uuid = Uuid::parse_str(&profile_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Profile id non valido"))?;

    let binds = ProfileUpdateBinds::from_body(&body);
    let row = sqlx::query(
        r#"
        UPDATE user_profiles SET
            name               = COALESCE($3, name),
            description        = COALESCE($4, description),
            avatar_emoji       = COALESCE($5, avatar_emoji),
            system_prompt      = COALESCE($6, system_prompt),
            default_provider   = $7,
            default_model      = $8,
            default_automation = $9,
            updated_at         = NOW()
        WHERE id = $1 AND user_id = $2 AND is_system = FALSE
        RETURNING *
        "#,
    )
    .bind(profile_uuid)
    .bind(user_id)
    .bind(binds.name)
    .bind(binds.description)
    .bind(binds.avatar_emoji)
    .bind(binds.system_prompt)
    .bind(binds.default_provider)
    .bind(binds.default_model)
    .bind(binds.default_automation)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| {
        api_error(
            StatusCode::NOT_FOUND,
            "Profilo non trovato o non modificabile",
        )
    })?;

    Ok(Json(row_to_json(&row)))
}

/// DELETE /api/profiles/:id — elimina profilo (solo profili utente, non di sistema)
pub async fn delete_profile(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(profile_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let profile_uuid = Uuid::parse_str(&profile_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Profile id non valido"))?;

    let deleted = sqlx::query(
        "DELETE FROM user_profiles WHERE id = $1 AND user_id = $2 AND is_system = FALSE RETURNING id",
    )
    .bind(profile_uuid)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if deleted.is_none() {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            "Profilo non trovato o non eliminabile",
        ));
    }
    Ok(Json(json!({ "ok": true })))
}

/// POST /api/profiles/:id/default — imposta come default (solo profili utente)
pub async fn set_default_profile(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(profile_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let profile_uuid = Uuid::parse_str(&profile_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Profile id non valido"))?;

    // Verifica che esista ed appartenga all'utente (o sia di sistema)
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM user_profiles WHERE id = $1 AND (user_id = $2 OR is_system = TRUE))",
    )
    .bind(profile_uuid)
    .bind(user_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(false);

    if !exists {
        return Err(api_error(StatusCode::NOT_FOUND, "Profilo non trovato"));
    }

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    sqlx::query("UPDATE user_profiles SET is_default = FALSE WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    sqlx::query("UPDATE user_profiles SET is_default = TRUE, updated_at = NOW() WHERE id = $1")
        .bind(profile_uuid)
        .execute(&mut *tx)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    tx.commit()
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "ok": true })))
}

/// POST /api/profiles/:id/fork — crea una copia personale di un profilo di sistema
pub async fn fork_profile(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(profile_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let profile_uuid = Uuid::parse_str(&profile_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Profile id non valido"))?;

    let source = sqlx::query(
        "SELECT name, description, avatar_emoji, system_prompt, default_provider, default_model, default_automation
         FROM user_profiles WHERE id = $1",
    )
    .bind(profile_uuid)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Profilo sorgente non trovato"))?;

    let base_name: String = source.try_get("name").unwrap_or_default();
    let fork_name = format!("{} (copia)", base_name);

    let row = sqlx::query(
        r#"
        INSERT INTO user_profiles
          (user_id, name, description, avatar_emoji, system_prompt,
           default_provider, default_model, default_automation, is_system)
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8, FALSE)
        RETURNING *
        "#,
    )
    .bind(user_id)
    .bind(&fork_name)
    .bind(
        source
            .try_get::<Option<String>, _>("description")
            .unwrap_or(None),
    )
    .bind(
        source
            .try_get::<String, _>("avatar_emoji")
            .unwrap_or_else(|_| "\u{1F916}".to_string()),
    )
    .bind(
        source
            .try_get::<String, _>("system_prompt")
            .unwrap_or_default(),
    )
    .bind(
        source
            .try_get::<Option<String>, _>("default_provider")
            .unwrap_or(None),
    )
    .bind(
        source
            .try_get::<Option<String>, _>("default_model")
            .unwrap_or(None),
    )
    .bind(
        source
            .try_get::<Option<String>, _>("default_automation")
            .unwrap_or(None),
    )
    .fetch_one(&state.db)
    .await
    .map_err(|e| {
        if e.to_string().contains("unique") || e.to_string().contains("duplicate") {
            api_error(StatusCode::CONFLICT, "Hai gia' una copia di questo profilo")
        } else {
            api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
    })?;

    Ok(Json(row_to_json(&row)))
}

// ── Internal helper per chat_messages.rs ──────────────────────────────────

/// Carica il system_prompt del profilo selezionato.
/// Cerca prima nei profili utente, poi nei profili di sistema.
/// Se profile_id == "auto", sceglie automaticamente il profilo più adatto.
pub async fn fetch_profile_context(
    db: &sqlx::PgPool,
    user_id: Uuid,
    profile_id: &str,
    request_text: &str,
) -> (String, Option<String>, Option<String>, Option<String>) {
    if profile_id == "default" || profile_id.trim().is_empty() {
        return (String::new(), None, None, None);
    }

    if profile_id == "auto" {
        return auto_select_profile(db, user_id, request_text).await;
    }

    let Ok(uuid) = Uuid::parse_str(profile_id) else {
        return (String::new(), None, None, None);
    };

    // Cerca profilo utente O profilo di sistema
    let row = sqlx::query(
        "SELECT system_prompt, default_provider, default_model, default_automation, avatar_emoji, name
         FROM user_profiles WHERE id = $1 AND (user_id = $2 OR is_system = TRUE)",
    )
    .bind(uuid)
    .bind(user_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    match row {
        None => (String::new(), None, None, None),
        Some(r) => build_profile_result(r),
    }
}

/// Sceglie automaticamente il profilo più adatto analizzando il testo della richiesta.
/// Considera sia profili utente che profili di sistema.
async fn auto_select_profile(
    db: &sqlx::PgPool,
    user_id: Uuid,
    request_text: &str,
) -> (String, Option<String>, Option<String>, Option<String>) {
    let rows = sqlx::query(
        r#"
        SELECT system_prompt, default_provider, default_model, default_automation,
               avatar_emoji, name, description
        FROM user_profiles
        WHERE user_id = $1 OR is_system = TRUE
        ORDER BY is_default DESC, is_system ASC, created_at ASC
        "#,
    )
    .bind(user_id)
    .fetch_all(db)
    .await
    .unwrap_or_else(|e| {
        tracing::warn!("auto_select_profile: SELECT user_profiles fallita: {e}");
        Vec::new()
    });

    if rows.is_empty() {
        return (String::new(), None, None, None);
    }

    let req_lower = request_text.to_lowercase();

    let mut best_score: usize = 0;
    let mut best_idx: Option<usize> = None;

    for (idx, row) in rows.iter().enumerate() {
        let name = row
            .try_get::<String, _>("name")
            .unwrap_or_default()
            .to_lowercase();
        let description = row
            .try_get::<Option<String>, _>("description")
            .unwrap_or(None)
            .unwrap_or_default()
            .to_lowercase();
        let prompt = row
            .try_get::<String, _>("system_prompt")
            .unwrap_or_default()
            .to_lowercase();

        let candidate_text = format!(
            "{} {} {}",
            name,
            description,
            &prompt[..prompt.len().min(300)]
        );
        let keywords: Vec<&str> = candidate_text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 3)
            .collect();

        let score = keywords
            .iter()
            .filter(|&&kw| req_lower.contains(kw))
            .count();

        if score > best_score {
            best_score = score;
            best_idx = Some(idx);
        }
    }

    match best_idx {
        // idx proviene da enumerate() su rows, quindi nth(idx) e' sempre Some.
        // Difensivo: se per qualche motivo non lo fosse, ritorniamo default.
        Some(idx) if best_score > 0 => rows
            .into_iter()
            .nth(idx)
            .map(build_profile_result)
            .unwrap_or_else(|| (String::new(), None, None, None)),
        _ => (String::new(), None, None, None),
    }
}

fn build_profile_result(
    r: sqlx::postgres::PgRow,
) -> (String, Option<String>, Option<String>, Option<String>) {
    let prompt = r.try_get::<String, _>("system_prompt").unwrap_or_default();
    let provider = r
        .try_get::<Option<String>, _>("default_provider")
        .unwrap_or(None);
    let model = r
        .try_get::<Option<String>, _>("default_model")
        .unwrap_or(None);
    let automation = r
        .try_get::<Option<String>, _>("default_automation")
        .unwrap_or(None);
    let emoji = r
        .try_get::<String, _>("avatar_emoji")
        .unwrap_or_else(|_| "\u{1F916}".to_string());
    let name = r.try_get::<String, _>("name").unwrap_or_default();
    let header = if prompt.is_empty() {
        String::new()
    } else {
        format!(
            "=== PROFILO: {} {} ===\n{}\n=== FINE PROFILO ===\n\n",
            emoji, name, prompt
        )
    };
    (header, provider, model, automation)
}

// ── Admin handlers ─────────────────────────────────────────────────────────

/// GET /api/admin/profiles — lista tutti i profili di sistema
pub async fn admin_list_profiles(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
) -> ApiResult {
    let rows = sqlx::query("SELECT * FROM user_profiles WHERE is_system = TRUE ORDER BY name ASC")
        .fetch_all(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let profiles: Vec<Value> = rows.iter().map(row_to_json).collect();
    Ok(Json(json!({ "profiles": profiles })))
}

/// POST /api/admin/profiles — crea nuovo profilo di sistema
pub async fn admin_create_profile(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    Json(body): Json<CreateProfileRequest>,
) -> ApiResult {
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il nome del profilo e' obbligatorio",
        ));
    }
    let binds = ProfileInsertBinds::from_fields(
        body.description.as_ref(),
        body.avatar_emoji.as_ref(),
        body.system_prompt.as_ref(),
        body.default_provider.as_ref(),
        body.default_model.as_ref(),
        body.default_automation.as_ref(),
    );
    let row = sqlx::query(
        r#"
        INSERT INTO user_profiles
          (user_id, name, description, avatar_emoji, system_prompt,
           default_provider, default_model, default_automation, is_system)
        VALUES (NULL,$1,$2,$3,$4,$5,$6,$7, TRUE)
        RETURNING *
        "#,
    )
    .bind(&name)
    .bind(binds.description)
    .bind(binds.avatar_emoji)
    .bind(binds.system_prompt)
    .bind(binds.default_provider)
    .bind(binds.default_model)
    .bind(binds.default_automation)
    .fetch_one(&state.db)
    .await
    .map_err(|e| map_profile_insert_error(e, "Un profilo di sistema con questo nome esiste gia'"))?;

    Ok(Json(row_to_json(&row)))
}

/// PUT /api/admin/profiles/:id — aggiorna un profilo di sistema (o qualsiasi)
pub async fn admin_update_profile(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    AxumPath(profile_id): AxumPath<String>,
    Json(body): Json<UpdateProfileRequest>,
) -> ApiResult {
    let profile_uuid = Uuid::parse_str(&profile_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Profile id non valido"))?;

    let binds = ProfileUpdateBinds::from_body(&body);
    let row = sqlx::query(
        r#"
        UPDATE user_profiles SET
            name               = COALESCE($2, name),
            description        = COALESCE($3, description),
            avatar_emoji       = COALESCE($4, avatar_emoji),
            system_prompt      = COALESCE($5, system_prompt),
            default_provider   = $6,
            default_model      = $7,
            default_automation = $8,
            updated_at         = NOW()
        WHERE id = $1
        RETURNING *
        "#,
    )
    .bind(profile_uuid)
    .bind(binds.name)
    .bind(binds.description)
    .bind(binds.avatar_emoji)
    .bind(binds.system_prompt)
    .bind(binds.default_provider)
    .bind(binds.default_model)
    .bind(binds.default_automation)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Profilo non trovato"))?;

    Ok(Json(row_to_json(&row)))
}

/// DELETE /api/admin/profiles/:id — elimina un profilo di sistema
pub async fn admin_delete_profile(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    AxumPath(profile_id): AxumPath<String>,
) -> ApiResult {
    let profile_uuid = Uuid::parse_str(&profile_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Profile id non valido"))?;

    let deleted =
        sqlx::query("DELETE FROM user_profiles WHERE id = $1 AND is_system = TRUE RETURNING id")
            .bind(profile_uuid)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if deleted.is_none() {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            "Profilo di sistema non trovato",
        ));
    }
    Ok(Json(json!({ "ok": true })))
}

/// GET /api/admin/user-profiles — lista profili custom degli utenti (read-only per admin)
pub async fn admin_list_user_profiles(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
) -> ApiResult {
    let rows = sqlx::query(
        r#"
        SELECT up.*, u.email as user_email
        FROM user_profiles up
        LEFT JOIN users u ON u.id = up.user_id
        WHERE up.is_system = FALSE AND up.user_id IS NOT NULL
        ORDER BY u.email ASC, up.name ASC
        "#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let profiles: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut v = row_to_json(r);
            if let Some(obj) = v.as_object_mut() {
                obj.insert(
                    "userEmail".to_string(),
                    json!(r.try_get::<Option<String>, _>("user_email").unwrap_or(None)),
                );
            }
            v
        })
        .collect();
    Ok(Json(json!({ "profiles": profiles })))
}

/// GET /api/admin/profiles/:id/mcp-servers — lista server MCP associati al profilo
pub async fn admin_get_profile_mcp_servers(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    AxumPath(profile_id): AxumPath<String>,
) -> ApiResult {
    let profile_uuid = Uuid::parse_str(&profile_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Profile id non valido"))?;

    let rows = sqlx::query(
        r#"
        SELECT s.id, s.name, s.description, s.transport, s.scope, s.enabled
        FROM mcp_servers s
        JOIN profile_mcp_servers pm ON pm.mcp_server_id = s.id
        WHERE pm.profile_id = $1
        ORDER BY s.name ASC
        "#,
    )
    .bind(profile_uuid)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let servers: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<Uuid, _>("id").ok().map(|v| v.to_string()),
                "name": r.try_get::<String, _>("name").unwrap_or_default(),
                "description": r.try_get::<Option<String>, _>("description").unwrap_or(None),
                "transport": r.try_get::<String, _>("transport").unwrap_or_default(),
                "scope": r.try_get::<String, _>("scope").unwrap_or_default(),
                "enabled": r.try_get::<bool, _>("enabled").unwrap_or(true),
            })
        })
        .collect();

    Ok(Json(json!({ "servers": servers })))
}

/// PUT /api/admin/profiles/:id/mcp-servers — sostituisce i server MCP del profilo
pub async fn admin_set_profile_mcp_servers(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    AxumPath(profile_id): AxumPath<String>,
    Json(body): Json<SetProfileMcpServersRequest>,
) -> ApiResult {
    let profile_uuid = Uuid::parse_str(&profile_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Profile id non valido"))?;

    let server_uuids: Vec<Uuid> = body
        .mcp_server_ids
        .iter()
        .filter_map(|s| Uuid::parse_str(s).ok())
        .collect();

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    sqlx::query("DELETE FROM profile_mcp_servers WHERE profile_id = $1")
        .bind(profile_uuid)
        .execute(&mut *tx)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    for server_uuid in &server_uuids {
        sqlx::query(
            "INSERT INTO profile_mcp_servers (profile_id, mcp_server_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(profile_uuid)
        .bind(server_uuid)
        .execute(&mut *tx)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    tx.commit()
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "ok": true, "count": server_uuids.len() })))
}

/// GET /api/admin/global-mcp-servers — lista server MCP globali disponibili per i profili
pub async fn admin_list_global_mcp_servers(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
) -> ApiResult {
    let rows = sqlx::query(
        r#"
        SELECT id, name, description, transport, scope, enabled
        FROM mcp_servers
        WHERE scope = 'global'
        ORDER BY name ASC
        "#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let servers: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<Uuid, _>("id").ok().map(|v| v.to_string()),
                "name": r.try_get::<String, _>("name").unwrap_or_default(),
                "description": r.try_get::<Option<String>, _>("description").unwrap_or(None),
                "transport": r.try_get::<String, _>("transport").unwrap_or_default(),
                "scope": r.try_get::<String, _>("scope").unwrap_or_default(),
                "enabled": r.try_get::<bool, _>("enabled").unwrap_or(true),
            })
        })
        .collect();

    Ok(Json(json!({ "servers": servers })))
}
