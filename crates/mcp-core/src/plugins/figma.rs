use super::*;

pub async fn get_figma_oauth_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult {
    if claims.role != "admin" {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Solo admin può verificare stato OAuth Figma",
        ));
    }

    let has_client_id = get_setting(&state.db, "figma_client_id")
        .await
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let has_client_secret = get_setting(&state.db, "figma_client_secret")
        .await
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let access_token = get_setting(&state.db, "figma_oauth_token")
        .await
        .unwrap_or_default();
    let token_scope = get_setting(&state.db, "figma_token_scope")
        .await
        .unwrap_or_default();
    let token_expires_at = get_setting(&state.db, "figma_token_expires_at")
        .await
        .unwrap_or_default();
    let last_error = get_setting(&state.db, "figma_last_oauth_error")
        .await
        .unwrap_or_default();
    let prefer_stdio = resolve_bool_setting(&state.db, "figma_mcp_prefer_stdio", true).await;

    Ok(Json(json!({
        "configured": has_client_id && has_client_secret,
        "hasClientId": has_client_id,
        "hasClientSecret": has_client_secret,
        "hasAccessToken": !access_token.trim().is_empty(),
        "tokenType": if is_figma_pat(&access_token) { "pat" } else { "oauth_or_unknown" },
        "tokenScope": token_scope,
        "tokenExpiresAt": token_expires_at,
        "lastError": last_error,
        "redirectUri": get_setting(&state.db, "figma_oauth_redirect_uri").await.unwrap_or_else(figma_oauth_redirect_uri),
        "preferStdioFallback": prefer_stdio
    })))
}

pub async fn start_figma_oauth(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<FigmaOAuthStartRequest>,
) -> ApiResult {
    if claims.role != "admin" {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Solo admin può avviare OAuth Figma",
        ));
    }
    let user_id = parse_user_id(&claims)?;
    let (client_id, _client_secret, redirect_uri) =
        figma_oauth_client_credentials(&state.db).await?;
    let discovery = fetch_figma_oauth_discovery()
        .await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, error))?;

    let jwt_secret = get_or_create_jwt_secret(&state.db)
        .await
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let signed_state = encode(
        &Header::default(),
        &FigmaOAuthStateClaims {
            user_id: user_id.to_string(),
            return_to: sanitize_return_to(body.return_to.as_deref()),
            exp: (Utc::now() + Duration::minutes(20)).timestamp() as usize,
        },
        &EncodingKey::from_secret(jwt_secret.as_bytes()),
    )
    .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;

    let url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&resource={}",
        discovery.authorization_endpoint,
        urlencoding::encode(&client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode("mcp:connect"),
        urlencoding::encode(&signed_state),
        urlencoding::encode("https://mcp.figma.com")
    );

    Ok(Json(json!({
        "url": url,
        "redirectUri": redirect_uri
    })))
}

pub async fn figma_oauth_callback(
    State(state): State<AppState>,
    Query(query): Query<FigmaOAuthCallbackQuery>,
) -> Response {
    let mut return_to = FIGMA_DEFAULT_RETURN_TO.to_string();

    let Some(raw_state) = query.state.as_deref() else {
        return redirect_with_status(&return_to, "error", Some("State OAuth mancante"));
    };

    let jwt_secret = match get_or_create_jwt_secret(&state.db).await {
        Ok(secret) => secret,
        Err(error) => {
            return redirect_with_status(
                &return_to,
                "error",
                Some(&format!("Errore interno JWT: {error}")),
            );
        }
    };

    let decoded = decode::<FigmaOAuthStateClaims>(
        raw_state,
        &DecodingKey::from_secret(jwt_secret.as_bytes()),
        &Validation::default(),
    );
    let state_claims = match decoded {
        Ok(data) => data.claims,
        Err(_) => {
            return redirect_with_status(
                &return_to,
                "error",
                Some("State OAuth non valido o scaduto"),
            );
        }
    };
    return_to = sanitize_return_to(Some(&state_claims.return_to));

    if let Some(error) = query.error.as_deref() {
        let msg = query
            .error_description
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(error);
        store_figma_oauth_error(&state.db, msg).await;
        return redirect_with_status(&return_to, "error", Some(msg));
    }

    let user_id = match Uuid::parse_str(&state_claims.user_id) {
        Ok(value) => value,
        Err(_) => {
            return redirect_with_status(
                &return_to,
                "error",
                Some("Utente OAuth Figma non valido"),
            );
        }
    };

    let role = sqlx::query_scalar::<_, String>("SELECT role FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    if role != "admin" {
        return redirect_with_status(
            &return_to,
            "error",
            Some("Solo admin può completare OAuth Figma"),
        );
    }

    let Some(code) = query.code.as_deref().filter(|value| !value.trim().is_empty()) else {
        return redirect_with_status(&return_to, "error", Some("Code OAuth mancante"));
    };

    let (client_id, client_secret, redirect_uri) =
        match figma_oauth_client_credentials(&state.db).await {
            Ok(values) => values,
            Err((_, payload)) => {
                let msg = payload
                    .0
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("Configurazione OAuth Figma incompleta")
                    .to_string();
                store_figma_oauth_error(&state.db, &msg).await;
                return redirect_with_status(&return_to, "error", Some(&msg));
            }
        };

    let discovery = match fetch_figma_oauth_discovery().await {
        Ok(value) => value,
        Err(error) => {
            store_figma_oauth_error(&state.db, &error).await;
            return redirect_with_status(&return_to, "error", Some(&error));
        }
    };

    let client = Client::builder()
        .connect_timeout(std::time::Duration::from_secs(8))
        .timeout(std::time::Duration::from_secs(25))
        .build()
        .unwrap_or_else(|_| Client::new());

    let token_response = match client
        .post(&discovery.token_endpoint)
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
        ])
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(error) => {
            let msg = format!("Token exchange OAuth Figma fallito: {error}");
            store_figma_oauth_error(&state.db, &msg).await;
            return redirect_with_status(&return_to, "error", Some(&msg));
        }
    };

    let http_status = token_response.status();
    let token_payload = match token_response.json::<FigmaOAuthTokenResponse>().await {
        Ok(payload) => payload,
        Err(error) => {
            let msg = format!("Risposta token OAuth Figma non valida: {error}");
            store_figma_oauth_error(&state.db, &msg).await;
            return redirect_with_status(&return_to, "error", Some(&msg));
        }
    };

    if !http_status.is_success()
        || token_payload
            .access_token
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
    {
        let msg = token_payload
            .error_description
            .clone()
            .or(token_payload.error.clone())
            .unwrap_or_else(|| format!("Token endpoint HTTP {}", http_status.as_u16()));
        store_figma_oauth_error(&state.db, &msg).await;
        return redirect_with_status(&return_to, "error", Some(&msg));
    }

    let access_token = token_payload
        .access_token
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_string();
    let refresh_token = token_payload
        .refresh_token
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_string();
    let scope = token_payload
        .scope
        .unwrap_or_else(|| "mcp:connect".to_string());
    let token_type = token_payload
        .token_type
        .unwrap_or_else(|| "Bearer".to_string());
    let expires_at = token_payload
        .expires_in
        .map(|seconds| (Utc::now() + Duration::seconds(seconds.max(0))).to_rfc3339());

    let _ = upsert_setting_value(
        &state.db,
        "figma_oauth_token",
        &access_token,
        "connectors",
        "Token Figma (PAT figd_... o OAuth access token) usato dal plugin Figma MCP",
        true,
    )
    .await;
    let _ = upsert_setting_value(
        &state.db,
        "figma_refresh_token",
        &refresh_token,
        "connectors",
        "Refresh token OAuth Figma",
        true,
    )
    .await;
    let _ = upsert_setting_value(
        &state.db,
        "figma_token_scope",
        &scope,
        "connectors",
        "Scope token OAuth Figma",
        false,
    )
    .await;
    let _ = upsert_setting_value(
        &state.db,
        "figma_token_expires_at",
        expires_at.as_deref().unwrap_or(""),
        "connectors",
        "Scadenza token OAuth Figma (ISO)",
        false,
    )
    .await;
    let _ = upsert_setting_value(
        &state.db,
        "figma_last_oauth_error",
        "",
        "connectors",
        "Ultimo errore OAuth Figma",
        false,
    )
    .await;

    let success_message = format!("OAuth Figma collegato ({}).", token_type);
    redirect_with_status(&return_to, "ok", Some(&success_message))
}
