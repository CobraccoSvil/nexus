//! Tool `nexus_verify_change` (ADR 0019 L3 + ADR 0036): catena di verifica
//! post-modifica con fail-fast al primo step rosso.
//!
//! Gli step NON sono un vocabolario fisso: vengono dal PROFILO PER-AMBIENTE
//! inferito da un LLM che osserva il progetto (`verify_profile`, mig 0508).
//! Nessuna matrice statica linguaggio->comando (decisione utente): se il
//! profilo non e' disponibile il tool lo dichiara con esito strutturato,
//! senza comandi generici di ripiego.
//!
//! Esito STRUTTURATO (regole M e Q): il tool ritorna una
//! `nexus_types::tool_outcome::RispostaTool`, quindi «ho fallito» sta in un
//! CAMPO e non piu' in un marker anteposto al corpo JSON — che infatti torna a
//! essere un documento integro. Ogni step riporta `exit_code` — il CAMPO della
//! `RispostaTool` che `run_command` restituisce, non un numero ri-estratto dal
//! suo testo — piu' `build_errors` (punto unico
//! `nexus_agent_graph::count_build_errors`, stessa coppia dei criteri del
//! final_gate); il consumatore legge `passed`/`first_failure`, mai la prosa.
//!
//! Un report con `passed: false` e' un tool RIUSCITO, non fallito: la catena ha
//! MISURATO, e il rosso e' il suo risultato. E' la stessa distinzione di
//! `RispostaTool::comando` (un build che esce 1 e' un tool riuscito che riporta
//! un comando fallito). Il tool fallisce solo quando la verifica non e' POTUTA
//! partire: kill-switch, profilo assente, scope che non seleziona nulla.
//!
//! Precedenza comando per step (regola G/L): override locale in
//! `run_configurations` (role = nome step) > comando dello step nel profilo.

use super::*;

use crate::verify_profile::VerifyProfileStep;
use nexus_agent_tools::{input_contract::InputTool, tool_inputs::NexusVerifyChangeInput};
use nexus_types::tool_outcome::{NaturaFallimento, RispostaTool};

/// Costruisce l'esito FALLITO del tool: il corpo JSON che il modello legge, con
/// l'esito e la NATURA nei campi. Condiviso dai rami di errore qui sotto
/// (kill-switch disattivo, profilo assente, scope che non seleziona nulla),
/// che altrimenti sarebbero indistinguibili da un report riuscito per
/// anti-loop/supervisore/final_gate.
fn verify_failure(payload: Value, natura: NaturaFallimento) -> RispostaTool {
    RispostaTool::fallito(payload.to_string()).con_natura(natura)
}

/// Comando risolto per uno step, con la provenienza (per il report).
struct ResolvedCmd {
    command: String,
    source: &'static str, // "run_configuration" | "verify_profile"
}

/// Cio' che vale per TUTTI gli step della catena, risolto una volta sola.
struct StepConfig<'a> {
    /// `working_dir` esplicito del chiamante: vince su quello dello step.
    working_dir: Option<&'a str>,
    /// Bound globale DB-driven, usato dagli step che non propongono il proprio.
    step_timeout_s: u64,
    output_max_chars: usize,
}

/// Override locale per-progetto: run_configuration col role omonimo dello
/// step (vince SEMPRE sul comando del profilo: e' la scelta esplicita
/// dell'utente). `Ok(None)` = nessun override, si usa il profilo.
///
/// L'errore di query RISALE invece di degradare a `None`: prima un `.ok()` lo
/// appiattiva su «nessun override», cioe' il tool eseguiva il comando del
/// profilo al posto di quello scelto dall'utente e poi lo DICHIARAVA come tale
/// nel campo `command_source` del report. Un guasto del DB si presentava come
/// una configurazione diversa da quella vera.
async fn resolve_step_override(
    db: &sqlx::PgPool,
    project_id: uuid::Uuid,
    step: &str,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String, Vec<String>)> = sqlx::query_as(
        "SELECT command, args FROM run_configurations \
         WHERE project_id = $1 AND role = $2 \
         ORDER BY essential DESC, updated_at DESC LIMIT 1",
    )
    .bind(project_id)
    .bind(step)
    .fetch_optional(db)
    .await?;
    let Some((command, args)) = row else {
        return Ok(None);
    };
    let full = if args.is_empty() {
        command
    } else {
        format!("{} {}", command, args.join(" "))
    };
    Ok((!full.trim().is_empty()).then_some(full))
}

/// Il comando da eseguire per uno step, con la sua provenienza.
async fn risolvi_comando(
    db: &sqlx::PgPool,
    project_id: uuid::Uuid,
    profile_step: &VerifyProfileStep,
) -> Result<ResolvedCmd, sqlx::Error> {
    let override_utente = resolve_step_override(db, project_id, &profile_step.step).await?;
    Ok(match override_utente {
        Some(command) => ResolvedCmd {
            command,
            source: "run_configuration",
        },
        None => ResolvedCmd {
            command: profile_step.command.clone(),
            source: "verify_profile",
        },
    })
}

/// Estratto head+tail non distruttivo dell'output per il report (i totali dei
/// build sono IN FONDO: mai tagliare solo la coda).
fn output_excerpt(raw: &str, max_chars: usize) -> (String, bool) {
    let total = raw.chars().count();
    if total <= max_chars {
        return (raw.to_string(), false);
    }
    let head: String = raw.chars().take(max_chars / 2).collect();
    let tail: String = {
        let skip = total - max_chars / 2;
        raw.chars().skip(skip).collect()
    };
    (format!("{head}\n[... output troncato ...]\n{tail}"), true)
}

/// Gli step del profilo che lo scope richiesto seleziona.
///
/// Il confronto e' INSENSIBILE alle maiuscole perche' i valori promessi al
/// modello sono i nomi degli step del profilo copiati verbatim
/// (`agent_turn_setup::apply_verify_scope_enum`): con l'abbassamento a
/// minuscole di prima, un profilo con uno step `Lint` vedeva rifiutato proprio
/// il valore che il catalogo gli aveva appena dichiarato — la stessa divergenza
/// da cui l'enum dinamico e' nato.
fn seleziona_step<'a>(
    profile: &'a [VerifyProfileStep],
    scope: &str,
) -> Result<Vec<&'a VerifyProfileStep>, RispostaTool> {
    // Catena completa: tutti gli step del profilo, nell'ordine del profilo.
    if scope.eq_ignore_ascii_case("full") {
        return Ok(profile.iter().collect());
    }
    let selezione: Vec<&VerifyProfileStep> = if scope.eq_ignore_ascii_case("quick") {
        // Rapido: solo gli step che l'LLM ha marcato per il gate di chiusura.
        profile.iter().filter(|s| s.gate).collect()
    } else {
        profile
            .iter()
            .filter(|s| s.step.eq_ignore_ascii_case(scope))
            .collect()
    };
    if selezione.is_empty() {
        return Err(scope_senza_step(profile, scope));
    }
    Ok(selezione)
}

/// Lo scope non ha selezionato NESSUNO step.
///
/// RAMO NUDO CHIUSO: `quick` su un profilo in cui l'LLM non ha marcato alcuno
/// step come `gate` percorreva una catena VUOTA e usciva con `passed: true` e
/// `steps: []` — un verde che nessuno aveva misurato, consegnato all'agente
/// come prova oggettiva che la modifica compila. E' rimediabile ED e' detto
/// come: `full`, o uno degli step elencati.
fn scope_senza_step(profile: &[VerifyProfileStep], scope: &str) -> RispostaTool {
    let available: Vec<&str> = profile.iter().map(|s| s.step.as_str()).collect();
    let (error, detail) = if scope.eq_ignore_ascii_case("quick") {
        (
            "empty_scope",
            "scope 'quick' non seleziona nessuno step: il profilo di questo progetto non marca \
             nessuno step per il gate di chiusura, quindi la catena non verificherebbe nulla. \
             Richiama il tool con scope 'full', oppure con uno degli step in available_steps."
                .to_string(),
        )
    } else {
        (
            "invalid_scope",
            format!(
                "scope '{scope}' non presente nel profilo: usa 'quick', 'full' oppure uno degli \
                 step elencati in available_steps."
            ),
        )
    };
    let payload = serde_json::json!({
        "error": error,
        "detail": detail,
        "available_steps": available,
    });
    verify_failure(payload, NaturaFallimento::Rimediabile)
}

/// Riga di report per uno step il cui comando non si e' potuto RISOLVERE.
///
/// Non c'e' `command` perche' non se ne conosce nessuno di legittimo: eseguire
/// quello del profilo significherebbe ignorare in silenzio una scelta
/// dell'utente che il DB non ha saputo dire.
fn riga_override_illeggibile(step: &str, e: &sqlx::Error) -> Value {
    serde_json::json!({
        "step": step,
        "passed": false,
        "skipped_reason": null,
        "detail": format!("override run_configurations non leggibile: {e}"),
    })
}

/// Esegue UNO step e ne compone la riga di report. Il `bool` dice se e' verde.
async fn esegui_step(
    ctx: &AgentToolContext,
    cfg: &StepConfig<'_>,
    profile_step: &VerifyProfileStep,
) -> (Value, bool) {
    match risolvi_comando(&ctx.db, ctx.project_id, profile_step).await {
        Ok(resolved) => esegui_comando_step(ctx, cfg, profile_step, &resolved).await,
        Err(e) => (riga_override_illeggibile(&profile_step.step, &e), false),
    }
}

/// Lancia il comando risolto e ne traduce l'esito in una riga di report.
async fn esegui_comando_step(
    ctx: &AgentToolContext,
    cfg: &StepConfig<'_>,
    profile_step: &VerifyProfileStep,
    resolved: &ResolvedCmd,
) -> (Value, bool) {
    let step = profile_step.step.as_str();
    let mut tool_input = serde_json::json!({ "command": resolved.command });
    // working_dir: input esplicito del chiamante > working_dir dello step.
    if let Some(wd) = cfg.working_dir.or(profile_step.working_dir.as_deref()) {
        tool_input["working_dir"] = serde_json::json!(wd);
    }
    // Timeout per-step: quello proposto dal profilo per QUESTO step, col bound
    // globale DB-driven come default (il run_command ha i suoi probe interni):
    // allo scadere lo step fallisce con motivo strutturato.
    let effective_timeout_s = profile_step
        .timeout_s
        .map(|t| t.max(1.0) as u64)
        .unwrap_or(cfg.step_timeout_s);
    let started = std::time::Instant::now();
    let attesa = tokio::time::timeout(
        std::time::Duration::from_secs(effective_timeout_s),
        super::command::tool_run_command(ctx, &tool_input),
    )
    .await;
    let duration_ms = started.elapsed().as_millis() as u64;
    let Ok(risposta) = attesa else {
        let scaduto = serde_json::json!({
            "step": step,
            "command": resolved.command,
            "command_source": resolved.source,
            "passed": false,
            "skipped_reason": null,
            "timeout": true,
            "duration_ms": duration_ms,
            "detail": format!("step oltre il timeout ({effective_timeout_s}s)"),
        });
        return (scaduto, false);
    };

    // Esito STRUTTURATO: exit_code dal CAMPO della risposta (regola Q) + rete
    // build_errors (exit 0 bugiardo di certi bundler, stessa coppia del
    // final_gate). Ri-estrarlo dal testo, com'era, faceva dipendere l'esito
    // dello step dal primo "EXIT CODE:" che comparisse nella stringa — compreso
    // quello di un hint scritto in `nexus_command_hints`.
    let exit_code = risposta.exit_code;
    // Il tool ha DICHIARATO di non aver potuto eseguire (comando rifiutato,
    // probe scaduto, spawn fallito): non c'e' exit code perche' non c'e' stata
    // esecuzione, ed e' un'informazione diversa da "eseguito senza stato
    // d'uscita". Il report la espone invece di appiattirla su un null.
    let invocation_failed = risposta.esito.e_fallito();
    let build_errors = nexus_agent_graph::count_build_errors(&risposta.testo);
    let step_ok = exit_code == Some(0) && build_errors == 0;
    let (excerpt, truncated) = output_excerpt(&risposta.testo, cfg.output_max_chars);
    let riga = serde_json::json!({
        "step": step,
        "command": resolved.command,
        "command_source": resolved.source,
        "passed": step_ok,
        "exit_code": exit_code,
        "invocation_failed": invocation_failed,
        "build_errors": build_errors,
        "duration_ms": duration_ms,
        "output_excerpt": excerpt,
        "output_truncated": truncated,
    });
    (riga, step_ok)
}

/// Percorre la catena con fail-fast: dopo il primo rosso gli step restanti sono
/// marcati `skipped`, non eseguiti. Ritorna le righe di report e il nome del
/// primo step rosso (`None` = catena verde).
async fn esegui_catena(
    ctx: &AgentToolContext,
    cfg: &StepConfig<'_>,
    steps: &[&VerifyProfileStep],
) -> (Vec<Value>, Option<String>) {
    let mut report_steps: Vec<Value> = Vec::with_capacity(steps.len());
    let mut first_failure: Option<String> = None;
    for profile_step in steps {
        let step = profile_step.step.as_str();
        if first_failure.is_some() {
            report_steps.push(serde_json::json!({
                "step": step,
                "skipped_reason": "fail_fast",
            }));
            continue;
        }
        let (riga, step_ok) = esegui_step(ctx, cfg, profile_step).await;
        if !step_ok {
            first_failure = Some(step.to_string());
        }
        report_steps.push(riga);
    }
    (report_steps, first_failure)
}

/// I due limiti DB-driven della catena (timeout per-step, taglio dell'estratto).
async fn limiti_catena(db: &sqlx::PgPool) -> (u64, usize) {
    let step_timeout_s = nexus_auth::get_setting(db, "agent.verify.step_timeout_s")
        .await
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(180)
        .max(10);
    let output_max_chars = nexus_auth::get_setting(db, "agent.verify.output_max_chars")
        .await
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(4000)
        .max(200);
    (step_timeout_s, output_max_chars)
}

/// Kill-switch DB-driven della catena.
///
/// La natura e' DEL SISTEMA: e' una decisione dell'admin, e ne' correggere la
/// chiamata ne' ritentarla la cambia.
async fn catena_abilitata(db: &sqlx::PgPool) -> Result<(), RispostaTool> {
    if nexus_auth::get_bool_setting_or(db, "agent.verify.enabled", false).await {
        return Ok(());
    }
    let payload = serde_json::json!({
        "error": "verify_disabled",
        "detail": "agent.verify.enabled non attivo: catena di verifica disabilitata dall'admin."
    });
    Err(verify_failure(payload, NaturaFallimento::DelSistema))
}

/// Il profilo per-ambiente del progetto (ADR 0036): inferito da LLM alla prima
/// richiesta, poi cache su tabella con invalidazione deterministica. Il tool
/// puo' triggerare l'inferenza (ha meta-db, neural e root nel contesto).
///
/// Profilo vuoto = NESSUN comando generico di ripiego (decisione utente): esito
/// strutturato onesto, il chiamante sa che la verifica non e' partita. La natura
/// e' DEL SISTEMA perche' le cause che `ensure_profile` appiattisce su un elenco
/// vuoto — kill-switch `agent.verify_infer`, DB muto, inferenza LLM non
/// riuscita — non sono nessuna delle due cose che l'agente potrebbe fare:
/// correggere la chiamata, o ritentarla.
async fn profilo_del_progetto(
    ctx: &AgentToolContext,
) -> Result<Vec<VerifyProfileStep>, RispostaTool> {
    let profile =
        crate::verify_profile::ensure_profile(&ctx.db, &ctx.neural, ctx.project_id, &ctx.root_path)
            .await;
    if !profile.is_empty() {
        return Ok(profile);
    }
    let payload = serde_json::json!({
        "error": "profile_unavailable",
        "detail": concat!(
            "Profilo di verifica dell'ambiente non disponibile: inferenza LLM non ",
            "riuscita, oppure agent.verify_infer.enabled non attivo, oppure nessun ",
            "profilo salvato. Chiedi all'admin di attivare l'inferenza, oppure ",
            "definisci run_configurations con role di verifica.",
        ),
    });
    Err(verify_failure(payload, NaturaFallimento::DelSistema))
}

/// Lo scope richiesto, col default dell'handler.
///
/// Il contratto non dichiara un default per `scope`: lo sceglie l'handler, e
/// sceglie il piu' completo — uno scope omesso non deve produrre una verifica
/// piu' debole di quella che il chiamante crede di aver chiesto.
fn scope_richiesto(params: &NexusVerifyChangeInput) -> String {
    params
        .scope
        .as_ref()
        .map(|s| s.come_stringa().trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("full")
        .to_string()
}

pub(super) async fn tool_nexus_verify_change(
    ctx: &AgentToolContext,
    input: &Value,
) -> RispostaTool {
    let params = match NexusVerifyChangeInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    if let Err(risposta) = catena_abilitata(&ctx.db).await {
        return risposta;
    }
    let scope = scope_richiesto(&params);
    let profile = match profilo_del_progetto(ctx).await {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let available: Vec<&str> = profile.iter().map(|s| s.step.as_str()).collect();
    let steps = match seleziona_step(&profile, &scope) {
        Ok(s) => s,
        Err(risposta) => return risposta,
    };

    let (step_timeout_s, output_max_chars) = limiti_catena(&ctx.db).await;
    let cfg = StepConfig {
        working_dir: params.working_dir.as_deref(),
        step_timeout_s,
        output_max_chars,
    };
    let (report_steps, first_failure) = esegui_catena(ctx, &cfg, &steps).await;

    // Il tool ha MISURATO: un rosso e' il suo risultato, non un suo fallimento
    // (stessa distinzione di `RispostaTool::comando`). Nessun `exit_code`
    // sull'aggregato: la catena esegue N processi e un numero solo andrebbe
    // inventato — chi vuole gli stati d'uscita li trova per step nel report.
    RispostaTool::riuscito(
        serde_json::json!({
            "scope": scope,
            "passed": first_failure.is_none(),
            "first_failure": first_failure,
            "steps": report_steps,
            "profile_steps_available": available,
        })
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Step di profilo minimale per i test del criterio di selezione.
    fn step_di_prova(nome: &str, gate: bool) -> VerifyProfileStep {
        VerifyProfileStep {
            step: nome.to_string(),
            command: format!("echo {nome}"),
            working_dir: None,
            timeout_s: None,
            gate,
            rationale: None,
            baseline_exit_code: None,
            probe: None,
        }
    }

    #[test]
    fn verify_failure_dichiara_il_fallimento_e_preserva_il_payload() {
        // Chiama il PRODUTTORE reale usato dai rami di errore del tool: se
        // domani uno di quei rami smettesse di passare da qui, resterebbe
        // invisibile ad anti-loop/supervisore/final_gate (regola M).
        let out = verify_failure(
            serde_json::json!({
                "error": "profile_unavailable",
                "detail": "profilo assente",
            }),
            NaturaFallimento::DelSistema,
        );
        assert!(out.esito.e_fallito(), "l'esito sta nel campo: {out:?}");
        assert_eq!(
            out.natura,
            Some(NaturaFallimento::DelSistema),
            "la natura la dichiara chi conosce la causa: {out:?}"
        );
        // Il payload e' un JSON INTEGRO: senza marker in testa non c'e' nulla da
        // togliere prima di rileggerlo.
        let parsed: Value =
            serde_json::from_str(&out.testo).expect("il corpo di un errore e' JSON valido");
        assert_eq!(parsed["error"], "profile_unavailable");
    }

    #[test]
    fn excerpt_preserva_testa_e_coda() {
        let raw = format!("{}FINE-CODA", "x".repeat(10_000));
        let (excerpt, truncated) = output_excerpt(&raw, 400);
        assert!(truncated);
        assert!(excerpt.starts_with("xxxx"));
        assert!(
            excerpt.ends_with("FINE-CODA"),
            "la coda (totali build) va preservata"
        );
        assert!(excerpt.contains("[... output troncato ...]"));
    }

    #[test]
    fn excerpt_sotto_soglia_invariato() {
        let (excerpt, truncated) = output_excerpt("breve", 400);
        assert!(!truncated);
        assert_eq!(excerpt, "breve");
    }

    /// `quick` su un profilo senza step di gate NON e' una catena verde: e' una
    /// catena che non gira.
    ///
    /// MUTAZIONE: rimettendo il `filter(|s| s.gate).collect()` senza il
    /// controllo sul vuoto, la selezione ritorna `Ok(vec![])`, la catena non
    /// esegue nulla e il report esce `passed: true` con `steps: []`.
    #[test]
    fn quick_senza_step_di_gate_non_e_una_catena_verde() {
        let profilo = vec![step_di_prova("typecheck", false), step_di_prova("test", false)];
        let errore = seleziona_step(&profilo, "quick").expect_err("nessuno step selezionato");
        assert!(errore.esito.e_fallito());
        assert_eq!(
            errore.natura,
            Some(NaturaFallimento::Rimediabile),
            "l'agente rimedia scegliendo un altro scope"
        );
        let parsed: Value = serde_json::from_str(&errore.testo).expect("corpo JSON");
        assert_eq!(parsed["error"], "empty_scope");
        // Rimediabile ED e' detto COME: il messaggio nomina lo scope da usare e
        // il campo che elenca le alternative.
        let detail = parsed["detail"].as_str().unwrap_or_default();
        assert!(detail.contains("'full'"), "detail: {detail}");
        assert!(detail.contains("available_steps"), "detail: {detail}");
        assert_eq!(parsed["available_steps"], serde_json::json!(["typecheck", "test"]));
    }

    /// Uno step marcato per il gate rende `quick` una selezione legittima.
    #[test]
    fn quick_seleziona_i_soli_step_di_gate() {
        let profilo = vec![step_di_prova("typecheck", true), step_di_prova("e2e", false)];
        let scelti = seleziona_step(&profilo, "quick").expect("uno step di gate c'e'");
        assert_eq!(scelti.len(), 1);
        assert_eq!(scelti[0].step, "typecheck");
        // `full` li prende tutti, nell'ordine del profilo.
        let tutti = seleziona_step(&profilo, "full").expect("full non filtra");
        assert_eq!(tutti.len(), 2);
    }

    /// Il nome dello step si confronta senza guardare le maiuscole: quel che il
    /// catalogo promette al modello sono i nomi del profilo copiati verbatim,
    /// e prima lo scope veniva abbassato a minuscole prima del confronto.
    ///
    /// MUTAZIONE: riportando il confronto a `s.step == scope` con lo scope
    /// abbassato, questo test rosseggia con `invalid_scope` su un valore che il
    /// catalogo dichiarava ammesso.
    #[test]
    fn lo_step_del_profilo_si_riconosce_a_qualunque_cassa() {
        let profilo = vec![step_di_prova("Lint-Frontend", false)];
        let scelti = seleziona_step(&profilo, "Lint-Frontend").expect("valore del catalogo");
        assert_eq!(scelti.len(), 1);
        // E uno scope inventato resta rifiutato, con l'elenco per rimediare.
        let errore = seleziona_step(&profilo, "inventato").expect_err("fuori profilo");
        let parsed: Value = serde_json::from_str(&errore.testo).expect("corpo JSON");
        assert_eq!(parsed["error"], "invalid_scope");
        assert_eq!(parsed["available_steps"], serde_json::json!(["Lint-Frontend"]));
    }

    #[sqlx::test]
    async fn resolve_step_override_vince_sul_profilo(pool: sqlx::PgPool) {
        // L'override utente (run_configurations, role = nome step) vince sul
        // comando del profilo inferito; senza override -> None (si usa il
        // profilo). NESSUN terzo livello statico (ADR 0036).
        sqlx::query(
            "CREATE TABLE run_configurations ( \
                 id UUID PRIMARY KEY DEFAULT gen_random_uuid(), \
                 project_id UUID NOT NULL, \
                 label TEXT NOT NULL DEFAULT '', \
                 command TEXT NOT NULL, \
                 args TEXT[] NOT NULL DEFAULT '{}', \
                 role TEXT, \
                 essential BOOLEAN NOT NULL DEFAULT FALSE, \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW() \
             )",
        )
        .execute(&pool)
        .await
        .expect("create run_configurations");
        let pid = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO run_configurations (project_id, command, args, role) VALUES \
             ($1, 'cargo', ARRAY['nextest','run'], 'test')",
        )
        .bind(pid)
        .execute(&pool)
        .await
        .expect("seed run_config");

        // Override locale presente.
        let r = resolve_step_override(&pool, pid, "test")
            .await
            .expect("query riuscita")
            .expect("override");
        assert_eq!(r, "cargo nextest run");

        // Altro progetto senza run_config -> None (si usa il profilo).
        assert!(resolve_step_override(&pool, uuid::Uuid::new_v4(), "test")
            .await
            .expect("query riuscita")
            .is_none());
        // Step senza role corrispondente -> None.
        assert!(resolve_step_override(&pool, pid, "lint")
            .await
            .expect("query riuscita")
            .is_none());
    }

    /// Un DB che non risponde NON diventa «nessun override»: l'errore risale, e
    /// il chiamante lo dichiara nel report invece di eseguire il comando del
    /// profilo spacciandolo per la scelta dell'utente.
    ///
    /// MUTAZIONE: rimettendo `.ok().flatten()` in `resolve_step_override`,
    /// questo test rosseggia perche' la query fallita torna `Ok(None)`.
    #[sqlx::test]
    async fn un_db_muto_non_diventa_assenza_di_override(pool: sqlx::PgPool) {
        // Nessuna `run_configurations` in questo DB: la query fallisce, e il
        // fallimento e' cio' che deve arrivare al chiamante.
        let e = resolve_step_override(&pool, uuid::Uuid::new_v4(), "test")
            .await
            .expect_err("la tabella non esiste: la query fallisce");
        let riga = riga_override_illeggibile("test", &e);
        assert_eq!(riga["passed"], serde_json::json!(false));
        assert!(
            riga["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("run_configurations"),
            "il report nomina cio' che non si e' potuto leggere: {riga}"
        );
    }
}
