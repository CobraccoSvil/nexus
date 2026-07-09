//! Punto unico anti-persistenza dei placeholder di redazione (regola L).
//!
//! Incidente Beaty-Book 2026-07-02 (run 4360e0ee): il secret scanner del
//! gateway redige le connection string col placeholder FISSO
//! `[REDACTED:db_connection_string]`, che NON entra nella RedactionMap e non
//! e' quindi reversibile; la reidratazione post-flight tocca comunque solo
//! `response.content`, mai gli argomenti delle tool call. Il modello ha
//! copiato il placeholder come valore letterale di DATABASE_URL in
//! `run_service` e in `backend/.env`: pg-connection-string lo ha interpretato
//! come URL relativa (`new URL(str, 'postgres://base')` -> host `base`) e il
//! backend e' morto con `getaddrinfo ENOTFOUND base`. Stessa dinamica con
//! `[REDACTED:email_pii]` persistito nei sorgenti e nel DB applicativo.
//!
//! Difesa alla causa radice (regola H), due gambe:
//! 1. Iniezione server-side della DATABASE_URL del DB progetto
//!    (`ensure_project_db_url`): attiva in `run_command` e, per i processi
//!    long-running, dentro `spawn_agent_process` (punto unico che copre tool,
//!    wizard, pannello e run config). Il modello non deve MAI comporla.
//! 2. Questo guard rifiuta esecuzione/persistenza di input che contengono un
//!    placeholder di redazione, con un tool_result esplicativo che indirizza
//!    il modello al meccanismo corretto (env iniettate, `request_port`).
//!
//! La reidratazione dei segreti negli argomenti delle tool call e' stata
//! SCARTATA deliberatamente: il segreto tornerebbe in chiaro negli step
//! persistiti (`agent_steps`) e nei log (violazione regola F). Un placeholder
//! non deve mai diventare un valore: se il valore serve davvero, il canale
//! giusto e' l'iniezione server-side o la configurazione di progetto.
//!
//! Formati riconosciuti (entrambi i layer di redazione del gateway):
//! - `[REDACTED:<tipo>]`      — secret scanner (irreversibile by design);
//! - `__NEXUS_<KIND>_<N>__`   — RedactionMap (anonymizer/Presidio): reversibile
//!   SOLO in `response.content`; dentro una tool call resta un segnaposto.

use serde_json::json;

/// Cerca il primo placeholder di redazione nel testo. Ritorna il placeholder
/// completo se trovato. Funzione pura (testabile senza DB).
pub fn find_redacted_placeholder(text: &str) -> Option<String> {
    find_secret_scanner_placeholder(text).or_else(|| find_redaction_map_placeholder(text))
}

/// `[REDACTED:<tipo>]` — il tipo e' uno slug snake_case corto (es.
/// `db_connection_string`). Si pretende la chiusura `]` immediata dopo lo
/// slug: una `]` lontana o caratteri estranei non sono un placeholder genuino.
fn find_secret_scanner_placeholder(text: &str) -> Option<String> {
    const MARKER: &str = "[REDACTED:";
    let mut search_from = 0;
    while let Some(rel) = text[search_from..].find(MARKER) {
        let start = search_from + rel;
        let after = &text[start + MARKER.len()..];
        let kind_len = after
            .bytes()
            .take_while(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
            .count();
        if kind_len > 0 && kind_len <= 48 && after.as_bytes().get(kind_len) == Some(&b']') {
            return Some(text[start..start + MARKER.len() + kind_len + 1].to_string());
        }
        search_from = start + MARKER.len();
    }
    None
}

/// `__NEXUS_<KIND>_<N>__` — placeholder della RedactionMap (kind maiuscolo +
/// contatore). Reidratato solo in `response.content`: se arriva qui dentro un
/// input tool e' un segnaposto copiato, non un valore.
fn find_redaction_map_placeholder(text: &str) -> Option<String> {
    const MARKER: &str = "__NEXUS_";
    let mut search_from = 0;
    while let Some(rel) = text[search_from..].find(MARKER) {
        let start = search_from + rel;
        let after = &text[start + MARKER.len()..];
        // <KIND>_<N>__ : maiuscole/cifre/underscore, poi la chiusura `__`.
        let body_len = after
            .bytes()
            .take_while(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'_')
            .count();
        // La chiusura `__` e' inclusa nel body (underscore); serve almeno
        // KIND(1) + `_` + N(1) + `__` e l'ultimo carattere digit prima di `__`.
        if (5..=64).contains(&body_len) {
            let body = &after[..body_len];
            if let Some(core) = body.strip_suffix("__") {
                if core.chars().last().is_some_and(|c| c.is_ascii_digit()) {
                    return Some(format!("{MARKER}{body}"));
                }
            }
        }
        search_from = start + MARKER.len();
    }
    None
}

/// CODICE STRUTTURATO (regola M) del rifiuto per placeholder di redazione,
/// anteposto al messaggio di `format_reject_message`. E' un CONTRATTO MACCHINA
/// stabile (come il marker d'errore `\u{274C}` e `EXIT CODE: N`), NON prosa in
/// linguaggio naturale: questa FONTE lo CODIFICA nel tool_result, il consumatore
/// a valle (`nexus_agent_graph::routing::signals::recent_redaction_rejected`) lo
/// LEGGE come segnale strutturato `redaction_rejected`, MAI facendo
/// `contains("[REDACTED:")` sul testo del placeholder (variabile e umano).
///
/// PUNTO UNICO (regola L): il letterale vive nel crate a VALLE che lo consuma
/// (`nexus_agent_graph::routing::signals::REDACTION_REJECTED_CODE`); qui lo
/// RI-ESPORTIAMO senza duplicarlo, cosi' fonte e consumatore condividono un
/// solo valore versionato insieme.
pub use nexus_agent_graph::routing::signals::REDACTION_REJECTED_CODE;

/// Messaggio di rifiuto per il modello: spiega cos'e' il placeholder e quale
/// meccanismo usare al posto di copiarlo.
///
/// Prefisso a DUE marker macchina (contratto dati, regola M): il marker d'errore
/// `\u{274C}` (letto dal punto unico `tool_runner_server::tool_result_is_error`
/// -> `is_error=true`: il rifiuto e' un ERRORE applicativo, non un finto
/// successo) e il codice strutturato [`REDACTION_REJECTED_CODE`] (letto a valle
/// come segnale `redaction_rejected`). Seguono la prosa esplicativa per il
/// modello (che resta SOLO per display/guida, mai per decidere).
pub fn format_reject_message(tool_name: &str, field: &str, placeholder: &str) -> String {
    format!(
        "\u{274C} {REDACTION_REJECTED_CODE} [BLOCCATO — placeholder di redazione nell'input]\n\
         L'input di `{tool_name}` (campo '{field}') contiene '{placeholder}': e' un SEGNAPOSTO \
         prodotto dalla redazione dei segreti nel contesto, NON un valore reale. Copiarlo in \
         comandi, file o configurazioni produce errori a runtime (es. getaddrinfo ENOTFOUND).\n\
         Come procedere senza il valore in chiaro:\n\
         - DATABASE_URL: NON comporla mai. `run_command` e `run_service` iniettano gia' \
         automaticamente DATABASE_URL e NEXUS_PROJECT_DB_URL del DB applicativo del progetto \
         nell'ambiente del processo: leggila da process.env / os.environ, e rimuovi qualsiasi \
         assegnazione esplicita dal comando o dal file .env.\n\
         - Porte: usa il tool `request_port`.\n\
         - Altri segreti (API key, email, credenziali): non inventare valori e non riusare il \
         placeholder; leggi da variabili d'ambiente o chiedi all'utente di configurarli."
    )
}

/// Enforcement con policy DB-driven (catalogo `nexus_resource_policies`,
/// kind='secret', rule='no_redacted_placeholder', default fail-safe enabled)
/// e audit. Ritorna `Some(messaggio di rifiuto)` se l'input va bloccato.
pub async fn enforce_no_redacted_placeholder(
    ctx: &crate::agent_tools::AgentToolContext,
    tool_name: &str,
    field: &str,
    text: &str,
) -> Option<String> {
    // Funzione pura prima del lookup policy: zero costo DB quando non serve.
    let placeholder = find_redacted_placeholder(text)?;
    let policy =
        super::resource_governance::policy(ctx.db.as_ref(), "secret", "no_redacted_placeholder")
            .await;
    if !policy.enabled {
        return None;
    }
    let mut entry = crate::security::AuditEntry::blocked(
        ctx.project_id,
        "redacted_placeholder_rejected",
        "secret",
    )
    .with_resource(placeholder.clone())
    .with_details(json!({
        "tool": tool_name,
        "field": field,
        "placeholder": placeholder,
    }))
    .with_actor_user(ctx.user_id);
    if let Some(s) = ctx.session_id {
        entry = entry.with_actor_session(s);
    }
    crate::security::record_audit(entry);
    tracing::warn!(
        tool = tool_name,
        field,
        placeholder = %placeholder,
        project_id = %ctx.project_id,
        "redaction_guard: placeholder di redazione copiato come valore — input rifiutato"
    );
    Some(format_reject_message(tool_name, field, &placeholder))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trova_placeholder_secret_scanner_caso_incidente() {
        // Caso reale Beaty-Book: placeholder copiato come valore di DATABASE_URL.
        let cmd = "DATABASE_URL=[REDACTED:db_connection_string] node server.js";
        assert_eq!(
            find_redacted_placeholder(cmd).as_deref(),
            Some("[REDACTED:db_connection_string]")
        );
        let env = "ADMIN_EMAIL=[REDACTED:email_pii]\nPORT=3000";
        assert_eq!(
            find_redacted_placeholder(env).as_deref(),
            Some("[REDACTED:email_pii]")
        );
    }

    #[test]
    fn trova_placeholder_redaction_map() {
        // Placeholder reversibile della RedactionMap: in una tool call NON
        // viene mai reidratato, quindi va bloccato allo stesso modo.
        let content = "const KEY = '__NEXUS_SECRET_VALUE_3__';";
        assert_eq!(
            find_redacted_placeholder(content).as_deref(),
            Some("__NEXUS_SECRET_VALUE_3__")
        );
    }

    #[test]
    fn testo_pulito_nessun_match() {
        assert!(find_redacted_placeholder("node server.js").is_none());
        assert!(find_redacted_placeholder("il valore e' REDACTED").is_none());
        // Marker aperto ma mai chiuso: non e' un placeholder genuino.
        assert!(find_redacted_placeholder("x = '[REDACTED:").is_none());
        // Env var legittima che inizia con NEXUS ma senza forma placeholder.
        assert!(find_redacted_placeholder("__NEXUS_PROJECT_DB_URL").is_none());
        assert!(find_redacted_placeholder("process.env.NEXUS_PROJECT_DB_URL").is_none());
    }

    #[test]
    fn placeholder_anomali_ignorati() {
        // Slug oltre il cap: non genuino.
        let long = format!("[REDACTED:{}]", "a".repeat(100));
        assert!(find_redacted_placeholder(&long).is_none());
        // Chiusura lontana con caratteri estranei in mezzo: non genuino.
        assert!(find_redacted_placeholder("arr[REDACTED: qualcosa] = 1").is_none());
        // Forma __NEXUS_..._ senza contatore numerico finale: non genuino.
        assert!(find_redacted_placeholder("__NEXUS_FOO_BAR__").is_none());
    }

    #[test]
    fn match_anche_in_mezzo_al_testo_multilinea() {
        let file = "PORT=3001\nDATABASE_URL=[REDACTED:db_connection_string]\nNODE_ENV=dev";
        assert_eq!(
            find_redacted_placeholder(file).as_deref(),
            Some("[REDACTED:db_connection_string]")
        );
    }

    #[test]
    fn messaggio_guida_ai_meccanismi_corretti() {
        let msg =
            format_reject_message("run_service", "command", "[REDACTED:db_connection_string]");
        assert!(msg.contains("[REDACTED:db_connection_string]"));
        assert!(msg.contains("run_service"));
        // Il messaggio deve indirizzare ai meccanismi corretti, non solo rifiutare.
        assert!(msg.contains("NEXUS_PROJECT_DB_URL"));
        assert!(msg.contains("request_port"));
    }

    #[test]
    fn messaggio_porta_i_due_marker_macchina() {
        // CONTRATTO DATI (regola M): il rifiuto e' prefissato dal marker d'errore
        // U+274C (-> is_error=true a valle) e dal codice strutturato
        // [REDACTION_REJECTED] (-> segnale redaction_rejected). I consumatori
        // leggono QUESTI codici, non la prosa del placeholder.
        let msg = format_reject_message("run_command", "command", "[REDACTED:email_pii]");
        assert!(
            msg.trim_start().starts_with('\u{274C}'),
            "il rifiuto deve iniziare col marker d'errore U+274C: {msg}"
        );
        assert!(
            msg.contains(REDACTION_REJECTED_CODE),
            "il rifiuto deve contenere il codice strutturato {REDACTION_REJECTED_CODE}: {msg}"
        );
    }
}
