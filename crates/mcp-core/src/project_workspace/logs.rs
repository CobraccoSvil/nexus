use super::*;

pub async fn get_project_problems(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    // Routing separazione DB (regola E): `jobs` e' tabella MIGRATA, vive nel
    // pool del progetto. DB progetto non disponibile -> errore tipizzato (503/404).
    let proj_pool =
        crate::project_db_routes::project_data_pool_from(&state.db, project_id).await?;

    // Aggregazione fonti nell'ordine canonico (preserva l'ordine di inserimento a
    // parita' di chiave di sort): quality -> security -> jobs -> diag -> runtime.
    let mut items = Vec::<Value>::new();
    items.extend(collect_quality_problems(&state.db, project_id).await?);
    // security_findings: tabella legacy (0001) senza writer attivo nel codebase;
    // includeva righe storiche eternamente "aperte". Esclusa dalla vista canonica
    // finche' non esiste uno scanner con lifecycle (fixed_at / resolved_at).
    items.extend(collect_failed_job_problems(&proj_pool, project_id).await?);
    items.extend(collect_service_diagnosis_problems(&state.db, project_id).await?);
    items.extend(collect_runtime_problems(&state.db, project_id).await?);

    crate::project_workspace::problem_aggregation::aggregate_problems(&mut items);
    sort_problems(&mut items);

    Ok(Json(json!({ "items": items })))
}

/// Ordina i problemi per severita crescente (error prima), poi per createdAt
/// decrescente (piu' recenti prima). Ordinamento stabile: a parita' di chiave
/// resta l'ordine di inserimento (che riflette la priorita' delle fonti).
fn sort_problems(items: &mut [Value]) {
    items.sort_by(|left, right| {
        let left_severity = left
            .get("severity")
            .and_then(Value::as_str)
            .map(severity_rank)
            .unwrap_or(2);
        let right_severity = right
            .get("severity")
            .and_then(Value::as_str)
            .map(severity_rank)
            .unwrap_or(2);
        left_severity.cmp(&right_severity).then_with(|| {
            right
                .get("createdAt")
                .and_then(Value::as_str)
                .cmp(&left.get("createdAt").and_then(Value::as_str))
        })
    });
}

/// PUNTO UNICO QUALITY (regola L): i quality finding hanno UNA sola tabella di
/// verita', `project_quality_findings` (scritta da run_quality_scan /
/// maybe_auto_scan_file, letta dal pannello "Ottimizzazione" via
/// get_quality_findings). La vecchia `quality_findings` (mig 0001) non e' mai
/// stata scritta da alcun INSERT — tabella morta — ed e' stata droppata
/// (mig 0487). Il pannello "Problemi" e' la vista aggregata e include anche i
/// quality finding, escludendo quelli gia' risolti (fixed_at), marcati come
/// falsi positivi (is_false_positive) o auto-soppressi dalla passata
/// vettoriale N+1 (is_auto_suppressed): stesso criterio di
/// get_quality_findings e dell'evento FindingsUpdated emesso dall'auto-scan
/// per-file (regola L: prima `is_auto_suppressed` non compariva in nessuno
/// dei tre, e un finding auto-soppresso comparso in tabella restava visibile
/// ovunque senza spiegazione, difetto reale del 30/07/2026).
async fn collect_quality_problems(
    db: &sqlx::PgPool,
    project_id: Uuid,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(
        r#"
        SELECT id, file_path, category, severity, title, line_number, scanned_at
        FROM project_quality_findings
        WHERE project_id = $1
          AND fixed_at IS NULL
          AND (is_false_positive = FALSE OR is_false_positive IS NULL)
          AND (is_auto_suppressed = FALSE OR is_auto_suppressed IS NULL)
        ORDER BY scanned_at DESC
        LIMIT 100
        "#,
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.get::<Uuid, _>("id").to_string(),
                "severity": row.get::<String, _>("severity"),
                "source": format!("quality:{}", row.get::<String, _>("category")),
                "message": row.get::<String, _>("title"),
                "filePath": row.get::<String, _>("file_path"),
                "line": row.try_get::<Option<i32>, _>("line_number").ok().flatten(),
                "column": serde_json::Value::Null,
                "createdAt": row.get::<chrono::DateTime<chrono::Utc>, _>("scanned_at").to_rfc3339(),
            })
        })
        .collect())
}

/// Invalida la vista UI del pannello Problemi: il frontend ascolta FindingsUpdated
/// e ri-fetcha via get_project_problems. Punto unico per refresh non-quality.
pub(crate) fn emit_problems_panel_refresh(project_id: Uuid, resolved_ids: Vec<Uuid>) {
    nexus_events::dispatcher::emit_global(
        project_id,
        nexus_events::ProjectEvent::FindingsUpdated {
            scan_id: None,
            total: 0,
            critical: 0,
            warnings: 0,
            resolved_ids,
        },
    );
}

/// Variante batch di [`emit_problems_panel_refresh`] per gli sweep multi-progetto
/// (observer, port_enforcer): raggruppa le righe `(project_id, diagnosi_id)`
/// risolte e emette UN refresh per progetto. Punto unico (regola L).
pub(crate) fn emit_problems_panel_refresh_batch(rows: &[(Uuid, Uuid)]) {
    if rows.is_empty() {
        return;
    }
    use std::collections::HashMap;
    let mut by_project: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for (project_id, id) in rows {
        by_project.entry(*project_id).or_default().push(*id);
    }
    for (project_id, ids) in by_project {
        emit_problems_panel_refresh(project_id, ids);
    }
}

/// Audit 27/05/2026: aggiunto 'passed' alla lista di esclusione.
/// I job Playwright vengono salvati con status='passed' quando i test
/// hanno successo, ma la query li includeva nel pannello Problemi
/// marcandoli erroneamente come severity='error' (30 falsi positivi visti
/// nel pannello su demo-wsl). Solo i job con status='failed' o stato
/// anomalo non standard devono apparire come problemi.
///
/// SUPERSEDED (regola H): un job fallito di uno STESSO kind seguito da un esito
/// di SUCCESSO piu' recente (stesso project+kind) non e' piu' un problema attivo
/// — la suite e' tornata verde. Senza questo filtro i run storici falliti
/// restavano nel pannello a vita gonfiandolo (es. 18 playwright_test failed
/// delle 04:12 ancora elencati dopo il passed delle 04:40). Il NOT EXISTS li
/// esclude appena un run success dello stesso kind li supera: il pannello
/// riflette lo stato REALE, non lo storico.
async fn collect_failed_job_problems(
    proj_pool: &sqlx::PgPool,
    project_id: Uuid,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(
        r#"
        SELECT id, kind, status, input, created_at
        FROM jobs j
        WHERE j.project_id = $1
          AND j.status NOT IN ('queued', 'running', 'completed', 'success', 'passed', 'ok', 'done')
          AND NOT EXISTS (
            SELECT 1 FROM jobs j2
            WHERE j2.project_id = j.project_id
              AND j2.kind = j.kind
              AND j2.status IN ('completed', 'success', 'passed', 'ok', 'done')
              AND j2.created_at > j.created_at
          )
        ORDER BY created_at DESC
        LIMIT 50
        "#,
    )
    .bind(project_id)
    .fetch_all(proj_pool)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(rows
        .into_iter()
        .map(|row| {
            let input = row.get::<Value, _>("input");
            json!({
                "id": row.get::<Uuid, _>("id").to_string(),
                "severity": "error",
                "source": row.get::<String, _>("kind"),
                "message": input.get("message").and_then(Value::as_str).unwrap_or("Job fallito"),
                "filePath": input.get("file_path").and_then(Value::as_str),
                "line": input.get("line").and_then(Value::as_i64),
                "column": input.get("column").and_then(Value::as_i64),
                "createdAt": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
            })
        })
        .collect())
}

/// Aggrega le diagnosi del service_observer (anomaly/crash su servizi del
/// progetto). E' il secondo store di "problemi" del progetto: prima visibile
/// solo via UI separata, ora compare nel pannello "Problemi" come deve essere
/// (regola L: un solo posto dove l'utente cerca i problemi).
///
/// Il criterio di visibilita' e' UNO e vale per ogni `signal_kind`: si mostra
/// tutto cio' che non e' `resolved`. `resolved` e' l'unico stato che significa
/// "non e' piu' un problema"; `diagnosing` vuol dire che qualcuno ci sta
/// lavorando e `failed_remediation` che ci ha provato e non ce l'ha fatta — in
/// nessuno dei due casi il problema e' finito.
///
/// Prima il predicato faceva un'eccezione per le sole `policy_violation`, e i
/// crash di servizio erano visibili unicamente da `open`. Con la chiusura della
/// remediation portata sul contratto di `service_recovery`, un crash che il
/// rimedio non risana finisce in `failed_remediation`: con l'eccezione ristretta
/// alle violazioni sarebbe sparito dal pannello esattamente come spariva prima
/// da `resolved`. Sarebbe stato lo stesso difetto, con un nome piu' onesto sulla
/// riga e nessuno a leggerlo.
async fn collect_service_diagnosis_problems(
    db: &sqlx::PgPool,
    project_id: Uuid,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(
        r#"
        SELECT id, unit, signal_kind, metric, value, threshold, detail, status,
               file_path, created_at
          FROM service_diagnoses
         WHERE project_id = $1
           AND status <> 'resolved'
         ORDER BY created_at DESC
         LIMIT 100
        "#,
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(rows.into_iter().map(diagnosis_row_to_problem).collect())
}

/// Mappa una riga `service_diagnoses` sul formato problema unificato.
fn diagnosis_row_to_problem(row: sqlx::postgres::PgRow) -> Value {
    let unit: String = row.get("unit");
    let signal_kind: String = row.get("signal_kind");
    let metric: Option<String> = row.try_get("metric").ok().flatten();
    let value: Option<f64> = row.try_get("value").ok().flatten();
    let threshold: Option<f64> = row.try_get("threshold").ok().flatten();
    let detail: Option<String> = row.try_get("detail").ok().flatten();
    let status: String = row.try_get("status").unwrap_or_else(|_| "open".to_string());
    let file_path: Option<String> = row.try_get("file_path").ok().flatten();
    // signal_kind = "crash" e' grave; "policy_violation" e' SEMPRE error
    // (violazione di governance risorse); "anomaly" e' warning.
    let severity = if signal_kind == "crash" || signal_kind == "policy_violation" {
        "error"
    } else {
        "warning"
    };
    let (source, message) = diagnosis_source_message(
        &signal_kind,
        &unit,
        &status,
        &metric,
        value,
        threshold,
        &detail,
    );
    // Segnale STRUTTURATO per la UI (regola M): il bottone "riprova riparazione"
    // decide su questo campo, mai sul prefisso testuale del messaggio. Vale per
    // le sole diagnosi di crash in stato terminale fallito: e' l'unico stato che
    // il ri-armo esplicito (`service_recovery::rearm_diagnosis`) accetta.
    let remediation_retryable = signal_kind == "crash" && status == "failed_remediation";
    json!({
        "id": row.get::<Uuid, _>("id").to_string(),
        "severity": severity,
        "source": source,
        "message": message,
        "filePath": file_path.clone().map(serde_json::Value::from).unwrap_or(serde_json::Value::Null),
        "line": serde_json::Value::Null,
        "column": serde_json::Value::Null,
        "createdAt": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
        "remediationRetryable": remediation_retryable,
    })
}

/// Ri-armo ESPLICITO di una riparazione fallita, dal pannello Problemi.
///
/// La richiesta umana e' il secondo segnale strutturato che rende una diagnosi
/// `failed_remediation` di nuovo ammissibile al ciclo di verifica (il primo e'
/// una scrittura registrata su un file del servizio): copre il caso in cui la
/// causa e' stata rimossa per una strada che `file_mutations` non vede — una
/// correzione fatta a mano nell'editor dell'utente. Delega al punto unico
/// `service_recovery::rearm_diagnosis` (regola L): stessa scrittura, stesse
/// condizioni, stesso motivo persistito. Da li' in poi il presidio esistente
/// riprende la riga col suo contratto invariato.
pub async fn retry_service_diagnosis(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((id, diag_id)): AxumPath<(String, String)>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let diagnosis_id = Uuid::parse_str(&diag_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Diagnosis id non valido"))?;
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    let riarmata = crate::project_workspace::service_recovery::rearm_diagnosis(
        &state.db,
        project_id,
        diagnosis_id,
        crate::project_workspace::service_recovery::REARM_EXPLICIT_REASON,
    )
    .await;
    match riarmata {
        Some(id) => {
            emit_problems_panel_refresh(project_id, vec![id]);
            Ok(Json(json!({ "rearmed": true, "diagnosisId": id.to_string() })))
        }
        // La riga non era una diagnosi di crash in stato terminale fallito di
        // questo progetto: niente da ri-armare, e lo stato non viene toccato.
        None => Err(api_error(
            StatusCode::CONFLICT,
            "La diagnosi non e' in stato failed_remediation: niente da ri-armare",
        )),
    }
}

/// PUNTO UNICO (regola L) del prefisso di stato di una diagnosi: dice a colpo
/// d'occhio se qualcuno ci sta lavorando o se ci ha provato senza riuscirci.
///
/// Vale per OGNI `signal_kind`, non per le sole violazioni risorse: da quando la
/// chiusura di una remediation di servizio passa dal contratto di
/// `service_recovery`, un crash puo' stare in `diagnosing` per l'intera verifica
/// e finire in `failed_remediation` — e senza prefisso la riga direbbe soltanto
/// "Servizio X: crash", tacendo che una riparazione e' gia' stata tentata e non
/// ha funzionato.
fn diagnosis_status_prefix(status: &str) -> &'static str {
    match status {
        "failed_remediation" => "Riparazione automatica FALLITA — ",
        "diagnosing" => "Riparazione automatica in corso — ",
        _ => "",
    }
}

/// Costruisce (source, message) per una diagnosi service_observer. Le violazioni
/// di policy vengono troncate a 600 char.
#[allow(clippy::too_many_arguments)]
fn diagnosis_source_message(
    signal_kind: &str,
    unit: &str,
    status: &str,
    metric: &Option<String>,
    value: Option<f64>,
    threshold: Option<f64>,
    detail: &Option<String>,
) -> (String, String) {
    let prefix = diagnosis_status_prefix(status);
    if signal_kind == "policy_violation" {
        // metric = 'kind/rule' (es. 'port/enforce_hardcode').
        let rule = metric.clone().unwrap_or_else(|| "resource".to_string());
        let kind_label = rule.split('/').next().unwrap_or("resource").to_string();
        let base = detail
            .clone()
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| format!("violazione {rule} ({unit})"));
        let mut message = format!("{prefix}Violazione risorse [{rule}]: {base}");
        if message.len() > 600 {
            message.truncate(600);
        }
        (format!("policy:{kind_label}"), message)
    } else {
        let metric_part = match (metric.as_deref(), value, threshold) {
            (Some(m), Some(v), Some(t)) => format!(" — {m}={v:.1} (soglia {t:.1})"),
            (Some(m), Some(v), None) => format!(" — {m}={v:.1}"),
            (Some(m), None, _) => format!(" — {m}"),
            _ => String::new(),
        };
        let mut msg = format!("{prefix}Servizio {unit}: {signal_kind}{metric_part}");
        if let Some(d) = detail.as_deref().filter(|s| !s.is_empty()) {
            msg.push('\n');
            msg.push_str(d);
        }
        (format!("service_observer:{signal_kind}"), msg)
    }
}

/// PUNTO UNICO (regola L): il pannello "Problemi" e' la vista canonica della
/// UI per i problemi del progetto e deve aggregare ANCHE i runtime issues
/// (project_runtime_issues, mig M10) — errori catturati dai tool agente
/// (run_command exit != 0, browser-check console errors) e dal service_observer
/// (container in Restarting/Exited, unit systemd failed). Prima erano visibili
/// solo all'endpoint separato /runtime-issues, e il pannello "Problemi" appariva
/// vuoto anche quando c'erano errori runtime evidenti (es. db in crash-loop con
/// `EAI_AGAIN postgres`). Filtra status open/in_progress (i resolved spariscono).
async fn collect_runtime_problems(
    db: &sqlx::PgPool,
    project_id: Uuid,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(
        r#"
        SELECT id, source, severity, message, details, tool_name, command, exit_code, created_at
          FROM project_runtime_issues
         WHERE project_id = $1
           AND status IN ('open', 'in_progress')
         ORDER BY created_at DESC
         LIMIT 200
        "#,
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(rows.into_iter().map(runtime_row_to_problem).collect())
}

/// Mappa una riga `project_runtime_issues` sul formato problema unificato.
/// `details` (stringa libera: output troncato, hint) viene appesa al campo
/// unificato "message" insieme a comando + exit code, cosi' la UI mostra
/// errore + contesto senza serializzare nuovi campi.
fn runtime_row_to_problem(row: sqlx::postgres::PgRow) -> Value {
    let details = row.try_get::<Option<String>, _>("details").ok().flatten();
    let tool_name = row.try_get::<Option<String>, _>("tool_name").ok().flatten();
    let command = row.try_get::<Option<String>, _>("command").ok().flatten();
    let exit_code = row.try_get::<Option<i32>, _>("exit_code").ok().flatten();
    let source = row.get::<String, _>("source");
    let base_msg = row.get::<String, _>("message");
    let mut message = base_msg.clone();
    if let Some(cmd) = command.as_deref().filter(|s| !s.is_empty()) {
        message = format!("{message}\n$ {cmd}");
        if let Some(code) = exit_code {
            message.push_str(&format!(" (exit={code})"));
        }
    }
    if let Some(d) = details.as_deref().filter(|s| !s.is_empty()) {
        message.push('\n');
        message.push_str(d);
    }
    json!({
        "id": row.get::<Uuid, _>("id").to_string(),
        "severity": row.get::<String, _>("severity"),
        "source": tool_name.unwrap_or_else(|| format!("runtime:{source}")),
        "message": message,
        "filePath": serde_json::Value::Null,
        "line": serde_json::Value::Null,
        "column": serde_json::Value::Null,
        "createdAt": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
    })
}

/// Legge gli ultimi N log da un file generato da `spawn_detached_service`
/// (`/tmp/nexus-proj-<unit>.log`). Usato in WSL/quando il manager systemd --user
/// non c'e' e quindi `journalctl` non vede il servizio. Output coerente con
/// `read_service_logs` (stesso schema evento per la UI). Niente filtro `--since`
/// del restart: il file di log e' append-only e copre l'intera vita del servizio
/// detached, quindi prendiamo solo l'ultima coda (`limit * 10`, capped 2000) —
/// la UI conserva gli ID gia' visti via `seenLogIdsRef` (debug-panel.tsx:178).
#[cfg(not(windows))]
async fn read_detached_logfile(
    path: &str,
    limit: usize,
    service: &str,
    channel: &str,
) -> Vec<serde_json::Value> {
    let max_lines = (limit * 10).clamp(200, 2000);
    let text = match tokio::fs::read_to_string(path).await {
        Ok(s) => s,
        Err(e) => {
            return vec![serde_json::json!({
                "id": format!("{service}-detached-err"),
                "channel": channel,
                "level": "error",
                "title": format!("Errore lettura log detached {service}"),
                "text": e.to_string(),
                "createdAt": chrono::Utc::now().to_rfc3339(),
            })];
        }
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return vec![serde_json::json!({
            "id": format!("{service}-detached-empty"),
            "channel": channel,
            "level": "info",
            "title": format!("{service} — nessun output (detached)"),
            "text": format!("Il servizio gira in modalita' detached (systemd --user non attivo). Logfile: {path}"),
            "createdAt": chrono::Utc::now().to_rfc3339(),
        })];
    }
    let mut lines: Vec<&str> = trimmed.lines().collect();
    if lines.len() > max_lines {
        lines = lines.split_off(lines.len() - max_lines);
    }
    let body = lines.join("\n");
    let level = detect_log_level(&body);
    let line_count = lines.len();
    vec![serde_json::json!({
        "id": format!("{service}-detached-{}", line_count),
        "channel": channel,
        "level": level,
        "title": format!("{service} — ultimi {line_count} log (detached)"),
        "text": body,
        "createdAt": chrono::Utc::now().to_rfc3339(),
    })]
}

pub(super) fn severity_rank(value: &str) -> i32 {
    match value.to_ascii_lowercase().as_str() {
        "error" | "critical" | "high" => 0,
        "warning" | "medium" => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod problems_tests {
    use crate::project_workspace::problem_aggregation;
    use serde_json::json;

    #[test]
    fn problem_aggregation_delegates_to_module() {
        let mut items = vec![
            json!({
                "id": "a",
                "severity": "error",
                "source": "policy:port",
                "message": "Violazione risorse [port/require_allocation]: a.ts:1 21950 (port/require_allocation) | x",
                "filePath": "a.ts",
                "line": 1,
                "createdAt": "2026-01-01T00:00:00Z",
            }),
            json!({
                "id": "b",
                "severity": "error",
                "source": "policy:port",
                "message": "Violazione risorse [port/require_allocation]: b.ts:2 21951 (port/require_allocation) | y",
                "filePath": "b.ts",
                "line": 2,
                "createdAt": "2026-01-01T00:00:01Z",
            }),
        ];
        problem_aggregation::aggregate_problems(&mut items);
        assert_eq!(items.len(), 1);
    }
}

/// Deriva il livello ("error"|"warn"|"info") dal contenuto testuale di un log,
/// per evidenziare la pillola del canale nella UI. Punto unico (regola L): la
/// stessa euristica era duplicata in read_detached_logfile e read_service_logs.
fn detect_log_level(text: &str) -> &'static str {
    let lower = text.to_lowercase();
    if lower.contains(" error ")
        || lower.contains("error:")
        || lower.contains("panicked")
        || lower.contains("exception:")
        || lower.contains("[error]")
    {
        "error"
    } else if lower.contains(" warn ") || lower.contains("warning:") || lower.contains("[warn]") {
        "warn"
    } else {
        "info"
    }
}

/// Recupera il timestamp dell'ultimo avvio del servizio (ActiveEnterTimestamp)
/// da usare come argomento `--since` di journalctl. In formato systemd
/// ("Sun 2026-04-26 16:30:00 CEST"), che journalctl accetta direttamente.
/// None se il servizio non e' mai stato avviato o systemctl non risponde.
#[cfg(not(windows))]
async fn service_active_since(service: &str) -> Option<String> {
    let show = tokio::process::Command::new("systemctl")
        .args([
            "--user",
            "show",
            service,
            "--property=ActiveEnterTimestamp",
            "--no-pager",
        ])
        .output()
        .await
        .ok();
    show.and_then(|o| {
        let s = String::from_utf8_lossy(&o.stdout).to_string();
        s.lines()
            .find_map(|l| {
                l.strip_prefix("ActiveEnterTimestamp=")
                    .map(|v| v.trim().to_string())
            })
            .filter(|v| !v.is_empty() && v != "n/a")
    })
}

/// Esegue `journalctl` per il servizio e ritorna il testo dell'output oppure,
/// in caso di errore/exit non-zero senza stdout, un evento gia' pronto per la UI
/// (variante Err). Isola la costruzione args + spawn dal chiamante.
#[cfg(not(windows))]
async fn run_journalctl(
    service: &str,
    channel: &str,
    n_lines: &str,
    since: &Option<String>,
) -> Result<String, Vec<serde_json::Value>> {
    let mut args: Vec<String> = vec![
        "--user".into(),
        "-u".into(),
        service.into(),
        "--no-pager".into(),
        "-n".into(),
        n_lines.to_string(),
        "--output=short-iso".into(),
    ];
    if let Some(ts) = since {
        args.push("--since".into());
        args.push(ts.clone());
    }
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = tokio::process::Command::new("journalctl")
        .args(&args_ref)
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() || !out.stdout.is_empty() => {
            Ok(String::from_utf8_lossy(&out.stdout).to_string())
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            Err(vec![serde_json::json!({
                "id": format!("{}-err", service),
                "channel": channel,
                "level": "warn",
                "title": format!("Nessun log disponibile per {}", service),
                "text": if stderr.is_empty() { "journalctl non ha restituito output.".to_string() } else { stderr },
                "createdAt": chrono::Utc::now().to_rfc3339(),
            })])
        }
        Err(e) => Err(vec![serde_json::json!({
            "id": format!("{}-err", service),
            "channel": channel,
            "level": "error",
            "title": format!("Errore lettura log {}", service),
            "text": e.to_string(),
            "createdAt": chrono::Utc::now().to_rfc3339(),
        })]),
    }
}

/// Legge le ultime N righe di log da un servizio systemd --user via journalctl.
///
/// Ritorna UN SOLO evento contenente l'intero output, in ordine cronologico
/// (le righe piu' recenti in fondo, come `journalctl -f` o `tail`). In passato
/// l'output veniva chunkato in eventi da 50 righe, ognuno con header tipo
/// "righe 1951-2000": confondente per l'utente, che vedeva una pila di blocchi
/// con timestamp simili e numerazione contraria al concetto di tail.
///
/// Importante: usa `--since` legato a `ActiveEnterTimestamp` del servizio, così che
/// dopo ogni `restart` la finestra log si "resetti" automaticamente — l'utente vede
/// solo gli eventi del nuovo ciclo di vita del servizio, non l'intera storia che
/// includeva crash precedenti gia' risolti.
#[cfg(not(windows))]
pub(super) async fn read_service_logs(
    service: &str,
    limit: usize,
    channel: &str,
) -> Vec<serde_json::Value> {
    // Tetto di righe restituite. `limit` arriva dal client (default 100, max 500
    // dopo clamp in get_output_events) — moltiplicato x10 per dare contesto
    // sufficiente, capped a 2000 per evitare payload enormi.
    let n_lines = (limit * 10).clamp(200, 2000).to_string();

    // Fallback DETACHED (regola L: la fonte del log e' una soltanto, ma il
    // backend storage cambia in base al manager attivo). In WSL `systemd --user`
    // non e' attivo, quindi `spawn_detached_service` (wizard.rs) scrive l'output
    // del servizio in `/tmp/nexus-proj-<unit>.log`. Senza questo fallback,
    // journalctl non vede nulla -> read_service_logs ritornava "Nessun log
    // disponibile" e il pannello Console Debug appariva sempre vuoto in WSL.
    let detached_path = format!("/tmp/nexus-proj-{service}.log");
    if let Ok(meta) = tokio::fs::metadata(&detached_path).await {
        if meta.is_file() {
            return read_detached_logfile(&detached_path, limit, service, channel).await;
        }
    }

    // Recupera il timestamp dell'ultimo avvio del servizio per il filtro --since.
    let since = service_active_since(service).await;

    let text = match run_journalctl(service, channel, &n_lines, &since).await {
        Ok(t) => t,
        Err(events) => return events,
    };

    if text.trim().is_empty() {
        return service_empty_log_event(service, channel, &since);
    }

    service_tail_log_event(service, channel, &since, text)
}

/// Evento "nessun log dal restart" quando journalctl restituisce output vuoto.
#[cfg(not(windows))]
fn service_empty_log_event(
    service: &str,
    channel: &str,
    since: &Option<String>,
) -> Vec<serde_json::Value> {
    let header = match since {
        Some(ts) => format!(
            "Il servizio e' attivo dal {} ma non ha prodotto output dal restart.",
            ts
        ),
        None => "Il servizio non ha prodotto output recente.".to_string(),
    };
    vec![serde_json::json!({
        "id": format!("{}-empty", service),
        "channel": channel,
        "level": "info",
        "title": format!("{} — nessun log dal restart", service),
        "text": header,
        "createdAt": chrono::Utc::now().to_rfc3339(),
    })]
}

/// Evento singolo con tutto il flusso di log del servizio. Le righe sono in
/// ordine cronologico ascendente (piu' vecchie in cima, piu' recenti in fondo)
/// come da default di journalctl, cosi' l'auto-scroll si comporta come tail -f.
#[cfg(not(windows))]
fn service_tail_log_event(
    service: &str,
    channel: &str,
    since: &Option<String>,
    text: String,
) -> Vec<serde_json::Value> {
    let level = detect_log_level(&text);
    let line_count = text.lines().count();
    let title = match since {
        Some(ts) => format!(
            "{} — ultimi {} log dal restart ({})",
            service, line_count, ts
        ),
        None => format!("{} — ultimi {} log", service, line_count),
    };
    vec![serde_json::json!({
        "id": format!("{}-tail", service),
        "channel": channel,
        "level": level,
        "title": title,
        "text": text,
        "createdAt": chrono::Utc::now().to_rfc3339(),
    })]
}

/// Legge le ultime 20 righe agent_processes del progetto (pool migrato). Errore
/// silenziato a Vec vuoto: i canali agent sono best-effort, non bloccano la
/// risposta se il pool progetto e' momentaneamente indisponibile.
async fn fetch_agent_process_rows(
    proj_pool: &sqlx::PgPool,
    project_id: Uuid,
) -> Vec<sqlx::postgres::PgRow> {
    sqlx::query(
        "SELECT id, label, command, status, pid, COALESCE(kind, 'service') as kind FROM agent_processes \
         WHERE project_id = $1 \
         ORDER BY created_at DESC LIMIT 20",
    )
    .bind(project_id)
    .fetch_all(proj_pool)
    .await
    .unwrap_or_default()
}

/// Canali fissi di sistema, presenti per ogni progetto indipendentemente dallo
/// stato dei servizi.
fn fixed_system_channels() -> Vec<Value> {
    vec![
        json!({ "id": "System",       "label": "System" }),
        json!({ "id": "Git",          "label": "Git" }),
        json!({ "id": "Tasks",        "label": "Tasks" }),
        json!({ "id": "Project Jobs", "label": "Project Jobs" }),
        json!({ "id": "Playwright",   "label": "Playwright" }),
        json!({ "id": "MCP Core",     "label": "MCP Core" }),
        json!({ "id": "Neural Core",  "label": "Neural Core" }),
    ]
}

pub async fn get_output_channels(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    // La chiamata vale come check di autorizzazione (fallisce con errore se
    // l'utente non puo' accedere al progetto): va eseguita su ogni OS. Il binding
    // e' prefissato `_` perche' `context.details.name` serve solo al blocco svc:*
    // Linux-only piu' sotto; su Windows resta usato solo per il side-effect di auth.
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    // Canali fissi di sistema
    let mut channels = fixed_system_channels();

    // Canali dinamici svc:* — uno per ogni servizio systemd del progetto
    // ({slug}-*.service). Linux-only: su Windows non esiste systemd `--user`,
    // i canali svc:* sono derivati piu' sotto dai servizi in `agent_processes`
    // (kind='service'), riusando le righe gia' lette (nessuna query extra).
    #[cfg(unix)]
    push_systemd_svc_channels(&mut channels, &_context.details.name).await;

    // Canali dinamici agent: usati dal pannello Servizi (tab separato).
    // Self-healing in Rust: marca come 'stopped' nel DB i processi con status='running'
    // ma PID inesistente (residui di chat AI precedenti, restart Nexus, kill esterni).
    // Routing separazione DB (regola E): `agent_processes` e' tabella MIGRATA,
    // vive nel pool del progetto (errore tipizzato se non disponibile). Lo riuso
    // sotto per la sanazione UPDATE (stesso project_id).
    let proj_pool =
        crate::project_db_routes::project_data_pool_from(&state.db, project_id).await?;
    let agent_rows_raw = fetch_agent_process_rows(&proj_pool, project_id).await;

    // Canali svc:* su Windows: nessun systemd, i servizi di progetto sono le righe
    // agent_processes con kind='service'. Si riusano le righe gia' lette (nessuna
    // query extra) generando un canale svc:{label} per label distinta, coerente
    // con lo schema id usato dal frontend su Linux.
    #[cfg(windows)]
    push_agent_svc_channels(&mut channels, &agent_rows_raw);

    // Identifica i processi fantasma (status='running' ma PID morto), li sana nel
    // DB e restituisce le sole righe vive per la costruzione dei canali agent.
    let agent_rows = sanitize_orphan_processes(&proj_pool, agent_rows_raw).await;

    for row in &agent_rows {
        channels.push(agent_row_to_channel(row));
    }

    Ok(Json(json!({ "channels": channels })))
}

/// Aggiunge i canali svc:* per i servizi systemd `--user` del progetto
/// ({slug}-*.service, escludendo quelli disabled). Linux-only.
#[cfg(unix)]
async fn push_systemd_svc_channels(channels: &mut Vec<Value>, project_name: &str) {
    let slug = project_name.to_lowercase().replace([' ', '_'], "-");
    let prefix = format!("{}-", slug);
    let Ok(svc_out) = tokio::process::Command::new("systemctl")
        .args([
            "--user",
            "list-unit-files",
            "--type=service",
            "--no-legend",
            "--no-pager",
        ])
        .output()
        .await
    else {
        return;
    };
    for line in String::from_utf8_lossy(&svc_out.stdout).lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let unit = cols.first().copied().unwrap_or("");
        let state = cols.get(1).copied().unwrap_or("");
        if unit.starts_with(&prefix) && unit.ends_with(".service") && state != "disabled" {
            let short = unit
                .strip_prefix(&prefix)
                .unwrap_or(unit)
                .strip_suffix(".service")
                .unwrap_or(unit);
            channels.push(json!({
                "id":    format!("svc:{}", unit),
                "label": short,
                "title": unit,
            }));
        }
    }
}

/// Aggiunge i canali svc:{label} derivati dalle righe agent_processes con
/// kind='service' (una per label distinta). Windows-only: rimpiazza i canali
/// systemd assenti su Windows.
#[cfg(windows)]
fn push_agent_svc_channels(channels: &mut Vec<Value>, agent_rows_raw: &[sqlx::postgres::PgRow]) {
    let mut seen_svc: std::collections::HashSet<String> = std::collections::HashSet::new();
    for row in agent_rows_raw {
        let kind: String = row.try_get::<String, _>("kind").unwrap_or_default();
        if kind != "service" {
            continue;
        }
        let label: String = row.try_get::<String, _>("label").unwrap_or_default();
        if label.is_empty() || !seen_svc.insert(label.clone()) {
            continue;
        }
        channels.push(json!({
            "id":    format!("svc:{}", label),
            "label": label,
            "title": label,
        }));
    }
}

/// Self-healing: marca come 'stopped' nel DB i processi 'running' con PID
/// inesistente (residui di chat precedenti, restart, kill esterni) e ritorna le
/// sole righe non-fantasma. Liveness cross-platform: punto unico
/// process_util::process_alive (regola L); il vecchio check inline su
/// `/proc/{pid}` era cieco su Windows e spegneva servizi realmente vivi.
async fn sanitize_orphan_processes(
    proj_pool: &sqlx::PgPool,
    agent_rows_raw: Vec<sqlx::postgres::PgRow>,
) -> Vec<sqlx::postgres::PgRow> {
    let mut orphan_ids: Vec<Uuid> = Vec::new();
    for row in &agent_rows_raw {
        let status: String = row.try_get::<String, _>("status").unwrap_or_default();
        if status != "running" {
            continue;
        }
        let pid: Option<i32> = row.try_get::<Option<i32>, _>("pid").ok().flatten();
        let alive = match pid {
            Some(p) if p > 0 => crate::process_util::process_alive(p as u32),
            _ => false,
        };
        if !alive {
            if let Ok(id) = row.try_get::<Uuid, _>("id") {
                orphan_ids.push(id);
            }
        }
    }
    if !orphan_ids.is_empty() {
        let _ = sqlx::query("UPDATE agent_processes SET status = 'stopped' WHERE id = ANY($1)")
            .bind(&orphan_ids)
            .execute(proj_pool)
            .await;
    }

    // Filtra immediatamente i fantasmi dal risultato corrente, così la response è già pulita
    let orphan_set: std::collections::HashSet<Uuid> = orphan_ids.into_iter().collect();
    agent_rows_raw
        .into_iter()
        .filter(|row| {
            let id: Uuid = row.try_get("id").unwrap_or_default();
            !orphan_set.contains(&id)
        })
        .collect()
}

/// Mappa una riga agent_processes sul canale "agent:{id}" con icona di stato.
fn agent_row_to_channel(row: &sqlx::postgres::PgRow) -> Value {
    let proc_id: Uuid = row.get("id");
    let label: String = row.get("label");
    let status: String = row.get("status");
    let kind: String = row.get("kind");
    let display = if label.is_empty() {
        let cmd: String = row.get("command");
        cmd.chars().take(30).collect::<String>()
    } else {
        label
    };
    let icon = match status.as_str() {
        "running" => "● ",
        "failed" => "✗ ",
        "stopped" => "○ ",
        _ => "◌ ",
    };
    json!({
        "id": format!("agent:{}", proc_id),
        "label": format!("{}{}", icon, display),
        "kind": kind
    })
}

pub async fn get_output_events(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<BTreeMap<String, String>>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let channel = query
        .get("channel")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "System".to_string());
    let limit = query
        .get("limit")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(100)
        .clamp(1, 500);

    // Routing separazione DB (regola E): `jobs` e `agent_processes` sono tabelle
    // MIGRATE (vivono nel pool del progetto); `git_operations` NON e' migrata e
    // resta sul meta-pool. DB progetto non disponibile -> errore tipizzato (503/404).
    let proj_pool =
        crate::project_db_routes::project_data_pool_from(&state.db, project_id).await?;

    let events = match channel.as_str() {
        "Git" => git_channel_events(&state.db, project_id, limit).await?,
        "Tasks" | "Project Jobs" | "Playwright" => {
            jobs_channel_events(&proj_pool, project_id, &channel, limit).await?
        }
        ch if ch.starts_with("agent:") => agent_channel_events(&proj_pool, ch, &channel).await?,
        "System" => system_channel_events(&state.db, &context).await,
        ch if ch.starts_with("svc:") => {
            svc_channel_events(&context, &proj_pool, ch, &channel, limit).await
        }
        _ => vec![project_context_event(&context, &channel)],
    };

    Ok(Json(json!({ "channel": channel, "events": events })))
}

/// Eventi del canale "Git" a partire da `git_operations` (meta-pool: tabella non
/// migrata). Il testo unifica stdout+stderr con separatore solo se entrambi
/// non vuoti.
async fn git_channel_events(
    db: &sqlx::PgPool,
    project_id: Uuid,
    limit: i64,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(
        r#"
        SELECT id, operation, status, stdout, stderr, created_at
        FROM git_operations
        WHERE project_id = $1
        ORDER BY created_at DESC
        LIMIT $2
        "#,
    )
    .bind(project_id)
    .bind(limit)
    .fetch_all(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.get::<Uuid, _>("id").to_string(),
                "channel": "Git",
                "level": if row.get::<String, _>("status") == "success" { "info" } else { "error" },
                "title": row.get::<String, _>("operation"),
                "text": format!(
                    "{}{}{}",
                    row.get::<String, _>("stdout"),
                    if !row.get::<String, _>("stdout").is_empty() && !row.get::<String, _>("stderr").is_empty() { "\n" } else { "" },
                    row.get::<String, _>("stderr")
                ),
                "createdAt": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
            })
        })
        .collect())
}

/// Eventi dei canali job ("Tasks"/"Project Jobs"/"Playwright"): filtro
/// playwright/non-playwright delegato alla query (bind $2 = nome canale).
async fn jobs_channel_events(
    proj_pool: &sqlx::PgPool,
    project_id: Uuid,
    channel: &str,
    limit: i64,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(
        r#"
        SELECT id, kind, status, input, created_at
        FROM jobs
        WHERE project_id = $1
          AND (
            $2 = 'Project Jobs'
            OR ($2 = 'Tasks' AND kind NOT ILIKE '%playwright%')
            OR ($2 = 'Playwright' AND kind ILIKE '%playwright%')
          )
        ORDER BY created_at DESC
        LIMIT $3
        "#,
    )
    .bind(project_id)
    .bind(channel)
    .bind(limit)
    .fetch_all(proj_pool)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(rows
        .into_iter()
        .map(|row| {
            let input = row.get::<Value, _>("input");
            json!({
                "id": row.get::<Uuid, _>("id").to_string(),
                "channel": channel,
                "level": if matches!(row.get::<String, _>("status").as_str(), "failed" | "error" | "cancelled") { "error" } else { "info" },
                "title": row.get::<String, _>("kind"),
                "text": input.get("message").and_then(Value::as_str).unwrap_or("Nessun output testuale disponibile"),
                "createdAt": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
            })
        })
        .collect())
}

/// Eventi del canale "agent:{uuid}" — output/stderr di un singolo processo
/// agent. Vec vuoto se il processo non esiste.
async fn agent_channel_events(
    proj_pool: &sqlx::PgPool,
    ch: &str,
    channel: &str,
) -> Result<Vec<Value>, ApiError> {
    let proc_id_str = &ch[6..];
    let proc_id = Uuid::parse_str(proc_id_str)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "process id non valido"))?;
    let row = sqlx::query(
        "SELECT id, label, command, status, exit_code, output, error_output, pid, created_at \
         FROM agent_processes WHERE id = $1",
    )
    .bind(proc_id)
    .fetch_optional(proj_pool)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(row) = row else {
        return Ok(vec![]);
    };
    let status: String = row.get("status");
    let output: String = row.get("output");
    let error_output: String = row.get("error_output");
    let label: String = row.get("label");
    let command: String = row.get("command");
    let pid: Option<i32> = row.try_get("pid").unwrap_or(None);
    let exit_code: Option<i32> = row.try_get("exit_code").unwrap_or(None);
    let title = format!(
        "{} [pid: {}, status: {}{}]",
        if label.is_empty() { &command } else { &label },
        pid.map(|p| p.to_string()).unwrap_or_else(|| "?".into()),
        status,
        exit_code
            .map(|c| format!(", exit: {}", c))
            .unwrap_or_default(),
    );
    let text = if error_output.is_empty() {
        output
    } else {
        format!("{}\n--- STDERR ---\n{}", output, error_output)
    };
    Ok(vec![json!({
        "id": proc_id.to_string(),
        "channel": channel,
        "level": if status == "failed" { "error" } else { "info" },
        "title": title,
        "text": text,
        "createdAt": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
    })])
}

/// Evento del canale "System": stato di TUTTI i servizi di progetto rilevati
/// dinamicamente, piu' nome progetto e root.
///
/// Enumerazione via PUNTO UNICO `service_manager::active().list(&ctx)` (regola L):
/// su Linux delega al backend systemd, su Windows ai processi gestiti in
/// `agent_processes`. Prima l'enumerazione era via `systemctl --user` diretto e
/// non gated: su Windows (niente systemd) restituiva sempre lista vuota e il
/// canale mentiva con "Nessun servizio trovato".
async fn system_channel_events(
    db: &sqlx::PgPool,
    context: &crate::projects::ProjectContext,
) -> Vec<Value> {
    use crate::project_workspace::service_manager::{self, ServiceBackend, ServiceContext};
    use crate::project_workspace::services::project_service_slug;

    let slug = project_service_slug(&context.details.name);
    let mut lines = Vec::new();

    let ctx = ServiceContext {
        db,
        port_registry: None,
        project_id: context.project_id,
        slug: &slug,
        project_root: &context.repository_root_path,
    };
    // `id` = unit completo (Linux) o label (Windows); `state` e' gia' normalizzato
    // (regola M: stato da enum strutturato, non da parsing di prosa). Riproduco la
    // shape testuale "{id}: {stato}" storica, con lo stato normalizzato al posto
    // della coppia grezza active/sub di systemd.
    for entry in service_manager::active().list(&ctx).await {
        let state = serde_json::to_value(entry.state)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| "unknown".to_string());
        lines.push(format!("{}: {} [{}]", entry.id, state, entry.managed_by));
    }

    if lines.is_empty() {
        lines.push(format!(
            "Nessun servizio trovato per il progetto '{}'.",
            slug
        ));
    }

    lines.push(String::new());
    lines.push(format!("Progetto: {}", context.details.name));
    lines.push(format!("Root: {}", context.root_path.to_string_lossy()));

    vec![json!({
        "id": format!("system-{}", context.project_id),
        "channel": "System",
        "level": "info",
        "title": "System status",
        "text": lines.join("\n"),
        "createdAt": chrono::Utc::now().to_rfc3339(),
    })]
}

/// Eventi del canale dinamico "svc:{unit}" — log del servizio via
/// read_service_logs. Vec vuoto se l'unit non appartiene al progetto (stesso
/// output dell'early-return originale `{ events: [] }`).
async fn svc_channel_events(
    context: &crate::projects::ProjectContext,
    proj_pool: &sqlx::PgPool,
    ch: &str,
    channel: &str,
    limit: i64,
) -> Vec<Value> {
    let unit = ch.strip_prefix("svc:").unwrap_or(ch);
    let slug = context.details.name.to_lowercase().replace([' ', '_'], "-");
    let prefix = format!("{}-", slug);
    // Verifica di appartenenza al progetto
    if !unit.starts_with(&prefix) {
        return vec![];
    }

    // Su Windows i servizi di progetto NON sono unit systemd ne' processi
    // detached con logfile in /tmp: sono processi gestiti (agent_processes,
    // kind='service') il cui stdout/stderr e' catturato in output/error_output
    // (punto unico regola L: la stessa fonte del modello servizi Windows,
    // list_services_windows). journalctl e /tmp/nexus-proj-*.log non esistono
    // qui, quindi read_service_logs restava muto e la Console Debug vuota.
    #[cfg(windows)]
    {
        let _ = limit;
        let short = unit
            .strip_prefix(&prefix)
            .unwrap_or(unit)
            .strip_suffix(".service")
            .unwrap_or(unit);
        return windows_service_log_events(proj_pool, context.project_id, short, channel).await;
    }

    #[cfg(not(windows))]
    {
        let _ = proj_pool;
        read_service_logs(unit, limit as usize, channel).await
    }
}

/// Log di un servizio Windows: legge output/error_output dalla riga
/// agent_processes piu' recente (l'ultimo ciclo di vita del servizio) sul pool
/// del progetto. Un solo evento con lo stream unificato, coerente con
/// service_tail_log_event su Linux.
#[cfg(windows)]
async fn windows_service_log_events(
    proj_pool: &sqlx::PgPool,
    project_id: Uuid,
    short: &str,
    channel: &str,
) -> Vec<Value> {
    use sqlx::Row;
    let row = sqlx::query(
        "SELECT id, status, output, error_output FROM agent_processes \
         WHERE project_id = $1 AND kind = 'service' AND label = $2 \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(project_id)
    .bind(short)
    .fetch_optional(proj_pool)
    .await
    .ok()
    .flatten();

    let Some(row) = row else {
        return vec![];
    };

    let status: String = row.try_get("status").unwrap_or_default();
    let output: String = row.try_get("output").unwrap_or_default();
    let error_output: String = row.try_get("error_output").unwrap_or_default();

    let mut text = String::new();
    if !output.trim().is_empty() {
        text.push_str(output.trim_end());
    }
    if !error_output.trim().is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(error_output.trim_end());
    }
    if text.is_empty() {
        text = format!("Servizio '{short}' ({status}): nessun output catturato.");
    }

    // Livello: punto unico detect_log_level; forza error se il processo e' failed.
    let level = if status == "failed" {
        "error"
    } else {
        detect_log_level(&text)
    };

    vec![serde_json::json!({
        "id": format!("winsvc-{}", row.get::<Uuid, _>("id")),
        "channel": channel,
        "level": level,
        "title": format!("{short} — {status}"),
        "text": text,
        "createdAt": chrono::Utc::now().to_rfc3339(),
    })]
}

/// Evento di fallback (canale non riconosciuto): riepilogo del contesto progetto.
fn project_context_event(context: &crate::projects::ProjectContext, channel: &str) -> Value {
    json!({
        "id": format!("system-{}", context.project_id),
        "channel": channel,
        "level": "info",
        "title": "Project context",
        "text": format!(
            "Progetto attivo: {}\nRoot: {}\nRepository: {}",
            context.details.name,
            context.root_path.to_string_lossy(),
            context.repository_root_path.to_string_lossy()
        ),
        "createdAt": chrono::Utc::now().to_rfc3339(),
    })
}

pub async fn get_playwright_runs(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    // Routing separazione DB (regola E): `jobs` e' tabella MIGRATA, vive nel pool
    // del progetto. La query `projects` piu' sotto resta sul meta-pool (non migrata).
    let proj_pool =
        crate::project_db_routes::project_data_pool_from(&state.db, project_id).await?;
    let runs = playwright_runs_for_project(&proj_pool, project_id)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Verifica se Playwright e' configurato (config file nella project root)
    let project_root: Option<String> =
        sqlx::query_scalar("SELECT project_root FROM projects WHERE id = $1")
            .bind(project_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();

    let configured = playwright_configured(project_root.as_deref());

    Ok(Json(json!({ "runs": runs, "configured": configured })))
}

/// Elenco dei run Playwright di un progetto, nel formato atteso dal pannello.
///
/// PUNTO UNICO (regola L) della lettura `jobs` per Playwright: la usano sia
/// l'endpoint REST `get_playwright_runs` sia lo snapshot del dispatcher
/// (`dispatcher_routes::project_snapshot`). Prima ognuno aveva la PROPRIA copia
/// della query: quando `jobs` e' stata migrata al DB per-progetto solo la copia
/// in questo file e' stata aggiornata, e lo snapshot ha continuato a leggere dal
/// meta-pool -- dove la tabella non esiste piu' -- restituendo 500 a ogni
/// bootstrap del dispatcher. Un solo lettore, una sola volta da aggiornare.
///
/// `pool` DEVE essere il pool del progetto (`project_data_pool_from`): qui non
/// si risolve da soli, cosi' il chiamante resta responsabile dell'ownership
/// (che ha gia' verificato con `load_project_context`).
pub async fn playwright_runs_for_project(
    pool: &sqlx::PgPool,
    project_id: Uuid,
) -> Result<Vec<Value>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT id, kind, status, input, created_at, updated_at, progress
        FROM jobs
        WHERE project_id = $1 AND kind ILIKE '%playwright%'
        ORDER BY created_at DESC
        LIMIT 50
        "#,
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(playwright_run_to_json).collect())
}

/// Mappa una riga job playwright sul formato lista run.
fn playwright_run_to_json(row: sqlx::postgres::PgRow) -> Value {
    let input = row.get::<Value, _>("input");
    let progress = row
        .try_get::<Value, _>("progress")
        .unwrap_or_else(|_| json!({}));
    json!({
        "id": row.get::<Uuid, _>("id").to_string(),
        "label": input.get("label").and_then(Value::as_str).unwrap_or("Playwright run"),
        "status": row.get::<String, _>("status"),
        "summary": input.get("message").and_then(Value::as_str),
        // Esito strutturato (regola M/N):
        // "passed"|"flaky"|"tests_failed"|"setup_failed". Assente sui job
        // pre-fix (input scritto prima di questo campo): il pannello ripiega
        // sul rendering legacy quando manca.
        "outcome": input.get("outcome").and_then(Value::as_str),
        "failureCause": input.get("failure_cause").and_then(Value::as_str),
        // Test dichiarati instabili dalla riesecuzione mirata: il debito di
        // test resta VISIBILE con i nomi, non riassunto in un colore.
        "flakyTests": input.get("flaky_tests").cloned().unwrap_or_else(|| json!([])),
        "artifacts": input.get("artifacts").cloned().unwrap_or_else(|| json!([])),
        "command": input.get("command").and_then(Value::as_str),
        "exitCode": input.get("exit_code").and_then(Value::as_i64),
        "progress": progress,
        "createdAt": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
        "updatedAt": row.try_get::<chrono::DateTime<chrono::Utc>, _>("updated_at")
            .map(|d| d.to_rfc3339())
            .unwrap_or_default(),
    })
}

/// True se la project root contiene un file di config Playwright (ts/js/mjs).
fn playwright_configured(project_root: Option<&str>) -> bool {
    let Some(root) = project_root else {
        return false;
    };
    let root_path = std::path::Path::new(root);
    root_path.join("playwright.config.ts").exists()
        || root_path.join("playwright.config.js").exists()
        || root_path.join("playwright.config.mjs").exists()
}

/// GET /api/projects/:id/playwright/runs/:run_id  — dettaglio singolo run con output_log.
pub async fn get_playwright_run_detail(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((project_id_str, run_id_str)): AxumPath<(String, String)>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id_str)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let run_id = Uuid::parse_str(&run_id_str)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Run id non valido"))?;
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    // Routing separazione DB (regola E): `jobs` e' tabella MIGRATA, vive nel pool
    // del progetto. DB progetto non disponibile -> errore tipizzato (503/404).
    let proj_pool =
        crate::project_db_routes::project_data_pool_from(&state.db, project_id).await?;
    let row = sqlx::query(
        r#"
        SELECT id, status, input, created_at, updated_at, progress, output_log
        FROM jobs
        WHERE id = $1 AND project_id = $2 AND kind = 'playwright_test'
        "#,
    )
    .bind(run_id)
    .bind(project_id)
    .fetch_optional(&proj_pool)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Run non trovato"))?;

    Ok(Json(playwright_run_detail_to_json(row)))
}

/// Mappa una riga job playwright sul formato dettaglio run (include outputLog).
fn playwright_run_detail_to_json(row: sqlx::postgres::PgRow) -> Value {
    let input = row.get::<Value, _>("input");
    let progress = row
        .try_get::<Value, _>("progress")
        .unwrap_or_else(|_| json!({}));
    let output_log = row
        .try_get::<Option<String>, _>("output_log")
        .ok()
        .flatten()
        .unwrap_or_default();

    json!({
        "id": row.get::<Uuid, _>("id").to_string(),
        "status": row.get::<String, _>("status"),
        "label": input.get("label").and_then(Value::as_str).unwrap_or("Playwright run"),
        "command": input.get("command").and_then(Value::as_str),
        "summary": input.get("message").and_then(Value::as_str),
        // Vedi playwright_run_to_json: stesso campo strutturato, stessa fonte.
        "outcome": input.get("outcome").and_then(Value::as_str),
        "failureCause": input.get("failure_cause").and_then(Value::as_str),
        "flakyTests": input.get("flaky_tests").cloned().unwrap_or_else(|| json!([])),
        "artifacts": input.get("artifacts").cloned().unwrap_or_else(|| json!([])),
        "exitCode": input.get("exit_code").and_then(Value::as_i64),
        "progress": progress,
        "outputLog": output_log,
        "createdAt": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
        "updatedAt": row.try_get::<chrono::DateTime<chrono::Utc>, _>("updated_at")
            .map(|d| d.to_rfc3339())
            .unwrap_or_default(),
    })
}

/// Verifica che il job playwright appartenga al progetto. Err NOT_FOUND se non
/// esiste, INTERNAL_SERVER_ERROR se la query fallisce.
async fn ensure_playwright_run_exists(
    proj_pool: &sqlx::PgPool,
    run_id: Uuid,
    project_id: Uuid,
) -> Result<(), ApiError> {
    let job_exists: Option<String> = sqlx::query_scalar(
        "SELECT status FROM jobs WHERE id = $1 AND project_id = $2 AND kind = 'playwright_test'",
    )
    .bind(run_id)
    .bind(project_id)
    .fetch_optional(proj_pool)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if job_exists.is_none() {
        return Err(api_error(StatusCode::NOT_FOUND, "Run non trovato"));
    }
    Ok(())
}

/// Stream SSE boxed emesso da `stream_playwright_run` (eventi infallibili).
type PlaywrightSseStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>
            + Send,
    >,
>;

/// Costruisce lo stream SSE: se esiste un channel live si aggancia al broadcast
/// e mappa ogni evento; altrimenti (run terminato) emette un singolo evento
/// "final" con lo stato dal DB e chiude.
async fn build_playwright_sse_stream(
    state: &AppState,
    proj_pool: &sqlx::PgPool,
    run_id: Uuid,
) -> PlaywrightSseStream {
    use axum::response::sse::Event;
    use futures::StreamExt;

    let rx_opt = state
        .playwright_channels
        .get(&run_id)
        .map(|tx| tx.subscribe());

    match rx_opt {
        Some(rx) => {
            // Run live: stream gli eventi finche' il sender e' aperto
            Box::pin(
                tokio_stream::wrappers::BroadcastStream::new(rx)
                    .map(|res| Ok::<_, std::convert::Infallible>(playwright_event_to_sse(res))),
            )
        }
        None => {
            // Run gia' chiuso: emette singolo evento "final" con stato DB e termina.
            let payload = playwright_final_payload(proj_pool, run_id).await;
            let ev = Event::default().event("final").data(payload.to_string());
            Box::pin(tokio_stream::once(Ok::<_, std::convert::Infallible>(ev)))
        }
    }
}

/// GET /api/projects/:id/playwright/runs/:run_id/stream  — SSE stream eventi live.
///
/// Eventi emessi (SSE event types):
/// - `line`: una riga di output Playwright
/// - `progress`: counter aggiornati (passed/failed/skipped/current_spec)
/// - `final`: status terminale (passed/failed/timeout)
///
/// Quando il run e' gia' terminato (no channel attivo), ritorna subito un
/// evento `final` con lo stato dal DB e chiude.
pub async fn stream_playwright_run(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((project_id_str, run_id_str)): AxumPath<(String, String)>,
) -> Result<axum::response::Sse<PlaywrightSseStream>, (StatusCode, Json<Value>)> {
    use axum::response::sse::Sse;

    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id_str)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let run_id = Uuid::parse_str(&run_id_str)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Run id non valido"))?;
    let _ = load_project_context(&state.db, project_id, user_id).await?;

    // Routing separazione DB (regola E): `jobs` e' tabella MIGRATA, vive nel pool
    // del progetto. Riuso il pool sotto per il fallback row_opt (stesso progetto).
    let proj_pool =
        crate::project_db_routes::project_data_pool_from(&state.db, project_id).await?;

    // Verifica che il job appartenga al progetto
    ensure_playwright_run_exists(&proj_pool, run_id, project_id).await?;

    let stream = build_playwright_sse_stream(&state, &proj_pool, run_id).await;

    Ok(Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

/// Converte un item del BroadcastStream Playwright in un evento SSE tipizzato
/// (line/progress/final); "error"/"lag" in caso di receiver in ritardo.
fn playwright_event_to_sse(
    res: Result<
        crate::playwright_live::PlaywrightEvent,
        tokio_stream::wrappers::errors::BroadcastStreamRecvError,
    >,
) -> axum::response::sse::Event {
    use axum::response::sse::Event;
    match res {
        Ok(ev) => {
            let event_type = match &ev {
                crate::playwright_live::PlaywrightEvent::Line { .. } => "line",
                crate::playwright_live::PlaywrightEvent::Progress { .. } => "progress",
                crate::playwright_live::PlaywrightEvent::Final { .. } => "final",
            };
            let data = serde_json::to_string(&ev).unwrap_or_default();
            Event::default().event(event_type).data(data)
        }
        Err(_) => Event::default().event("error").data("lag"),
    }
}

/// Costruisce il payload dell'evento SSE "final" a partire dallo stato del job
/// nel DB (run gia' terminato, nessun channel live attivo).
async fn playwright_final_payload(proj_pool: &sqlx::PgPool, run_id: Uuid) -> Value {
    let row_opt = sqlx::query("SELECT status, progress, input FROM jobs WHERE id = $1")
        .bind(run_id)
        .fetch_optional(proj_pool)
        .await
        .ok()
        .flatten();
    if let Some(row) = row_opt {
        let status: String = row.try_get("status").unwrap_or_else(|_| "unknown".into());
        let progress: Value = row.try_get("progress").unwrap_or_else(|_| json!({}));
        let input: Value = row.try_get("input").unwrap_or_else(|_| json!({}));
        let exit_code = input.get("exit_code").and_then(Value::as_i64).unwrap_or(-1);
        json!({
            "kind": "final",
            "job_id": run_id.to_string(),
            "status": status,
            "exit_code": exit_code,
            "progress": progress,
        })
    } else {
        json!({ "kind": "final", "job_id": run_id.to_string(), "status": "unknown" })
    }
}

#[derive(serde::Deserialize)]
pub struct ArtifactQuery {
    pub path: String,
}

pub async fn serve_playwright_artifact(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    axum::extract::Query(q): axum::extract::Query<ArtifactQuery>,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    use axum::http::header;
    use axum::response::IntoResponse;

    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;

    let canonical = resolve_artifact_path(&context, &q.path)?;

    let bytes = tokio::fs::read(&canonical)
        .await
        .map_err(|e| api_error(StatusCode::NOT_FOUND, format!("lettura file: {}", e)))?;

    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let mime = mime_for_extension(&ext);

    Ok(([(header::CONTENT_TYPE, mime)], bytes).into_response())
}

/// Path traversal guard per gli artifact Playwright: il path deve essere
/// relativo, non contenere `..`, puntare a test-results/ o playwright-report/ e
/// risolversi (canonicalize) dentro la project root. Ritorna il path canonico.
fn resolve_artifact_path(
    context: &crate::projects::ProjectContext,
    req_path: &str,
) -> Result<std::path::PathBuf, ApiError> {
    let rel = std::path::Path::new(req_path);
    if rel.is_absolute() || req_path.contains("..") {
        return Err(api_error(StatusCode::BAD_REQUEST, "path non valido"));
    }
    let allowed = req_path.contains("test-results/")
        || req_path.contains("playwright-report/")
        || req_path.contains("test-results\\")
        || req_path.contains("playwright-report\\");
    if !allowed {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "solo artifact Playwright sono accessibili",
        ));
    }
    let full = context.root_path.join(rel);
    let canonical = full
        .canonicalize()
        .map_err(|_| api_error(StatusCode::NOT_FOUND, "file non trovato"))?;
    let root_canonical = context.root_path.canonicalize().map_err(|_| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "project root non risolvibile",
        )
    })?;
    if !canonical.starts_with(&root_canonical) {
        return Err(api_error(StatusCode::FORBIDDEN, "path fuori dal progetto"));
    }
    Ok(canonical)
}

/// Mappa l'estensione file sul MIME type servito per gli artifact Playwright.
fn mime_for_extension(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "webm" => "video/webm",
        "mp4" => "video/mp4",
        "zip" => "application/zip",
        "html" => "text/html; charset=utf-8",
        _ => "application/octet-stream",
    }
}

pub async fn clear_playwright_runs(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    // Routing separazione DB (regola E): `jobs` e' tabella MIGRATA, vive nel pool
    // del progetto. DB progetto non disponibile -> errore tipizzato (503/404).
    let proj_pool =
        crate::project_db_routes::project_data_pool_from(&state.db, project_id).await?;
    let result =
        sqlx::query(r#"DELETE FROM jobs WHERE project_id = $1 AND kind ILIKE '%playwright%'"#)
            .bind(project_id)
            .execute(&proj_pool)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let deleted = result.rows_affected();
    // Dispatcher: notifica al frontend di svuotare il pannello Playwright in tempo reale
    nexus_events::dispatcher::emit(
        &state.project_channels,
        project_id,
        nexus_events::event::ProjectEvent::JobsCleared {
            job_kind: "playwright_test".to_string(),
            deleted,
        },
    );

    Ok(Json(json!({ "deleted": deleted })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    /// Semina un job e ritorna il suo id.
    async fn seed_job(pool: &PgPool, project_id: Uuid, kind: &str, input: Value) -> Uuid {
        sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO jobs (project_id, kind, status, input) \
             VALUES ($1, $2, 'passed', $3) RETURNING id",
        )
        .bind(project_id)
        .bind(kind)
        .bind(input)
        .fetch_one(pool)
        .await
        .expect("insert job")
    }

    /// I run Playwright si leggono dal DB del PROGETTO, filtrati per progetto e
    /// per kind. Il test gira sulla migrazione reale del set `project`
    /// (`PROJECT_MIGRATOR`, regola O): e' li' che `jobs` vive dopo la
    /// separazione, ed e' esattamente il fatto che il bug ignorava leggendola
    /// dal meta-pool. Se `jobs` uscisse dal set project, questo test cade.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn playwright_runs_letti_dal_db_progetto(pool: PgPool) {
        let progetto = Uuid::new_v4();
        let altro_progetto = Uuid::new_v4();

        let atteso = seed_job(
            &pool,
            progetto,
            "playwright_test",
            json!({ "label": "suite login", "command": "npx playwright test", "exit_code": 0 }),
        )
        .await;
        // Rumore che NON deve comparire: altro kind, e altro progetto.
        seed_job(&pool, progetto, "shadow_db_validation", json!({})).await;
        seed_job(&pool, altro_progetto, "playwright_test", json!({})).await;

        let runs = playwright_runs_for_project(&pool, progetto)
            .await
            .expect("lettura run playwright");

        assert_eq!(runs.len(), 1, "solo il job playwright di QUESTO progetto");
        let run = &runs[0];
        assert_eq!(run["id"], json!(atteso.to_string()));
        assert_eq!(run["label"], json!("suite login"));
        assert_eq!(run["status"], json!("passed"));
        // Campi che la copia nello snapshot non mappava: ora la fonte e' una sola.
        assert_eq!(run["command"], json!("npx playwright test"));
        assert_eq!(run["exitCode"], json!(0));
    }
}
