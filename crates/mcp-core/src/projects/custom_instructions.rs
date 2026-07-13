// Istruzioni personalizzate per-progetto e auto-generazione profili AI.

use super::*;

/// GET /api/projects/:id/custom-instructions — legge le istruzioni per-progetto
pub async fn get_custom_instructions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    // Verifica accesso
    load_project_context(&state.db, project_id, user_id).await?;

    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT custom_instructions FROM projects WHERE id = $1")
            .bind(project_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let instructions = row.and_then(|(v,)| v).unwrap_or_default();
    Ok(Json(json!({ "customInstructions": instructions })))
}

/// PATCH /api/projects/:id/custom-instructions — aggiorna le istruzioni per-progetto
pub async fn update_custom_instructions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    // Verifica accesso (deve essere owner o member con write)
    load_project_context(&state.db, project_id, user_id).await?;

    let instructions = body
        .get("customInstructions")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    sqlx::query("UPDATE projects SET custom_instructions = $2 WHERE id = $1")
        .bind(project_id)
        .bind(instructions)
        .execute(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    tracing::info!(
        "custom_instructions aggiornate per project_id={}",
        project_id
    );
    Ok(Json(
        json!({ "ok": true, "customInstructions": instructions }),
    ))
}

/// Genera istruzioni operative per-progetto da iniettare nel system prompt dell'agente.
pub(super) fn auto_generate_custom_instructions(
    root: &std::path::Path,
    frameworks: &[String],
    dependencies: &serde_json::Value,
) -> Option<String> {
    let mut rules: Vec<String> = Vec::new();
    let mut verify_cmd: Option<String> = None;

    let framework_strs: Vec<&str> = frameworks.iter().map(|s| s.as_str()).collect();

    // ── Rilevamento stack Node/TypeScript ──────────────────────────────────────
    let node_scripts = dependencies
        .get("node")
        .and_then(|n| n.get("scripts"))
        .and_then(|s| s.as_object());

    if let Some(scripts) = node_scripts {
        let has_verify = scripts.contains_key("verify");
        let has_typecheck = scripts.contains_key("typecheck");
        let has_build = scripts.contains_key("build");
        let pkg_manager = if root.join("pnpm-lock.yaml").exists() {
            "pnpm"
        } else if root.join("yarn.lock").exists() {
            "yarn"
        } else {
            "npm run"
        };

        if has_verify {
            verify_cmd = Some(format!("{pkg_manager} verify"));
        } else if has_typecheck && has_build {
            verify_cmd = Some(format!("{pkg_manager} typecheck && {pkg_manager} build"));
        } else if has_build {
            verify_cmd = Some(format!("{pkg_manager} build"));
        }

        if let Some(ref cmd) = verify_cmd {
            let is_next = framework_strs
                .iter()
                .any(|f| f.to_lowercase().contains("next"));
            let is_react = framework_strs
                .iter()
                .any(|f| f.to_lowercase().contains("react"));
            let target = if is_next {
                "Next.js"
            } else if is_react {
                "React"
            } else {
                "il progetto"
            };
            rules.push(format!(
                "VERIFICA OBBLIGATORIA: dopo aver modificato file TypeScript, TSX, JSX o CSS in {target}, \
                esegui `{cmd}` dalla directory root del progetto prima di dichiarare il task completato. \
                Se il comando fallisce, correggi tutti gli errori prima di concludere. \
                NON dichiarare mai 'task completato' senza aver verificato che il build è pulito."
            ));
        }

        let is_next = framework_strs
            .iter()
            .any(|f| f.to_lowercase().contains("next"));
        if is_next {
            rules.push(
                "INTEGRITÀ NEXT.JS: quando rimuovi o sposti componenti, verifica che TUTTI i link/href \
                che puntavano a quell'elemento (es. ancore #id, import, route) siano aggiornati di conseguenza. \
                Verifica che i CSS module usino solo classi definite nel file .module.css corrispondente.".to_string()
            );
        }
    }

    // ── Rilevamento stack Rust ─────────────────────────────────────────────────
    let has_cargo = root.join("Cargo.toml").exists();
    if has_cargo && verify_cmd.is_none() {
        rules.push(
            "VERIFICA OBBLIGATORIA: dopo modifiche a file Rust (.rs), esegui `cargo check` \
            dalla directory root prima di dichiarare il task completato. \
            Se ci sono errori di compilazione, correggili prima di concludere."
                .to_string(),
        );
    }

    // ── Rilevamento stack Python ───────────────────────────────────────────────
    let has_pyproject = root.join("pyproject.toml").exists();
    let has_requirements = root.join("requirements.txt").exists();
    if (has_pyproject || has_requirements) && verify_cmd.is_none() && !has_cargo {
        let test_cmd = if has_pyproject {
            "pytest"
        } else {
            "python -m pytest"
        };
        rules.push(format!(
            "VERIFICA OBBLIGATORIA: dopo modifiche a file Python (.py), esegui `{test_cmd}` \
            per verificare che i test passino prima di dichiarare il task completato."
        ));
    }

    if rules.is_empty() {
        None
    } else {
        Some(format!(
            "=== ISTRUZIONI SPECIFICHE DEL PROGETTO ===\n{}\n=== FINE ISTRUZIONI PROGETTO ===",
            rules
                .iter()
                .enumerate()
                .map(|(i, r)| format!("{}. {}", i + 1, r))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }
}

/// Crea automaticamente profili utente basati sullo stack tecnico rilevato dall'analisi progetto.
pub(super) async fn auto_create_profiles_from_analysis(
    db: &PgPool,
    cache: &crate::prompt_templates::TemplateCache,
    user_id: Uuid,
    project_name: &str,
    languages: &[String],
    frameworks: &[String],
) {
    struct ProfileSpec {
        name: &'static str,
        emoji: &'static str,
        description: &'static str,
        template_key: &'static str,
        triggers: &'static [&'static str],
    }

    let specs: &[ProfileSpec] = &[
        ProfileSpec {
            name: "Sviluppatore .NET / C#",
            emoji: "🔷",
            description: "Specializzato in C#, ASP.NET Core, Entity Framework e architetture .NET",
            template_key: "profile.developer_csharp_dotnet",
            triggers: &[
                ".NET",
                "C#",
                "ASP.NET",
                "Entity Framework",
                "Blazor",
                "MAUI",
                "MSBuild",
            ],
        },
        ProfileSpec {
            name: "Sviluppatore React / TypeScript",
            emoji: "⚛️",
            description:
                "Specializzato in React, TypeScript, Next.js e ecosistema frontend moderno",
            template_key: "profile.developer_react_typescript",
            triggers: &["React", "Next.js", "TypeScript", "Vite", "Remix", "Gatsby"],
        },
        ProfileSpec {
            name: "Sviluppatore Python",
            emoji: "🐍",
            description: "Specializzato in Python, Django, FastAPI e data engineering",
            template_key: "profile.developer_python",
            triggers: &[
                "Python",
                "Django",
                "FastAPI",
                "Flask",
                "Pydantic",
                "SQLAlchemy",
            ],
        },
        ProfileSpec {
            name: "Sviluppatore Rust",
            emoji: "🦀",
            description: "Specializzato in Rust, Axum, Tokio e sistemi ad alte prestazioni",
            template_key: "profile.developer_rust",
            triggers: &["Rust", "Axum", "Tokio", "Cargo", "Actix"],
        },
        ProfileSpec {
            name: "DevOps / Infrastruttura",
            emoji: "⚙️",
            description: "Specializzato in Docker, Kubernetes, CI/CD e infrastruttura cloud",
            template_key: "profile.devops_infrastructure",
            triggers: &[
                "Docker",
                "Kubernetes",
                "Terraform",
                "Ansible",
                "Helm",
                "GitHub Actions",
                "GitLab CI",
            ],
        },
        ProfileSpec {
            name: "Sviluppatore Vue / Nuxt",
            emoji: "💚",
            description: "Specializzato in Vue.js, Nuxt, Pinia e frontend Vue ecosystem",
            template_key: "profile.developer_vue_nuxt",
            triggers: &["Vue", "Nuxt", "Pinia", "Vuex", "Quasar"],
        },
        ProfileSpec {
            name: "Sviluppatore Mobile",
            emoji: "📱",
            description: "Specializzato in React Native, Flutter e sviluppo mobile cross-platform",
            template_key: "profile.developer_mobile",
            triggers: &["React Native", "Flutter", "Expo", "Capacitor", "Ionic"],
        },
        ProfileSpec {
            name: "Data Science / ML",
            emoji: "🧠",
            description: "Specializzato in machine learning, data analysis e Python scientifico",
            template_key: "profile.data_science_ml",
            triggers: &[
                "PyTorch",
                "TensorFlow",
                "scikit-learn",
                "Pandas",
                "NumPy",
                "Jupyter",
                "Keras",
                "HuggingFace",
            ],
        },
    ];

    let all_stack: Vec<String> = frameworks.iter().chain(languages.iter()).cloned().collect();

    for spec in specs {
        let matches = spec.triggers.iter().any(|t| {
            all_stack
                .iter()
                .any(|s| s.to_lowercase().contains(&t.to_lowercase()))
        });
        if !matches {
            continue;
        }

        let existing: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM user_profiles WHERE user_id = $1 AND name = $2")
                .bind(user_id)
                .bind(spec.name)
                .fetch_optional(db)
                .await
                .unwrap_or(None);

        if existing.is_some() {
            continue;
        }

        let profile_id = Uuid::new_v4();
        let description = format!(
            "{} — rilevato nel progetto '{}'",
            spec.description, project_name
        );
        let system_prompt =
            crate::prompt_templates::get_template_or_default(db, cache, spec.template_key).await;
        let _ = sqlx::query(
            "INSERT INTO user_profiles (id, user_id, name, avatar_emoji, description, system_prompt, \
             default_provider, default_model, default_automation, is_default, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, NULL, NULL, NULL, FALSE, NOW(), NOW())"
        )
        .bind(profile_id)
        .bind(user_id)
        .bind(spec.name)
        .bind(spec.emoji)
        .bind(&description)
        .bind(&system_prompt)
        .execute(db)
        .await;

        tracing::info!(
            user_id = %user_id,
            profile = spec.name,
            project = project_name,
            "Auto-created profile from project analysis"
        );
    }
}

/// Estrae il testo generato ripulito da un value neural (fn, non closure: non
/// viene "mossa" nel loop di failover).
fn clean_generated_text(v: &Value) -> String {
    v["content"]
        .as_str()
        .unwrap_or("")
        .trim()
        .trim_matches('"')
        .to_string()
}

/// POST /api/ai/generate-prompt — genera un system prompt per un profilo AI
pub async fn generate_system_prompt(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    Json(body): Json<GeneratePromptRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let desc_line = body
        .description
        .as_deref()
        .map(|d| format!("Descrizione: {d}\n"))
        .unwrap_or_default();

    let prompt_tpl = crate::prompt_templates::get_template_or_default(
        &state.db,
        &state.template_cache,
        "automation.profile_system_prompt_generator",
    )
    .await;
    let prompt = prompt_tpl
        .replace("{{name}}", &body.profile_name)
        .replace("{{desc}}", &desc_line);

    // Un value neural di FALLIMENTO (regola M: neural_value_is_failure — include il
    // CONTENT VUOTO di Gemini e i "[Error:...]") non deve MAI diventare il system
    // prompt: era il leak "[Error:...]" salvato come prompt. Se l'utente ha PINNATO
    // provider+model, si rispetta la sua scelta (una chiamata); altrimenti FAILOVER
    // tier-aware (punto unico complete_for_purpose_with_failover, regola L).
    let neural = &state.orchestrator.neural;
    let text = if let (Some(p), Some(m)) = (body.provider.as_deref(), body.model.as_deref()) {
        let result = neural
            .generate_completion(p, m, &prompt)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        if crate::orchestrator::neural_value_is_failure(&result) {
            return Err(api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Il provider selezionato non ha prodotto un prompt valido".to_string(),
            ));
        }
        clean_generated_text(&result)
    } else {
        use crate::internal_routing::{
            complete_for_purpose_with_failover, AttemptOutcome, PurposeFailoverError,
        };
        let attempt = |prov: String, mdl: String| {
            let prompt = &prompt;
            async move {
                match neural.generate_completion(&prov, &mdl, prompt).await {
                    Ok(v) if !crate::orchestrator::neural_value_is_failure(&v) => {
                        AttemptOutcome::Done(clean_generated_text(&v))
                    }
                    _ => AttemptOutcome::Failover,
                }
            }
        };
        match complete_for_purpose_with_failover(&state.db, "custom_instructions", attempt).await {
            Ok(t) => t,
            Err(PurposeFailoverError::AllCandidatesFailed) => {
                return Err(api_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Nessun provider del tier ha prodotto un prompt valido".to_string(),
                ))
            }
            Err(PurposeFailoverError::NoCandidate(_)) => {
                return Err(api_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Nessun modello risolvibile per 'custom_instructions'".to_string(),
                ))
            }
        }
    };

    Ok(Json(serde_json::json!({ "text": text })))
}
