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
/// Punto unico della riga `nexus_agent_meta_steps` che porta il piano di un run
/// (una sola per run, payload fuso fra i produttori).
pub mod meta_piano;
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
/// l'umano nel campo `error` del corpo JSON e l'esito in un CAMPO (regola Q).
///
/// Punto unico (regola L) del "come si dichiara un fallimento" per i tool che
/// rispondono in JSON: il corpo resta l'oggetto che il modello legge, e la
/// natura la DICHIARA chi conosce la causa invece di restare implicita.
///
/// # Cio' che stava qui accanto, e non c'e' piu'
///
/// Accanto a questa viveva `errore_json`, che l'esito lo metteva in un MARKER
/// anteposto al testo perche' la firma `-> String` non aveva un campo dove
/// metterlo. Era il difetto della regola Q visto dal lato del produttore, e il
/// suo doc prometteva che sarebbe sparita col primo giorno in cui nessuno la
/// chiamava piu': quel giorno e' arrivato, e non c'e' piu'.
///
/// I suoi ultimi chiamanti erano TRE, non uno — `archive_tools.rs`,
/// `attachments.rs` e `knowledge.rs` — migrati in parallelo. Il vincolo che ne
/// discende e' di ORDINE, non di codice: la rimozione da qui non puo' arrivare
/// in `main` prima delle tre migrazioni, perche' senza di loro questo crate non
/// compila; e non puo' restare indietro, perche' una funzione `pub(crate)` senza
/// chiamanti e' `dead_code` sotto `-D warnings`. Le quattro modifiche sono un
/// commit solo.
///
/// Il marker resta in `nexus_types::tool_outcome` per i tool degli ALTRI crate
/// che devono ancora migrare; in questo, un fallimento non e' piu' una stringa
/// da rileggere.
pub(crate) fn errore_tool(
    messaggio: impl std::fmt::Display,
    natura: nexus_types::tool_outcome::NaturaFallimento,
) -> nexus_types::tool_outcome::RispostaTool {
    errore_tool_con_dettagli(serde_json::json!({ "error": messaggio.to_string() }), natura)
}

/// Come [`errore_tool`] quando il corpo porta anche campi oltre `error` — il
/// verdetto di un audit, un `hint` con l'azione corretta.
pub(crate) fn errore_tool_con_dettagli(
    dettagli: serde_json::Value,
    natura: nexus_types::tool_outcome::NaturaFallimento,
) -> nexus_types::tool_outcome::RispostaTool {
    nexus_types::tool_outcome::RispostaTool::fallito(dettagli.to_string()).con_natura(natura)
}

