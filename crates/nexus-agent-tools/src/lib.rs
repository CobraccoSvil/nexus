//! nexus-agent-tools — parti del toolkit agente estratte dal monolite
//! mcp-core (split 7.4). mcp-core::agent_tools re-esporta questo crate
//! per mantenere validi i path storici crate::agent_tools::*.
//!
//! Passo agent_tools-1: i moduli senza dipendenza dal contesto tool.
//! Passo agent_tools-2: `ToolContextCore` (i campi di AgentToolContext
//! senza dipendenze da mcp-core) + i tool che usano solo quei campi.
//! Passo agent_tools-3: vision_tools (settings via punto unico nexus-auth)
//! e figma_tools (path-safety via nexus_types::workspace_paths).
//! Passo agent_tools-4: git (esecutore in nexus_types::git_exec; reindex
//! post-commit de-accoppiato via trait `FileReindexer` nel context core).
//! Passo agent_tools-5: `files` e `knowledge`. Il trait del reindex diventa
//! `FileMutationHooks` (l'INTERO ciclo di vita di una mutazione file: gate di
//! governance pre-scrittura, tracking, autocommit, hook post-scrittura), e
//! `TextEmbedder` copre l'unico accoppiamento vettoriale di `knowledge`.
//! Candidato successivo: il pacchetto wiki (richiede de-axumizzazione).

pub mod ambiente;
pub mod archive_tools;
pub mod attachment_inspector;
pub mod attachment_settings;
pub mod attachments;
pub mod audio_tools;
pub mod command_hints;
pub mod context_core;
pub mod input_contract;
pub mod dev_diagnostics;
pub mod dispatcher;
pub mod document_tools;
pub mod figma_tools;
pub mod files;
pub mod gateway_client;
pub mod git;
pub mod image_tools;
pub mod knowledge;
pub mod monitor;
pub mod paths;
pub mod profile_tools;
pub mod quality_tools;
pub mod read_cache;
pub mod safety;
pub mod scaffold_verifier;
pub mod shadcn_setup;
// NB: l'orchestrazione dei sub-agenti vive in `mcp-core::agent_tools::subagent_native`
// (richiede `native_engine`, non accessibile da qui per la gerarchia crate). Il
// vecchio modulo `subagent` che chiamava il brain /agent/subagent-run e' stato
// rimosso nel porting a grafo nativo (zero-Python).
pub mod todos;
pub mod tool_inputs;
pub mod tool_schema;
pub mod ui_patterns;
pub mod ui_reference_search;
pub mod ui_styling;
pub mod url_scanner;
pub mod video_tools;
pub mod vision_tools;

pub use context_core::ToolContextCore;

/// Il risultato di un tool di questo crate che ha FALLITO, col messaggio per
/// l'umano nel campo `error` del corpo JSON.
///
/// Punto unico (regola L) del "come si dichiara un fallimento" per i tool che
/// rispondono in JSON: il corpo resta l'oggetto che il modello legge, e la
/// dichiarazione d'esito viaggia nel marker che
/// `nexus_types::tool_outcome::is_tool_failure` riconosce. Senza il marker la
/// catena a valle (`RispostaTool::da_testo_legacy` -> `is_error` ->
/// `repeated_action_failed`) legge un `{"error": ...}` come tool RIUSCITO: un
/// allegato che rifiuta l'estrazione a ogni tentativo diventa cosi' una
/// ripetizione produttiva, e l'anti-loop la classifica come stallo invece che
/// come causa radice da diagnosticare (regola M).
pub(crate) fn errore_json(messaggio: impl std::fmt::Display) -> String {
    errore_json_con_dettagli(serde_json::json!({ "error": messaggio.to_string() }))
}

/// Come [`errore_json`] quando il corpo porta anche campi oltre `error` (tipico:
/// `hint` con l'azione corretta).
///
/// Conseguenza VOLUTA di entrambe: la stringa di errore non e' piu' un documento
/// JSON integro, perche' il marker sta in testa. Il percorso di SUCCESSO non e'
/// toccato, ed e' l'unico che i consumatori strutturati leggano: il presidio del
/// budget letture allegati
/// (`nexus-agent-graph::decisions::tool_dispatch::extract_returned_bytes`) cerca
/// l'intero `length` di primo livello, che solo una lettura RIUSCITA emette — su
/// un errore contava gia' zero byte, con o senza marker.
pub(crate) fn errore_json_con_dettagli(dettagli: serde_json::Value) -> String {
    nexus_types::tool_outcome::tool_failure(dettagli.to_string())
}
