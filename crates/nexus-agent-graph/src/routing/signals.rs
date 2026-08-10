//! Dipendenze PURE del routing, portate 1:1 dal brain Python.
//!
//! Sono le funzioni deterministiche (nessun IO, nessuna lettura DB) che le
//! `route_after_*` consultano per decidere. La config DB-driven arriva sempre
//! come parametro ([`super::config::RoutingConfig`], regola G). Punto unico
//! (regola L): se un giorno il path Rust sara' imboccato, i nodi delegano qui.
//!
//! Riferimenti Python (`brain/agents/nodes/helpers.py` salvo nota).
//! `_detect_unfulfilled_intent` (blacklist lessicale INTENT_NARRATION) e' stato
//! RIMOSSO (ADR 0018 fase 3); il secondo residuo lessicale,
//! `_PENDING_STEPS_LABELS` (62 etichette in 5 lingue, `detect_pending_steps_report`),
//! e' stato eliminato a sua volta: il `closure_verdict` che ne era il presunto
//! fallback non ha mai avuto un produttore nel motore nativo (ADR 0034), quindi
//! il ramo lessicale era l'UNICO decisore, non una difesa in profondita'. Il
//! segnale strutturale [`crate::decisions::structural_unfulfilled_signal`] +
//! `declared_outcome` (ADR 0034, task_complete) lo sostituiscono — vedi
//! [`unfulfilled_signal_with`].
//!   - `has_productive_action_in_history`  -> [`has_productive_action_in_history`]
//!   - `has_filesystem_mutation_in_history`-> [`has_filesystem_mutation_in_history`]
//!   - `_unfulfilled_signal`               -> [`unfulfilled_signal`] (routing.py)
//!   - `_is_software_task`  (final_gate.py)-> [`is_software_task`]
//!   - `_final_gate_eligible` (routing.py) -> [`final_gate_eligible`]
//!   - `todo_isolation_active` (orchestrator_config.py) -> [`todo_isolation_active`]

use serde_json::Value;

use crate::decisions::loop_signatures::{build_signature, firma_esito_ricerca, serie_in_stallo};
use crate::state::{AgentState, ContentBlock, Message, MessageContent};

use super::config::RoutingConfig;

// ──────────────────────────────────────────────────────────────────────────
//  Estrazione tool_use dalla history (fatto strutturale "questo run ha agito")
// ──────────────────────────────────────────────────────────────────────────

/// Itera i nomi dei tool_use emessi da ogni `Message::Ai` della history.
///
/// In Python `has_productive_action_in_history` / `has_filesystem_mutation_in_history`
/// leggono i tool_use da `additional_kwargs["anthropic_content"]` (blocchi
/// `{"type":"tool_use","name":...}`). Nel modello Rust un `Message::Ai` porta
/// i tool_use in DUE forme equivalenti a seconda di chi ha prodotto il
/// messaggio: il campo `tool_calls` (forma OpenAI-compat, da `lc_serde`) e/o i
/// blocchi `ContentBlock::ToolUse` nel `content` (forma Anthropic, equivalente
/// all'`anthropic_content` Python). Guardiamo ENTRAMBI per restare fedeli alla
/// semantica Python in ogni rappresentazione del messaggio.
fn ai_tool_use_names(messages: &[Message]) -> Vec<&str> {
    let mut names: Vec<&str> = Vec::new();
    for m in messages {
        if let Message::Ai {
            content,
            tool_calls,
            ..
        } = m
        {
            // Forma OpenAI-compat: tool_calls.
            for tc in tool_calls {
                names.push(tc.name.as_str());
            }
            // Forma Anthropic: blocchi tool_use nel content (== anthropic_content).
            if let MessageContent::Blocks(blocks) = content {
                for b in blocks {
                    if let ContentBlock::ToolUse { name, .. } = b {
                        names.push(name.as_str());
                    }
                }
            }
        }
    }
    names
}

/// Nomi dei tool di osservazione runtime che ricorrono in piu' liste e nei
/// test. Stessa convenzione di [`PORT_REQUEST_TOOL`]: un nome citato da piu'
/// punti si scrive una volta sola, cosi' un test non puo' verificare una
/// stringa che nessuna lista contiene piu'.
pub(crate) const READ_SERVICE_OUTPUT_TOOL: &str = "read_service_output";
pub(crate) const TAIL_SERVICE_LOGS_TOOL: &str = "tail_service_logs";
pub(crate) const LIST_ACTIVE_SERVICES_TOOL: &str = "list_active_services";
pub(crate) const NEXUS_LIST_PORTS_TOOL: &str = "nexus_list_ports";

/// Tool di SOLA esplorazione (`_EXPLORATION_ONLY_TOOLS` Python): leggono/ispezionano
/// senza produrre side-effect. Un tool_use con nome NON in questo set conta come
/// azione produttiva. Lista tenuta allineata 1:1 a helpers.py.
///
/// PUNTO UNICO (regola L) della lista: oltre a `has_productive_action_in_history`
/// la usa `decisions::loop_signatures::exploration_counter_update` (passata come
/// parametro per restare pura).
pub const EXPLORATION_ONLY_TOOLS: &[&str] = &[
    "nexus_list_archive_entries",
    "nexus_read_archive_entry",
    "nexus_inspect_attachment",
    "nexus_extract_figma_structure",
    "nexus_list_attachments",
    "nexus_read_attachment",
    "nexus_extract_docx_text",
    "nexus_extract_xlsx_data",
    "nexus_extract_pdf_text",
    "nexus_describe_image_attachment",
    "nexus_transcribe_audio",
    "read_file",
    "list_files",
    "grep",
    "read_file_lines",
    "search_in_files",
    "nexus_mcp_tool_search",
    "nexus_get_worklog",
    // Osservazione RUNTIME di servizi/porte (letture pure, per natura da
    // POLLING: stesso input, output che evolve nel tempo). Senza questa
    // classificazione il signature-loop trattava tre letture identiche di log
    // come stallo anche con edit/build in mezzo (run 2c41b145:
    // gemini-2.5-pro interrotto mentre monitorava il dev server tra una
    // correzione e l'altra). Da read-only ereditano: sconto post-progresso nel
    // signature-loop, soglia repeated_action piu' alta, conteggio nel budget
    // esplorazione (il polling infinito a vuoto resta guidato/interrotto).
    READ_SERVICE_OUTPUT_TOOL,
    TAIL_SERVICE_LOGS_TOOL,
    LIST_ACTIVE_SERVICES_TOOL,
    NEXUS_LIST_PORTS_TOOL,
];

/// Sottoinsieme di [`EXPLORATION_ONLY_TOOLS`] che osserva stato RUNTIME in
/// evoluzione (log/servizi/porte) invece di un contenuto statico (file,
/// grep, allegati): per natura da POLLING, la stessa domanda ripetuta a
/// intervalli e' l'uso previsto, non un sintomo di stallo (vedi il commento
/// sopra sull'incidente run 2c41b145). Punto unico (regola L) del
/// sottoinsieme "verifica post-lavoro" letto da [`recent_ai_turn_counts`] e
/// [`verifying_after_productive_work`] — usato dal gate G1 loop-conclamato
/// (`nodes::executor::ExecutorNode::g1_cap_effettivo`) per NON confondere una
/// fase di monitoraggio legittima dopo lavoro gia' fatto con uno stallo
/// genuino. L'invariante "ogni voce qui compare anche in
/// `EXPLORATION_ONLY_TOOLS`" e' coperta da test
/// (`runtime_observation_tools_e_sottoinsieme_di_exploration_only`), non
/// dalla struttura dati (le due liste rispondono a domande diverse: "non
/// muta nulla" contro "osserva qualcosa che evolve da solo").
pub const RUNTIME_OBSERVATION_TOOLS: &[&str] = &[
    READ_SERVICE_OUTPUT_TOOL,
    TAIL_SERVICE_LOGS_TOOL,
    LIST_ACTIVE_SERVICES_TOOL,
    NEXUS_LIST_PORTS_TOOL,
];

/// True se il run ha gia' eseguito almeno UN'azione PRODUTTIVA (tool_use con
/// nome NON in `EXPLORATION_ONLY_TOOLS`). Vedi `has_productive_action_in_history`.
///
/// Punto unico (regola L) del fatto strutturale "questo run ha gia' agito".
pub fn has_productive_action_in_history(messages: &[Message]) -> bool {
    ai_tool_use_names(messages)
        .into_iter()
        .any(|name| !EXPLORATION_ONLY_TOOLS.contains(&name))
}

/// Conta, negli ultimi `lookback` messaggi, i turni AI (`Message::Ai`) totali,
/// quanti hanno emesso almeno un tool_use PRODUTTIVO RIUSCITO (nome fuori da
/// [`EXPLORATION_ONLY_TOOLS`] e con esito diverso da errore, via
/// [`tool_result_outcome_after`]) e quanti sono turni di sola OSSERVAZIONE
/// RUNTIME riuscita (tutti i tool_use del turno in [`RUNTIME_OBSERVATION_TOOLS`],
/// esito RIUSCITO). Ritorna `(ai_turns, productive_turns, monitoring_turns)`.
///
/// Punto unico (regola L) del fatto strutturale "la history recente mostra
/// azione produttiva / verifica legittima?": [`has_recent_productive_action`]
/// e' un thin wrapper su questo conteggio (`productive_turns > 0`), e il gate
/// G1 loop-conclamato legge tutte e tre le componenti dallo STESSO conteggio
/// invece di ri-scandire la history con implementazioni distinte.
///
/// "Produttivo" richiede successo, non solo il nome del tool: un tool fuori da
/// `EXPLORATION_ONLY_TOOLS` che FALLISCE sempre (`is_error=true` a ogni
/// tentativo) non e' evidenza che il run stia progredendo, e non deve
/// sottrarre un turno al conteggio di stallo. Riusa
/// [`tool_result_outcome_after`] (stesso punto unico di
/// [`modified_files_from_messages`]), un solo calcolo dell'esito per turno.
pub fn recent_ai_turn_counts(messages: &[Message], lookback: usize) -> (usize, usize, usize) {
    let window = tail_messages(messages, lookback);
    /// Stesso raggio di [`modified_files_from_messages`]: il tool_result di un
    /// tool_use segue di norma entro poche posizioni nella history.
    const OUTCOME_LOOKAHEAD: usize = 3;
    let mut ai_turns = 0usize;
    let mut productive_turns = 0usize;
    let mut monitoring_turns = 0usize;
    for (idx, m) in window.iter().enumerate() {
        if !matches!(m, Message::Ai { .. }) {
            continue;
        }
        ai_turns += 1;
        let tool_uses = message_tool_uses(m);
        if tool_uses.is_empty() {
            continue;
        }
        let outcome = tool_result_outcome_after(window, idx, OUTCOME_LOOKAHEAD);
        let has_productive_name = tool_uses
            .iter()
            .any(|(name, _)| !EXPLORATION_ONLY_TOOLS.contains(name));
        if has_productive_name && outcome != Some(true) {
            productive_turns += 1;
        }
        let all_monitoring = tool_uses
            .iter()
            .all(|(name, _)| RUNTIME_OBSERVATION_TOOLS.contains(name));
        if all_monitoring && outcome == Some(false) {
            monitoring_turns += 1;
        }
    }
    (ai_turns, productive_turns, monitoring_turns)
}

/// Come [`has_productive_action_in_history`] ma limitata agli ULTIMI `lookback`
/// messaggi: distingue "il run sta producendo lavoro ADESSO" da "ha agito all'inizio
/// e ora gira a vuoto". Usata dal gate G1 loop-conclamato per NON abortire un run che
/// ha appena eseguito azioni concrete (anti falso-negativo, regola H): un run reale
/// aveva installato i browser Playwright + system-deps e fatto passare il test E2E,
/// ma il vecchio gate lessicale "non compiuto" (blacklist NARRAZIONE, rimossa con
/// ADR 0018 fase 3) lo abortiva ignorando i 16 tool riusciti, sostituendo il
/// successo con un messaggio di resa. Il segnale STRUTTURALE prevale sempre. Thin
/// wrapper su [`recent_ai_turn_counts`] (regola L): stessa finestra, stessa
/// nozione di "produttivo", un solo posto che la definisce.
pub fn has_recent_productive_action(messages: &[Message], lookback: usize) -> bool {
    recent_ai_turn_counts(messages, lookback).1 > 0
}

/// True se il run sta VERIFICANDO dopo aver gia' lavorato: nella finestra
/// recente OGNI turno AI con tool_use e' un turno di sola osservazione
/// RUNTIME riuscita (`monitoring_turns_in_lookback == ai_turns_in_lookback`,
/// entrambi da [`recent_ai_turn_counts`]) e altrove nella history — anche
/// FUORI dalla finestra, dove il gate G1 non guarderebbe piu' — c'e' evidenza
/// di almeno un'azione produttiva ([`has_productive_action_in_history`]).
///
/// Punto unico (regola L) del credito "verifica legittima dopo lavoro", letto
/// da `nodes::executor::ExecutorNode::g1_cap_effettivo` per allargare
/// `g1_recent_productive` oltre la finestra fissa. Senza questo credito, un
/// run che ha gia' scritto/editato e poi passa a 8+ turni di solo
/// `read_service_output`/`tail_service_logs`/`list_active_services`/
/// `nexus_list_ports` (mai un tool statico come `read_file`/`grep`, che
/// resta FUORI da `RUNTIME_OBSERVATION_TOOLS` e quindi non ottiene credito)
/// vede le proprie azioni produttive uscire dalla finestra di lookback e
/// viene trattato come loop conclamato — la stessa classe di falso positivo
/// gia' incontrata dal signature-loop sul run 2c41b145 (vedi
/// `EXPLORATION_ONLY_TOOLS` sopra), qui sul gate G1 invece che sul
/// detector di firme ripetute. Un run che monitora SENZA aver mai lavorato
/// non riceve credito (nessuna azione produttiva da nessuna parte nella
/// history): resta soggetto al gate come prima.
pub fn verifying_after_productive_work(
    messages: &[Message],
    ai_turns_in_lookback: usize,
    monitoring_turns_in_lookback: usize,
) -> bool {
    ai_turns_in_lookback > 0
        && monitoring_turns_in_lookback == ai_turns_in_lookback
        && has_productive_action_in_history(messages)
}

/// Elenca i file modificati con SUCCESSO (edit_file/write_file con tool_result NON
/// errore) negli ultimi `lookback` messaggi, in ordine di prima apparizione, senza
/// duplicati. Usata per un recap ONESTO: un ABORT non deve dichiarare "File toccati:
/// nessuno" quando l'agente ha realmente applicato modifiche (regola H). Pura.
pub fn modified_files_from_messages(messages: &[Message], lookback: usize) -> Vec<String> {
    let recent = tail_messages(messages, lookback);
    let mut out: Vec<String> = Vec::new();
    for (idx, m) in recent.iter().enumerate() {
        for (name, input) in message_tool_uses(m) {
            if !matches!(name, "edit_file" | "write_file") {
                continue;
            }
            let path = input
                .get("path")
                .or_else(|| input.get("file_path"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let Some(path) = path else { continue };
            // Solo edit RIUSCITO (outcome == Some(false) = non errore).
            if tool_result_outcome_after(recent, idx, 3) == Some(false)
                && !out.iter().any(|p| p == path)
            {
                out.push(path.to_string());
            }
        }
    }
    out
}

/// Riepilogo conciso dei tool eseguiti nella history, per nome con conteggio:
/// es. "5 azioni (write_file x3, run_command x2)". `None` se nessun tool_use.
/// Punto unico (regola L) del riepilogo-lavoro: l'executor lo allega al messaggio
/// quando il turno si interrompe (es. provider in cooldown), cosi' l'utente vede
/// COSA e' stato fatto e non solo l'errore. Ordine = prima apparizione.
pub fn summarize_actions_in_history(messages: &[Message]) -> Option<String> {
    let names = ai_tool_use_names(messages);
    if names.is_empty() {
        return None;
    }
    let total = names.len();
    let mut order: Vec<&str> = Vec::new();
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for n in &names {
        if !counts.contains_key(n) {
            order.push(n);
        }
        *counts.entry(n).or_insert(0) += 1;
    }
    let parts: Vec<String> = order
        .iter()
        .map(|n| {
            let c = counts[n];
            if c > 1 {
                format!("{n} x{c}")
            } else {
                (*n).to_string()
            }
        })
        .collect();
    Some(format!("{total} azioni ({})", parts.join(", ")))
}

/// True se il run ha eseguito almeno un tool che MUTA filesystem/progetto.
/// Vedi `has_filesystem_mutation_in_history`. La lista mutators arriva dalla
/// config (setting `agent.tools.result_cache_mutators`, mig 0394).
pub fn has_filesystem_mutation_in_history(messages: &[Message], cfg: &RoutingConfig) -> bool {
    ai_tool_use_names(messages)
        .into_iter()
        .any(|name| cfg.fs_mutator_tools.iter().any(|m| m == name))
}

/// Estrae il set dei FILE modificati dal run: per ogni tool_use in history il cui
/// nome e' un mutator fs (`cfg.fs_mutator_tools`, DB-driven), l'argomento `path`
/// o `file_path`. PUNTO UNICO (regola L) usato dal final_gate per il gate
/// DELTA-aware: un errore di build conta come REGRESSIONE solo se colpisce un
/// file che il task ha toccato; il debito preesistente in file non toccati non
/// blocca la chiusura. Riusa [`message_tool_uses`] (stesso estrattore (name,
/// input) del resto del modulo).
pub fn touched_files_in_history(
    messages: &[Message],
    cfg: &RoutingConfig,
) -> std::collections::BTreeSet<String> {
    let mut files = std::collections::BTreeSet::new();
    for m in messages {
        for (name, input) in message_tool_uses(m) {
            if !cfg.fs_mutator_tools.iter().any(|t| t == name) {
                continue;
            }
            let path = input
                .get("path")
                .and_then(Value::as_str)
                .or_else(|| input.get("file_path").and_then(Value::as_str));
            if let Some(p) = path {
                let p = p.trim();
                if !p.is_empty() {
                    files.insert(p.to_string());
                }
            }
        }
    }
    files
}

/// Tool i cui argomenti possono contenere una chiamata HTTP fatta dal run
/// (`curl`, `Invoke-WebRequest`, `wget`, un client scritto al volo): sono gli
/// stessi tool comando gia' tracciati da [`detect_repeated_failed_command`].
const HTTP_PROBE_TOOLS: &[&str] = &["run_command", "run_service", "run_in_terminal"];

/// Quante chiamate HTTP il run ha esercitato da se': un tool comando il cui
/// `command` cita un URL `http(s)://`. Serve a UNA sola domanda — *il silenzio
/// sul fronte funzionale e' sospetto?* — e non a costruire criteri.
///
/// La distinzione e' load-bearing. Derivare le prove del gate da qui coprirebbe
/// solo cio' che l'agente ha GIA' provato, cioe' tipicamente le sole GET: nel
/// caso reale (gestione-spese, 2026-07-28) avrebbe riprodotto esattamente il
/// falso positivo — `GET /api/expenses` verde mentre la `POST` rispondeva 500 —
/// con in piu' l'aria di una verifica. Come RIVELATORE, invece, e' esatto: un run
/// che ha interrogato un servizio HTTP ne ha uno, e chiudere senza averne provato
/// nessun endpoint non e' una chiusura verificata (vedi
/// `FinalGateNode::run`, che in quel caso lo DICHIARA).
///
/// Il fatto si legge dall'INPUT del tool (argomento strutturato che il run ha
/// scritto), non dal testo di una risposta: nessuna deduzione di stato tecnico
/// dalla prosa (regola M).
pub fn http_probes_in_history(messages: &[Message]) -> usize {
    let mut n = 0usize;
    for m in messages {
        for (name, input) in message_tool_uses(m) {
            if !HTTP_PROBE_TOOLS.contains(&name) {
                continue;
            }
            let cmd = input.get("command").and_then(Value::as_str).unwrap_or("");
            if cmd.contains("http://") || cmd.contains("https://") {
                n += 1;
            }
        }
    }
    n
}

// ──────────────────────────────────────────────────────────────────────────
//  Detector strutturali su lista messaggi (anti-loop dell'executor)
//
//  Tutti PURI su `&[Message]`: scansionano i blocchi tool_use/tool_result.
//  Leggono SOLO segnali strutturati (exit_code/is_error) e il contratto macchina
//  che i tool usano per dichiarare il proprio fallimento
//  ([`nexus_types::tool_outcome`]). Punto unico (regola L) della domanda
//  "questo tool_use e' riuscito?" -> [`tool_result_outcome_after`].
//
//  Qui viveva `TOOL_ERROR_HINTS`: 28 frasi ("timeout", "not found", "error:",
//  perfino "is_error") cercate nel PAYLOAD del risultato. Il ramo si prendeva
//  ogni volta che il tool non era un comando, e l'`is_error` letto due righe
//  sopra veniva ignorato sul ramo negativo — mancava il `return Some(false)`.
//  Un `read_file` RIUSCITO su un sorgente che contiene la parola "timeout"
//  contava come tool fallito: falso loop, run abortito, scale-down bloccato.
//  Il vocabolario e' stato TOLTO, non spostato nel DB: parametrizzarlo avrebbe
//  dato dignita' di configurazione a una premessa sbagliata, cioe' che l'esito
//  di una chiamata si legga nella prosa del suo risultato (regola M).
// ──────────────────────────────────────────────────────────────────────────

/// Nome del tool di allocazione porte (`_PORT_REQUEST_TOOL` Python: "request_port").
const PORT_REQUEST_TOOL: &str = "request_port";

/// Tool che, se presenti nella history recente, indicano risorse gia' attive
/// note al run (`_resource_tools` Python).
const RESOURCE_TOOLS: &[&str] = &[PORT_REQUEST_TOOL, LIST_ACTIVE_SERVICES_TOOL, "service_restart"];

/// Estrae i tool_use `(name, input)` di un singolo [`Message::Ai`], guardando
/// ENTRAMBE le forme: `tool_calls` (OpenAI-compat) e `ContentBlock::ToolUse`
/// (Anthropic, == anthropic_content Python). Ritorna vuoto per gli altri ruoli.
/// Punto unico (regola L): l'executor lo usa per individuare l'ULTIMO tool_use
/// nella coda (esito strutturato dello StallContext).
pub fn message_tool_uses(m: &Message) -> Vec<(&str, &Value)> {
    let mut out: Vec<(&str, &Value)> = Vec::new();
    if let Message::Ai {
        content,
        tool_calls,
        ..
    } = m
    {
        for tc in tool_calls {
            out.push((tc.name.as_str(), &tc.input));
        }
        if let MessageContent::Blocks(blocks) = content {
            for b in blocks {
                if let ContentBlock::ToolUse { name, input, .. } = b {
                    out.push((name.as_str(), input));
                }
            }
        }
    }
    out
}

/// Valuta l'esito di UN messaggio se e' un tool_result: `Some(true)`=errore,
/// `Some(false)`=successo, `None`=non e' un tool_result valutabile.
///
/// Gestisce ENTRAMBE le forme: [`Message::Tool`] (== `ToolMessage` langchain) e
/// i blocchi [`ContentBlock::ToolResult`] in un qualsiasi messaggio (==
/// `HumanMessage`+anthropic_content Python). Gerarchia dei segnali:
///   1. `exit_code` STRUTTURATO (tool-comando): 0=successo, !=0=errore;
///   2. `is_error` STRUTTURATO del blocco/messaggio tool.
///
/// Entrambi arrivano dal confine del dispatch, dove l'esito e' gia' strutturato.
/// Se il blocco c'e' e nessuno dei due dice "errore", il tool e' RIUSCITO: prima
/// mancava questo `Some(false)` e si scendeva a cercare 28 frasi nel testo del
/// risultato, cosicche' un file letto correttamente poteva dichiarare fallita la
/// lettura per una parola contenuta nel proprio codice.
///
/// Sul solo ramo senza blocco strutturato (un `ToolMessage` di testo piatto) si
/// legge il CONTRATTO con cui i tool dichiarano il fallimento
/// ([`nexus_types::tool_outcome::is_tool_failure`]): e' un marker macchina che
/// il produttore scrive apposta, non una parola pescata nella prosa.
///
/// PUNTO UNICO (regola L) della domanda "questo messaggio porta un tool_result
/// fallito?". Pubblica perche' la pone anche il rilevatore di anomalie del
/// supervisore, che prima la risolveva per conto proprio e sul canale sbagliato.
pub fn message_tool_result_outcome(m: &Message) -> Option<bool> {
    /// Esito di un blocco `ToolResult` dai suoi soli campi strutturati.
    fn esito_blocco(is_error: bool, exit_code: Option<i64>) -> bool {
        match exit_code {
            // Il tool-comando ha dichiarato il proprio exit code: e' il segnale
            // primario, e un exit 0 vale successo anche se `is_error` non e'
            // stato popolato dal costruttore del blocco.
            Some(ec) => ec != 0,
            None => is_error,
        }
    }
    /// Esito dei blocchi `ToolResult` presenti: `Some(true)` se ALMENO UNO
    /// dichiara un fallimento, `Some(false)` se ce n'e' almeno uno e nessuno lo
    /// dichiara, `None` se non ce ne sono. Un messaggio puo' portare i risultati
    /// di piu' tool eseguiti in parallelo, e la domanda a cui questo modulo
    /// risponde ("il tool_use qui sopra e' riuscito?") non discrimina per id:
    /// l'aggregazione prudente e' quella storica.
    fn esito_dei_blocchi(blocks: &[ContentBlock]) -> Option<bool> {
        let mut trovato = false;
        let mut fallito = false;
        for b in blocks {
            if let ContentBlock::ToolResult {
                is_error,
                exit_code,
                ..
            } = b
            {
                trovato = true;
                fallito |= esito_blocco(*is_error, *exit_code);
            }
        }
        trovato.then_some(fallito)
    }
    match m {
        // ToolMessage langchain: il content puo' essere testo o blocchi.
        Message::Tool { content, .. } => match content {
            MessageContent::Text(s) => Some(nexus_types::tool_outcome::is_tool_failure(s)),
            MessageContent::Blocks(blocks) => esito_dei_blocchi(blocks).or_else(|| {
                // Nessun blocco tool_result: resta il testo, e su quello vale il
                // contratto con cui i tool dichiarano il fallimento.
                Some(blocks.iter().any(|b| {
                    matches!(b, ContentBlock::Text { text }
                        if nexus_types::tool_outcome::is_tool_failure(text))
                }))
            }),
        },
        // anthropic_content tool_result in un HumanMessage (tool_dispatch_node
        // emette il tool_result come HumanMessage; gli AIMessage portano i
        // tool_use, mai i tool_result -> non valutati, come in Python).
        Message::Human { content } => match content {
            MessageContent::Blocks(blocks) => esito_dei_blocchi(blocks),
            // Testo piatto in un HumanMessage: e' il canale su cui viaggiano i
            // nudge dell'executor e i promemoria di sistema, non un esito.
            MessageContent::Text(_) => None,
        },
        // AIMessage: porta tool_use, mai tool_result -> non e' un risultato.
        Message::Ai { .. } => None,
    }
}

/// Coda degli ultimi `lookback` messaggi (come `messages[-lookback:]` Python).
fn tail_messages(messages: &[Message], lookback: usize) -> &[Message] {
    let start = messages.len().saturating_sub(lookback);
    &messages[start..]
}

/// True se nella history ci sono gia' stati tool call effettivi (un `Message::Ai`
/// con almeno un tool_use). Vedi `_has_tool_calls_in_history`.
pub fn has_tool_calls_in_history(messages: &[Message]) -> bool {
    messages.iter().any(|m| !message_tool_uses(m).is_empty())
}

/// Esito del primo tool_result nei `max_ahead` messaggi dopo `recent[idx]`.
/// `Some(true)`=errore, `Some(false)`=successo, `None`=nessun risultato trovato.
/// Vedi `_tool_result_outcome_after` (max_ahead=3 default). Punto unico (regola L)
/// della domanda "il tool_use a recent[idx] e' riuscito?".
pub fn tool_result_outcome_after(recent: &[Message], idx: usize, max_ahead: usize) -> Option<bool> {
    let end = (idx + 1 + max_ahead).min(recent.len());
    for nm in recent.iter().take(end).skip(idx + 1) {
        if let Some(outcome) = message_tool_result_outcome(nm) {
            return Some(outcome);
        }
    }
    None
}

/// Chiave CONTRATTO del tool_result emesso dal ponte figlio->padre di
/// `mcp-core::agent_tools::subagent_native` (`K_SUB_RUN_ID`): discrimina un
/// tool_result di sub-run da qualunque altro. Duplicata come letterale (mcp-core
/// dipende da questo crate, non viceversa: non possiamo importare la sua const),
/// stabile come gli altri contratti macchina (`EXIT CODE: N`, `\u{274C}`).
const SUBAGENT_RUN_ID_KEY: &str = "subagent_run_id";

/// `true` se un payload di tool_result e' la chiusura RIUSCITA di un sub-run:
/// porta la chiave contratto [`SUBAGENT_RUN_ID_KEY`] E `status == "completed"`
/// (segnale MACCHINA, regola M — non prosa). `paused`/`timeout`/`failed` NON
/// contano. Gestisce sia il payload gia' strutturato (`Value::Object`) sia la
/// forma tipica in cui il tool ritorna una STRINGA JSON (`Value::String`).
fn is_completed_subagent_payload(v: &Value) -> bool {
    fn obj_matches(obj: &serde_json::Map<String, Value>) -> bool {
        obj.contains_key(SUBAGENT_RUN_ID_KEY)
            && obj.get("status").and_then(Value::as_str) == Some("completed")
    }
    match v {
        Value::Object(obj) => obj_matches(obj),
        Value::String(s) => serde_json::from_str::<Value>(s)
            .ok()
            .as_ref()
            .and_then(Value::as_object)
            .map(obj_matches)
            .unwrap_or(false),
        _ => false,
    }
}

/// `true` se nella history c'e' il tool_result di un `dispatch_subagent(s)`
/// COMPLETATO con successo (un sub-run arrivato a fine turno, che ha percio'
/// dichiarato il proprio esito via `task_complete`).
///
/// Serve al final_gate (`completion_confirmed`, ADR 0034): quando il PADRE
/// coordinatore delega l'intero lavoro a un sub-agente — che HA dichiarato in
/// modo strutturato — e chiude il turno senza ri-dichiarare a sua volta, la
/// CHIUSURA onesta del run ESISTE gia' (quella del figlio). Il criterio non deve
/// bocciare per "nessuna dichiarazione": ne cerca UNA, non che sia del padre. La
/// verifica tecnica (build/typecheck) resta a guardia della correttezza — un
/// figlio che ha lasciato il lavoro incompleto fa fallire gli altri criteri.
///
/// Punto unico (regola L) del fatto strutturale "un sub-run e' stato delegato e
/// chiuso con successo in questo run". Legge il segnale MACCHINA (`status`), mai
/// la prosa del summary.
pub fn has_completed_subagent_dispatch(messages: &[Message]) -> bool {
    messages.iter().any(message_has_completed_subagent_result)
}

/// Vero se il messaggio porta un tool_result di sub-run completato, in una
/// qualsiasi delle forme (ToolMessage testo/blocchi, HumanMessage con blocchi
/// tool_result), come [`message_tool_result_outcome`].
fn message_has_completed_subagent_result(m: &Message) -> bool {
    let block_matches = |b: &ContentBlock| match b {
        ContentBlock::ToolResult { content, .. } => is_completed_subagent_payload(content),
        ContentBlock::Text { text } => is_completed_subagent_payload(&Value::String(text.clone())),
        ContentBlock::ToolUse { .. } => false,
    };
    match m {
        Message::Tool { content, .. } => match content {
            MessageContent::Text(s) => is_completed_subagent_payload(&Value::String(s.clone())),
            MessageContent::Blocks(blocks) => blocks.iter().any(block_matches),
        },
        Message::Human { content } => match content {
            MessageContent::Blocks(blocks) => blocks.iter().any(block_matches),
            MessageContent::Text(_) => false,
        },
        Message::Ai { .. } => false,
    }
}

/// CODICE STRUTTURATO (regola M) che il guard anti-persistenza-redazione della
/// fonte (`mcp-core::security::redaction_guard`) antepone al tool_result quando
/// RIFIUTA un input contenente un placeholder di redazione (audit
/// `redacted_placeholder_rejected`). E' un CONTRATTO MACCHINA stabile — come il
/// marker d'errore `\u{274C}` e `EXIT CODE: N` — non prosa: la fonte lo CODIFICA,
/// [`recent_redaction_rejected`] lo LEGGE. mcp-core importa QUESTA costante
/// (punto unico, regola L): un solo letterale, definito nel crate a valle che i
/// consumatori leggono, referenziato dalla fonte a monte.
pub const REDACTION_REJECTED_CODE: &str = "[REDACTION_REJECTED]";

/// Rende il testo di UN content di tool_result (stringa o blocchi) per la sola
/// ricerca del codice sentinella [`REDACTION_REJECTED_CODE`]. Gemello di
/// [`content_value_to_text`] applicato ai `ContentBlock::ToolResult`; NON
/// classifica il significato del testo (regola M: cerca un codice macchina,
/// non pattern di prosa).
fn tool_result_text_of(m: &Message) -> Option<String> {
    match m {
        Message::Tool { content, .. } => Some(match content {
            MessageContent::Text(s) => s.clone(),
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolResult { content, .. } => {
                        Some(content_value_to_text(content))
                    }
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }),
        Message::Human { content } => {
            let MessageContent::Blocks(blocks) = content else {
                return None;
            };
            let parts: Vec<String> = blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolResult { content, .. } => {
                        Some(content_value_to_text(content))
                    }
                    _ => None,
                })
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        Message::Ai { .. } => None,
    }
}

/// `true` se negli ultimi `lookback` messaggi c'e' un tool_result che porta il
/// CODICE STRUTTURATO [`REDACTION_REJECTED_CODE`] (la fonte ha rifiutato un input
/// per placeholder di redazione). E' il SEGNALE STRUTTURATO (regola M) che
/// alimenta `StallContext.redaction_rejected`: riconosce il blocco ambientale
/// (l'email/segreto ri-oscurato che il modello continua a copiare) SENZA
/// pattern-matching sulla prosa del messaggio ne' `contains("[REDACTED:")` sul
/// placeholder umano. Punto unico (regola L): l'executor delega qui, non
/// re-implementa la scansione.
pub fn recent_redaction_rejected(messages: &[Message], lookback: usize) -> bool {
    tail_messages(messages, lookback)
        .iter()
        .filter_map(tool_result_text_of)
        .any(|t| t.contains(REDACTION_REJECTED_CODE))
}

/// Cap del testo estratto per il confronto output-progresso (evita di
/// confrontare blob enormi: la testa e' sufficiente a distinguere due esiti).
const OUTPUT_COMPARE_CAP: usize = 4000;

/// Soglia di similarita' (Jaccard su righe) oltre cui due output della STESSA
/// azione sono considerati "lo stesso esito". Sotto soglia = l'esito e'
/// CAMBIATO -> la ripetizione mostra PROGRESSO (es. build che fallisce con
/// errori diversi dopo ogni correzione), non uno stallo.
///
/// Taratura CONSERVATIVA (0.75): un esito identico con 1-2 righe volatili su
/// 10 (timestamp/durate, "Done in 741ms") resta ~0.82 -> SIMILE (stallo, come
/// storico); due errori davvero diversi condividono solo il boilerplate ->
/// tipicamente < 0.75 -> DIVERSO (progresso). Nel dubbio si classifica SIMILE:
/// la feature puo' solo salvare run che progrediscono, mai nascondere uno
/// stallo piu' di quanto facesse il comportamento storico.
pub const OUTPUT_SIMILARITY_THRESHOLD: f64 = 0.75;

/// TESTO del primo tool_result dopo `idx` (gemello di
/// [`tool_result_outcome_after`], stessa finestra e STESSE FORME accettate:
/// `Message::Tool` langchain E `Message::Human` con blocchi `ToolResult`):
/// usato per il confronto output-progresso.
/// `None` se nessun tool_result nella finestra.
///
/// Il TAGLIO lo dichiara il chiamante, perche' le due domande che passano di qui
/// lo vogliono opposto: il confronto output-progresso guarda la TESTA del testo
/// e [`OUTPUT_COMPARE_CAP`] gli basta (evita di confrontare blob enormi), mentre
/// chi deve DESERIALIZZARE il payload non puo' accettare alcun taglio — un JSON
/// troncato non si parsa, e il criterio tacerebbe proprio sui risultati piu'
/// ricchi. Un cap fisso qui dentro rendeva il secondo uso impossibile senza che
/// nulla lo dichiarasse.
fn tool_result_text_after(
    recent: &[Message],
    idx: usize,
    max_ahead: usize,
    cap: Option<usize>,
) -> Option<String> {
    let end = (idx + 1 + max_ahead).min(recent.len());
    for nm in recent.iter().take(end).skip(idx + 1) {
        let content = match nm {
            Message::Tool { content, .. } => content,
            Message::Human { content } if matches!(content, MessageContent::Blocks(_)) => content,
            _ => continue,
        };
        let text: String = match content {
            MessageContent::Text(s) => s.clone(),
            MessageContent::Blocks(blocks) => {
                let parts: Vec<String> = blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolResult { content, .. } => {
                            Some(content_value_to_text(content))
                        }
                        _ => None,
                    })
                    .collect();
                // Human senza alcun blocco ToolResult: non e' un tool_result
                // (es. un nudge testuale a blocchi) -> continua a cercare.
                if parts.is_empty() {
                    continue;
                }
                parts.join("\n")
            }
        };
        return Some(match cap {
            Some(n) => text.chars().take(n).collect(),
            None => text,
        });
    }
    None
}

/// Forma testuale di un content di tool_result (stringa diretta o JSON
/// serializzato): solo per il CONFRONTO strutturale, mai per decidere sul
/// significato del testo (regola M).
fn content_value_to_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// CONFRONTO STRUTTURALE di due output della stessa azione: Jaccard
/// sull'insieme delle righe trimmate non vuote, soglia
/// [`OUTPUT_SIMILARITY_THRESHOLD`]. E' una misura di uguaglianza fuzzy del
/// dato grezzo (le righe volatili tipo "Done in 741ms" pesano 1/N), NON una
/// classificazione semantica del contenuto (regola M rispettata: nessun
/// pattern-matching sul significato). Due output entrambi vuoti sono simili.
pub fn outputs_similar(a: &str, b: &str) -> bool {
    let lines = |s: &str| -> std::collections::HashSet<String> {
        s.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    };
    let la = lines(a);
    let lb = lines(b);
    if la.is_empty() && lb.is_empty() {
        return true;
    }
    let inter = la.intersection(&lb).count() as f64;
    let union = la.union(&lb).count() as f64;
    union > 0.0 && (inter / union) >= OUTPUT_SIMILARITY_THRESHOLD
}

/// `true` se le ultime DUE occorrenze della signature `target_sig` nella
/// finestra recente hanno prodotto OUTPUT DIVERSI (sotto soglia di
/// similarita'): la ripetizione sta facendo PROGRESSO — es. `npm run build`
/// rilanciata dopo ogni correzione, che fallisce con errori via via diversi —
/// e NON va chiusa come loop (incidente "run_command: npm run build si
/// ripeteva senza ulteriore progresso" su un modello che stava convergendo).
/// `false` se gli output sono uguali (stallo vero) o se non ci sono almeno
/// due occorrenze con output confrontabile.
pub fn repeated_signature_output_progress(
    messages: &[Message],
    target_sig: &str,
    lookback: usize,
) -> bool {
    let recent = tail_messages(messages, lookback);
    let mut outputs: Vec<String> = Vec::new();
    for (idx, m) in recent.iter().enumerate() {
        for (name, input) in message_tool_uses(m) {
            if build_signature(name, input) == target_sig {
                if let Some(text) = tool_result_text_after(recent, idx, 3, Some(OUTPUT_COMPARE_CAP))
                {
                    outputs.push(text);
                }
            }
        }
    }
    if outputs.len() < 2 {
        return false;
    }
    let last = &outputs[outputs.len() - 1];
    let prev = &outputs[outputs.len() - 2];
    !outputs_similar(prev, last)
}

/// La chiave che un payload di ricerca porta letteralmente quando ha risultati.
/// Serve SOLO a evitare di deserializzare ogni tool_result della history (log,
/// file letti, output di comandi): non e' un criterio — la decisione resta sui
/// CAMPI dopo il parse (regola M) — ed e' esatta, non euristica, perche' un JSON
/// con un campo `hits` contiene per forza questa sottostringa.
const CHIAVE_HITS: &str = "\"hits\"";

/// La firma d'ESITO del tool_use a `recent[idx]`, se quel tool ha risposto con
/// un insieme di risultati. `None` per tutto il resto.
///
/// Il payload viaggia come TESTO (il tool serializza il proprio JSON e il wire
/// porta stringhe), quindi qui si DESERIALIZZA prima di guardare i campi: non e'
/// leggere lo stato tecnico dal testo (regola M), e' rimettere in forma un
/// oggetto che il produttore aveva composto dai propri campi.
fn firma_esito_del_tool_use(recent: &[Message], idx: usize, name: &str) -> Option<String> {
    let testo = tool_result_text_after(recent, idx, 3, None)?;
    if !testo.contains(CHIAVE_HITS) {
        return None;
    }
    let payload: Value = serde_json::from_str(&testo).ok()?;
    firma_esito_ricerca(name, &payload)
}

/// `Some(firma_esito)` se il tool che il turno sta chiamando ORA ha gia'
/// risposto `soglia` volte di fila con lo STESSO identico insieme di risultati.
///
/// IL DIFETTO CHE COPRE, misurato il 10/08/2026 su `prova-fix-10-08`: 17
/// chiamate a `nexus_search_semantic` in un run, 16 riuscite, tutte con gli
/// stessi quattro hit da `index.html`, per una domanda che si risolveva leggendo
/// un file di 183 righe — 852K token dopo che la PRIMA chiamata aveva gia' la
/// risposta. Nessun presidio poteva vederlo: [`repeated_signature_output_progress`]
/// e il signature-loop firmano l'INPUT, e query variate di una parola danno 17
/// firme diverse; `repeated_action_failed` guarda le ripetizioni che FALLISCONO;
/// `correction_progress` misura le scritture, e qui non se ne fa nessuna. Il
/// vocabolario del repo lo diceva gia' — «solo una ripetizione che RIESCE senza
/// progresso e' uno stallo vero» — ma quel caso non aveva un rilevatore.
///
/// GEMELLA E INVERSA di [`repeated_signature_output_progress`]: quella dice
/// «stessa domanda, risposte diverse -> sta progredendo, non chiuderlo»; questa
/// dice «domande diverse, stessa risposta -> sta girando a vuoto». Stesso
/// attraversamento, stesso principio: il progresso si misura sull'ESITO.
///
/// TRE CAUTELE, tutte verso il NON rilevare:
/// - il tool dev'essere fra quelli che il turno chiede ADESSO
///   (`tool_richiesti_ora`): se il modello ha smesso di cercare e sta scrivendo,
///   la ripetizione passata non e' un motivo per fermarlo — stessa condizione
///   con cui il signature-loop pretende la firma fra le `new_signatures`;
/// - un'AZIONE PRODUTTIVA azzera ogni serie: una ricerca ripetuta dopo una
///   scrittura e' una verifica legittima, e puo' dare lo stesso esito;
/// - un turno che emette PIU' tool_use non produce firme: il risultato non e'
///   attribuibile con certezza al singolo tool_use (la history non porta qui il
///   `tool_use_id`), e su un'attribuzione incerta non si decide.
pub fn ricerca_senza_nuovi_risultati(
    messages: &[Message],
    tool_richiesti_ora: &[String],
    lookback: usize,
    soglia: usize,
) -> Option<String> {
    let recent = tail_messages(messages, lookback);
    let mut serie: Vec<(String, Vec<String>)> = Vec::new();
    for (idx, m) in recent.iter().enumerate() {
        let uses = message_tool_uses(m);
        if uses.is_empty() {
            continue;
        }
        if let [(name, _)] = uses.as_slice() {
            if let Some(firma) = firma_esito_del_tool_use(recent, idx, name) {
                match serie.iter_mut().find(|(n, _)| n == name) {
                    Some((_, s)) => s.push(firma),
                    None => serie.push(((*name).to_string(), vec![firma])),
                }
                continue;
            }
        }
        // Nessun insieme di risultati da questo turno: se ha fatto qualcosa che
        // non e' sola lettura, e' lavoro, e ogni serie riparte da li'.
        if uses
            .iter()
            .any(|(n, _)| !EXPLORATION_ONLY_TOOLS.contains(n))
        {
            serie.clear();
        }
    }
    tool_richiesti_ora.iter().find_map(|name| {
        serie
            .iter()
            .find(|(n, _)| n == name)
            .and_then(|(_, s)| serie_in_stallo(s, soglia))
    })
}

/// Conta le chiamate `request_port` negli ultimi `lookback` messaggi (default 16).
/// Segnale STRUTTURALE del loop di riallocazione. NESSUN filtro su input/label.
/// Vedi `_count_recent_request_port`.
pub fn count_recent_request_port(messages: &[Message], lookback: usize) -> i64 {
    let recent = tail_messages(messages, lookback);
    let mut count = 0i64;
    for m in recent {
        for (name, _) in message_tool_uses(m) {
            if name == PORT_REQUEST_TOOL {
                count += 1;
            }
        }
    }
    count
}

/// True se nella history recente (default lookback 24) risulta gia' una risorsa
/// attiva nota al run (un tool_use request_port / list_active_services /
/// service_restart). Vedi `_has_active_resources_in_history`.
pub fn has_active_resources_in_history(messages: &[Message], lookback: usize) -> bool {
    let recent = tail_messages(messages, lookback);
    recent
        .iter()
        .flat_map(message_tool_uses)
        .any(|(name, _)| RESOURCE_TOOLS.contains(&name))
}

/// True se uno degli ultimi `lookback` tool message (default 4) indica errore.
/// Vedi `_detect_recent_tool_error`: scansiona in ordine INVERSO i soli
/// [`Message::Tool`] (== `ToolMessage`), si ferma dopo `lookback` di essi, e
/// segnala errore sul segnale strutturato del blocco, o sul contratto di
/// dichiarazione ([`nexus_types::tool_outcome`]) quando il content e' testo piatto.
pub fn detect_recent_tool_error(messages: &[Message], lookback: usize) -> bool {
    let mut checked = 0usize;
    for m in messages.iter().rev() {
        if checked >= lookback {
            break;
        }
        let Message::Tool { .. } = m else {
            continue;
        };
        checked += 1;
        if message_tool_result_outcome(m) == Some(true) {
            return true;
        }
    }
    false
}

/// Statistiche errore tool STRUTTURATE per lo SCALE-CONTROLLER (regola M: dal
/// segnale `exit_code`/`is_error` del tool_result, MAI dal parsing della prosa —
/// stesso punto unico di [`detect_recent_tool_error`] via
/// `message_tool_result_outcome`). Scansiona gli ultimi `lookback` `Message::Tool`
/// e ritorna `(error_count, error_free_streak)`:
///   - `error_count`: quanti tool_result nella finestra sono errori (esito `true`);
///   - `error_free_streak`: quanti tool_result CONSECUTIVI in coda (dall'ultimo
///     all'indietro) sono SENZA errore, fermandosi al primo errore.
/// Un tool_result con esito ignoto (`None`) NON conta come errore ma INTERROMPE la
/// streak pulita (conservativo: non affermiamo "pulito" su un esito ambiguo). Su
/// history vuota o senza tool_result ritorna `(0, 0)`.
pub fn tool_error_stats(messages: &[Message], lookback: usize) -> (i64, i64) {
    let mut error_count = 0i64;
    let mut streak = 0i64;
    let mut streak_open = true;
    let mut checked = 0usize;
    for m in messages.iter().rev() {
        if checked >= lookback {
            break;
        }
        let Message::Tool { .. } = m else {
            continue;
        };
        checked += 1;
        match message_tool_result_outcome(m) {
            Some(true) => {
                error_count += 1;
                streak_open = false;
            }
            Some(false) => {
                if streak_open {
                    streak += 1;
                }
            }
            None => {
                // Esito ambiguo: non e' un errore, ma chiude la streak pulita.
                streak_open = false;
            }
        }
    }
    (error_count, streak)
}

/// Comandi shell tracciati da `_detect_repeated_failed_command` (1:1).
const FAILED_COMMAND_TOOLS: &[&str] = &["run_command", "run_service", "run_in_terminal"];

/// Rileva la ripetizione dello STESSO comando shell con ERRORE. Ritorna
/// `(Some(command), count)` della signature `command|working_dir` piu' frequente
/// che ha prodotto errore, `(None, 0)` se nessuna. Vedi
/// `_detect_repeated_failed_command` (lookback=12). Solo i comandi il cui
/// tool_result successivo (entro 3 step) e' errore vengono contati.
pub fn detect_repeated_failed_command(
    messages: &[Message],
    lookback: usize,
) -> (Option<String>, i64) {
    if messages.is_empty() {
        return (None, 0);
    }
    let recent = tail_messages(messages, lookback);
    // signature `command|working_dir` -> count; preferisce l'ultima in parita'.
    let mut failed: Vec<(String, i64)> = Vec::new();
    let mut last_signature: Option<String> = None;
    for (idx, m) in recent.iter().enumerate() {
        for (name, input) in message_tool_uses(m) {
            if !FAILED_COMMAND_TOOLS.contains(&name) {
                continue;
            }
            let cmd = input
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if cmd.is_empty() {
                continue;
            }
            let wd = input
                .get("working_dir")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            let signature = format!("{cmd}|{wd}");
            // _detect_repeated_failed_command guarda i 3 messaggi successivi e
            // valuta il PRIMO ToolMessage trovato (max_ahead=3, ma si ferma al
            // primo result a prescindere dall'esito: break).
            let next_is_error = first_tool_result_is_error(recent, idx, 3);
            if next_is_error == Some(true) {
                bump(&mut failed, &signature);
                last_signature = Some(signature);
            }
        }
    }
    pick_top(&failed, last_signature.as_deref()).map_or((None, 0), |(sig, count)| {
        let cmd = sig
            .split_once('|')
            .map(|(c, _)| c)
            .unwrap_or(&sig)
            .to_string();
        (Some(cmd), count)
    })
}

/// Tool tracciati da `detect_repeated_action` -> chiavi argomento che ne
/// definiscono il BERSAGLIO (path/comando/pattern).
///
/// Il bersaglio NON e' l'identita' dell'azione (quella e' l'INPUT COMPLETO, via
/// [`build_signature`]): qui si estrae solo il bersaglio per la label leggibile e
/// per le esclusioni basate sul file (rilettura-dopo-edit, falso-doppione).
/// PUNTO UNICO (regola L): l'estrazione del bersaglio passa tutta da qui.
///
/// Oltre ai tool PRODUTTIVI (scrittura/comando) sono inclusi i tool di SOLA
/// LETTURA con bersaglio (read_file/list_files/grep & co.): la ripetizione
/// IDENTICA di una lettura (stesso path/pattern) e' un loop di esplorazione che
/// non converge (NON-convergenza, regola H) e va fermato dal progress_controller
/// ben prima del cap esplorazione 2x. Per questi tool la ripetizione conta a
/// prescindere dall'esito (vedi [`is_read_only_repeatable_tool`]): rileggere con
/// SUCCESSO lo stesso file e' proprio lo stallo da interrompere.
fn repeated_action_keys(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "write_file" | "edit_file" => Some(&["path", "file_path"]),
        "run_command" | "run_service" | "run_in_terminal" => Some(&["command"]),
        // Tool di sola lettura con bersaglio: bersaglio = path o pattern.
        "read_file" | "read_file_lines" | "list_files" => Some(&["path", "file_path", "dir"]),
        "grep" | "search_in_files" => Some(&["pattern", "query", "path"]),
        _ => None,
    }
}

/// True se `name` e' un tool di SOLA LETTURA per cui la ripetizione identica conta
/// come stallo a PRESCINDERE dall'esito (a differenza dei tool produttivi, dove la
/// PRIMA occorrenza riuscita esclude la signature come "ridondanza innocua"). Per i
/// read-only la rilettura riuscita ripetuta E' lo stallo (l'agente non avanza):
/// quindi NON va esclusa dal conteggio. Punto unico (regola L) della distinzione.
fn is_read_only_repeatable_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file" | "read_file_lines" | "list_files" | "grep" | "search_in_files"
    )
}

/// Esito ricco di [`detect_repeated_action_detailed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepeatedActionHit {
    /// Label leggibile per nudge/recap: `name: bersaglio` (bersaglio troncato a
    /// 120 char). NON contiene il discriminante di contenuto (resta umano).
    pub label: String,
    /// Conteggio della signature vincente nella finestra recente.
    pub count: i64,
    /// Nome del tool ripetuto (`edit_file`, `write_file`, `run_command`, ...).
    pub tool_name: String,
    /// `true` se l'ULTIMA occorrenza della signature vincente e' FALLITA
    /// (tool_result con errore). Discrimina "edit_file fallito da correggere"
    /// dalle altre ripetizioni: alimenta il nudge specifico del controller.
    pub failed: bool,
    /// `true` se l'azione ripetuta e' l'esecuzione di un comando di
    /// build/test riconosciuto (vedi [`is_build_or_test_command`]). Segnale
    /// STRUTTURATO (tool_name + primo token del comando), mai un `contains()`
    /// sulla label leggibile: quella include il bersaglio per intero, e un
    /// path come `frontend-test-utils` la farebbe scattare per errore (bug
    /// reale chiuso il 30/07/2026 in `progress_controller`).
    pub is_build_or_test: bool,
}

/// Vocabolario dei comandi di build/test riconosciuti dal PRIMO TOKEN del
/// comando (mai da un `contains()` su tutta la command line: un path o un
/// pacchetto che CONTIENE "test" — es. `pnpm install
/// packages/frontend-test-utils` — non deve bastare). Specchio ridotto dello
/// storico vocabolario di `progress_controller::BUILD_TEST_LABEL_KEYWORDS`,
/// che matchava anche parole generiche (`test`, `lint`, `compile`) proprio
/// come substring: qui restano solo comandi ESEGUIBILI reali, confrontati per
/// UGUAGLIANZA col primo token.
const BUILD_TEST_FIRST_TOKENS: &[&str] = &[
    "cargo", "npm", "pnpm", "yarn", "tsc", "make", "pytest", "gradle", "mvn", "go", "eslint",
];

/// `true` se l'azione e' l'esecuzione di un COMANDO (`run_command`/
/// `run_service`/`run_in_terminal`) il cui primo token e' un comando di
/// build/test noto. Un tool diverso (`edit_file`, `read_file`, ...) non e'
/// mai build/test a prescindere dal bersaglio: il tool_name struttura la
/// domanda PRIMA di guardare il testo, che e' esattamente il "residuo" che
/// resta da giudicare (regola M: segnale strutturato, poi un confronto
/// deterministico sul solo token rilevante — mai un modello nel ciclo caldo).
fn is_build_or_test_command(tool_name: &str, target: &str) -> bool {
    if !matches!(tool_name, "run_command" | "run_service" | "run_in_terminal") {
        return false;
    }
    let primo_token = target.split_whitespace().next().unwrap_or("");
    // Il token puo' comparire con un path davanti (`./node_modules/.bin/pnpm`):
    // si confronta l'ultimo segmento, non l'intero percorso.
    let comando = primo_token.rsplit(['/', '\\']).next().unwrap_or(primo_token);
    BUILD_TEST_FIRST_TOKENS.contains(&comando)
}

/// Rileva la ripetizione IDENTICA di un'azione produttiva (scrittura/comando),
/// a prescindere dall'esito. Versione RICCA: ritorna [`RepeatedActionHit`] con
/// label, conteggio, nome tool ed esito dell'ultima occorrenza.
///
/// IDENTITA' dell'azione (regola L, punto unico UNIVERSALE): [`build_signature`]
/// `(name, input)` = name + hash dell'INPUT COMPLETO. Lo stesso punto del detector
/// dell'engine ([`crate::decisions::loop_signatures`]): vale per OGNI tool e OGNI
/// argomento, senza whitelist da mantenere. Cosi' due chiamate dello stesso tool
/// che differiscono in QUALSIASI argomento (l'`old_string` di un edit, il range di
/// un read_file, il path di un grep) sono azioni DISTINTE (count 1 ciascuna): solo
/// la chiamata DAVVERO identica fa count>=2. Il bersaglio ([`repeated_action_keys`])
/// serve solo per label/esclusioni, non per l'identita'.
///
/// FALSO-DOPPIONE: le signature la cui PRIMA occorrenza e' RIUSCITA
/// (`tool_result_outcome_after == Some(false)`) sono ESCLUSE dal conteggio
/// (ridondanza innocua, non stallo). Lookback canonico 24.
pub fn detect_repeated_action_detailed(
    messages: &[Message],
    lookback: usize,
) -> Option<RepeatedActionHit> {
    if messages.is_empty() {
        return None;
    }
    let recent = tail_messages(messages, lookback);
    let mut counts: Vec<(String, i64)> = Vec::new();
    let mut labels: Vec<(String, String)> = Vec::new();
    // sig -> nome tool (per il ramo edit-fallito del controller).
    let mut tool_names: Vec<(String, String)> = Vec::new();
    // sig -> esito dell'ULTIMA occorrenza (true = fallita).
    let mut last_failed: Vec<(String, bool)> = Vec::new();
    // sig -> l'azione e' un comando di build/test riconosciuto (tool_name +
    // primo token, regola M): alimenta `RepeatedActionHit::is_build_or_test`.
    let mut build_test: Vec<(String, bool)> = Vec::new();
    let mut succeeded: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Target (file) con un edit_file/write_file RIUSCITO visto finora nella finestra:
    // una rilettura read-only di uno di questi DOPO la modifica e' VERIFICA del
    // risultato, NON uno stallo (regola H). Senza questa esclusione, il pattern sano
    // "leggi -> modifica -> rileggi per verificare" faceva scattare repeated_action a
    // soglia 2 e ABORTIVA un task GIA' risolto, con recap falso "File toccati: nessuno"
    // (incidente vite.config.ts: edit applicato, poi rilettura -> falso loop -> abort).
    let mut modified_targets: std::collections::HashSet<String> = std::collections::HashSet::new();
    // RILETTURA-DOPO-PROGRESSO (regola H, generalizza l'esclusione rilettura-dopo-edit):
    // una ripetizione di tool READ-ONLY NON e' uno stallo se tra l'occorrenza PRECEDENTE
    // della STESSA signature e quella corrente c'e' stata almeno UN'AZIONE PRODUTTIVA (un
    // tool NON read-only: write/edit/run_command/run_service/nexus_db_query/...). E' il
    // pattern del DEBUGGING attivo: rileggi un file per VERIFICARE dopo aver agito, non a
    // vuoto. Senza questa esclusione, due riletture sparse intervallate da ~12 azioni
    // produttive scattavano repeated_action a soglia 2 e ABORTIVANO un agente che stava
    // CONVERGENDO (incidente deepseek-v4-pro: HTTP 500 backend, utente gia' creato, due
    // read_file di index.js a step 18 e 24 -> falso loop -> abort). Solo le read-only
    // ripetute SENZA alcuna azione produttiva in mezzo (rilettura davvero a vuoto) restano
    // stallo. `last_productive_idx` = indice del messaggio dell'ULTIMA azione produttiva
    // vista finora; `read_first_idx` = per ogni signature read-only, l'indice della sua
    // PRIMA occorrenza non ancora "scontata" dal progresso.
    let mut last_productive_idx: Option<usize> = None;
    let mut read_first_idx: Vec<(String, usize)> = Vec::new();
    let mut last_sig: Option<String> = None;
    // sig -> testo dell'ULTIMO tool_result visto (per il confronto
    // output-progresso delle azioni produttive fallite ripetute).
    let mut last_outputs: Vec<(String, String)> = Vec::new();
    for (idx, m) in recent.iter().enumerate() {
        for (name, input) in message_tool_uses(m) {
            let Some(keys) = repeated_action_keys(name) else {
                continue;
            };
            // bersaglio = primo argomento non vuoto fra le chiavi candidate.
            let mut target = String::new();
            for k in keys {
                if let Some(v) = input.get(*k).and_then(Value::as_str) {
                    let v = v.trim();
                    if !v.is_empty() {
                        target = v.to_string();
                        break;
                    }
                }
            }
            if target.is_empty() {
                continue;
            }
            // Esito strutturale dell'occorrenza corrente (primo tool_result dopo).
            let outcome = tool_result_outcome_after(recent, idx, 3);
            // Rilettura-di-verifica: un tool di SOLA LETTURA su un file gia' modificato
            // (edit/write riuscito PRIMA, cronologicamente, nella finestra) non e' una
            // ripetizione-stallo ma la verifica della modifica -> NON conta.
            if is_read_only_repeatable_tool(name) && modified_targets.contains(&target) {
                continue;
            }
            // IDENTITA' UNIVERSALE (regola L): la firma e' l'UNICA definizione di
            // "stessa azione", data da build_signature(name, input) = name + hash
            // dell'INPUT COMPLETO (ordine chiavi irrilevante). E' lo STESSO punto
            // usato dal detector dell'engine (loop_signatures): nessun tool puo'
            // sfuggire, perche' OGNI argomento entra per costruzione (il range di
            // read_file, l'old_string di edit_file, il pattern+path di grep, ...).
            // Niente whitelist di chiavi da mantenere a mano -> niente piu' falsi
            // loop quando si aggiunge un tool/argomento. Il `target` qui sopra resta
            // solo per la label leggibile e per le esclusioni sul bersaglio
            // (rilettura-dopo-edit, falso-doppione).
            let sig = build_signature(name, input);
            // ESCLUSIONE rilettura-dopo-progresso (solo tool READ-ONLY). Per i tool
            // PRODUTTIVI il comportamento resta invariato: aggiornano l'indice di
            // progresso e contano sempre. Per i read-only: se la signature e' gia'
            // comparsa e DOPO la sua prima occorrenza c'e' stata un'azione produttiva
            // (last_productive_idx > prima_occorrenza), questa rilettura e' VERIFICA,
            // non stallo -> non incrementa il conteggio; aggiorna la "prima occorrenza"
            // a quella corrente cosi' una eventuale terza rilettura va misurata di
            // nuovo rispetto al progresso piu' recente.
            if is_read_only_repeatable_tool(name) {
                if let Some((_, first_idx)) = read_first_idx.iter_mut().find(|(s, _)| *s == sig) {
                    if last_productive_idx.is_some_and(|p| p > *first_idx) {
                        *first_idx = idx;
                        continue;
                    }
                } else {
                    read_first_idx.push((sig.clone(), idx));
                }
            } else {
                // Azione produttiva ESEGUITA: segna il progresso che "scusa" le
                // successive riletture read-only delle signature gia' viste.
                last_productive_idx = Some(idx);
            }
            // OUTPUT-PROGRESSO (regola M/H): per un'azione PRODUTTIVA FALLITA
            // ripetuta, se l'esito TESTUALE dell'occorrenza corrente differisce
            // da quello della precedente (confronto STRUTTURALE, outputs_similar:
            // mai semantica del testo), l'azione sta PROGREDENDO — es. `npm run
            // build` rilanciata dopo ogni correzione che fallisce con errori via
            // via diversi — e il conteggio RIPARTE. Solo la ripetizione con lo
            // STESSO esito e' uno stallo (incidente "run_command: npm run build
            // si ripeteva senza ulteriore progresso" su un run che convergeva).
            if !is_read_only_repeatable_tool(name) && outcome == Some(true) {
                let cur_out = tool_result_text_after(recent, idx, 3, Some(OUTPUT_COMPARE_CAP))
                    .unwrap_or_default();
                if let Some((_, prev_out)) = last_outputs.iter_mut().find(|(s, _)| *s == sig) {
                    if !outputs_similar(prev_out, &cur_out) {
                        if let Some((_, c)) = counts.iter_mut().find(|(s, _)| *s == sig) {
                            *c = 0;
                        }
                    }
                    *prev_out = cur_out;
                } else {
                    last_outputs.push((sig.clone(), cur_out));
                }
            }
            bump(&mut counts, &sig);
            let label_value: String = target.chars().take(120).collect();
            set_label(&mut labels, &sig, format!("{name}: {label_value}"));
            set_label(&mut tool_names, &sig, name.to_string());
            // Sul TARGET completo (non troncato), non sulla label: il primo
            // token va letto dal comando vero, non dalla sua resa a 120 char.
            set_bool(&mut build_test, &sig, is_build_or_test_command(name, &target));
            last_sig = Some(sig.clone());
            // Un edit/write RIUSCITO (outcome == Some(false)) segna il target come
            // modificato: le successive riletture read-only dello stesso file sono
            // verifica e vengono escluse dal conteggio (sopra).
            if outcome == Some(false) && matches!(name, "edit_file" | "write_file") {
                modified_targets.insert(target.clone());
            }
            // FALSO-DOPPIONE (solo tool PRODUTTIVI): la prima occorrenza RIUSCITA
            // esclude la signature (ridondanza innocua). Per i tool di SOLA LETTURA
            // la rilettura RIUSCITA ripetuta E' lo stallo (l'agente non avanza),
            // quindi NON va esclusa: conta come ripetizione (regola H).
            if outcome == Some(false) && !is_read_only_repeatable_tool(name) {
                succeeded.insert(sig.clone());
            }
            // Memorizza l'esito dell'ULTIMA occorrenza vista (None -> non fallita).
            set_bool(&mut last_failed, &sig, outcome == Some(true));
        }
    }
    // Rimuove le signature riuscite (mai stallo da abort).
    counts.retain(|(sig, _)| !succeeded.contains(sig));
    let (sig, count) = pick_top(&counts, last_sig.as_deref())?;
    let label = labels
        .iter()
        .find(|(s, _)| *s == sig)
        .map(|(_, l)| l.clone())
        .unwrap_or_else(|| sig.clone());
    let tool_name = tool_names
        .iter()
        .find(|(s, _)| *s == sig)
        .map(|(_, n)| n.clone())
        .unwrap_or_default();
    let failed = last_failed
        .iter()
        .find(|(s, _)| *s == sig)
        .map(|(_, f)| *f)
        .unwrap_or(false);
    let is_build_or_test = build_test
        .iter()
        .find(|(s, _)| *s == sig)
        .map(|(_, f)| *f)
        .unwrap_or(false);
    Some(RepeatedActionHit {
        label,
        count,
        tool_name,
        failed,
        is_build_or_test,
    })
}

/// Variante COMPATTA storica: `(Some(label), count)` o `(None, 0)`. Delega al
/// punto unico [`detect_repeated_action_detailed`] (regola L). Conservata per i
/// call site/test che non hanno bisogno di nome tool ed esito.
pub fn detect_repeated_action(messages: &[Message], lookback: usize) -> (Option<String>, i64) {
    match detect_repeated_action_detailed(messages, lookback) {
        Some(hit) => (Some(hit.label), hit.count),
        None => (None, 0),
    }
}

// ── Helper di conteggio condivisi dai due detector di ripetizione ───────────

/// Valuta il PRIMO tool_result trovato entro `max_ahead` messaggi dopo `idx`
/// e ritorna il suo esito (`break` al primo, come `_detect_repeated_failed_command`).
fn first_tool_result_is_error(recent: &[Message], idx: usize, max_ahead: usize) -> Option<bool> {
    let end = (idx + 1 + max_ahead).min(recent.len());
    for nm in recent.iter().take(end).skip(idx + 1) {
        if let Message::Tool { .. } = nm {
            return message_tool_result_outcome(nm);
        }
    }
    None
}

/// Incrementa il contatore della signature in una lista-associativa ordinata
/// per inserimento (replica `dict[sig] = dict.get(sig,0)+1`).
fn bump(list: &mut Vec<(String, i64)>, sig: &str) {
    if let Some(entry) = list.iter_mut().find(|(s, _)| s == sig) {
        entry.1 += 1;
    } else {
        list.push((sig.to_string(), 1));
    }
}

/// Imposta/aggiorna la label leggibile di una signature.
fn set_label(list: &mut Vec<(String, String)>, sig: &str, label: String) {
    if let Some(entry) = list.iter_mut().find(|(s, _)| s == sig) {
        entry.1 = label;
    } else {
        list.push((sig.to_string(), label));
    }
}

/// Imposta/aggiorna un flag booleano dell'ULTIMA occorrenza di una signature
/// (sovrascrive sempre: alla fine resta il valore della chiamata piu'
/// recente). Punto unico (regola L) per `last_failed` e `build_test`: stessa
/// forma `sig -> bool`, stessa semantica "vince l'ultima".
fn set_bool(list: &mut Vec<(String, bool)>, sig: &str, value: bool) {
    if let Some(entry) = list.iter_mut().find(|(s, _)| s == sig) {
        entry.1 = value;
    } else {
        list.push((sig.to_string(), value));
    }
}

/// Ritorna la signature con chiave massima `(count, sig == last)`, replicando
/// `max(items, key=lambda kv: (kv[1], kv[0] == last))` di Python. `max` tiene il
/// PRIMO massimo a parita' PIENA di chiave (sostituisce solo se STRETTAMENTE
/// maggiore), quindi usiamo `>` e scorriamo in ordine d'inserimento. Il flag
/// `sig == last` (l'ultima signature processata) prevale a parita' di count.
fn pick_top(list: &[(String, i64)], last: Option<&str>) -> Option<(String, i64)> {
    let mut best: Option<&(String, i64)> = None;
    for item in list {
        let item_key = (item.1, Some(item.0.as_str()) == last);
        match best {
            None => best = Some(item),
            Some(b) => {
                let best_key = (b.1, Some(b.0.as_str()) == last);
                if item_key > best_key {
                    best = Some(item);
                }
            }
        }
    }
    best.map(|(s, c)| (s.clone(), *c))
}

// ──────────────────────────────────────────────────────────────────────────
//  Segnale SEMANTICO "esito non compiuto" (_unfulfilled_signal, routing.py)
// ──────────────────────────────────────────────────────────────────────────

/// PUNTO UNICO parametrico (regola L) del segnale "esito non compiuto": la
/// variante su stato/config ([`unfulfilled_signal`]) delega qui; l'executor
/// (config propria `ExecutorConfig`, `conteggio_g1`) e il resoconto onesto
/// (`applica_resoconto_onesto`) chiamano direttamente questa firma coi segnali
/// gia' risolti.
///
/// Precedenza (ADR 0034):
///   1. `declared_outcome` (tool `task_complete`) presente -> la dichiarazione
///      del modello DECIDE: ritorna `false`. E' l'UNICO produttore reale di un
///      esito strutturato nel motore nativo — `closure_judge`/`closure_verdict`
///      non e' mai stato portato (nessun codice nel motore nativo lo scrive,
///      ADR 0034 nota finale: "resta una via complementare NON portata al
///      nativo"). Consultare quel campo non era un fallback dietro un segnale
///      primario: era un ramo morto che nascondeva il vero decisore (regola O).
///   2. altrimenti [`crate::decisions::structural_unfulfilled_signal`] ("ha
///      smesso senza fare nulla": tool disponibili, nessuna tool call in
///      QUESTO turno, turno action-oriented, entro la finestra di forcing) —
///      un segnale REALE calcolato dai campi dello stato corrente, mai
///      un'etichetta lessicale cercata nel testo del modello.
///
/// ADR 0018 fase 3 aveva gia' rimosso la blacklist NARRAZIONE
/// (`detect_unfulfilled_intent`); questo punto elimina il secondo residuo
/// lessicale, `_PENDING_STEPS_LABELS` (62 etichette in 5 lingue + conteggio
/// bullet): un match casuale di "todo"/"next steps"/... nel testo del modello
/// (incluso il testo GIA' DECORATO da blocchi di sistema in alcuni call site)
/// riapriva il turno (G1Continue), alimentava il conteggio di re-route/
/// escalation e sostituiva il resoconto mostrato all'utente — mentre il
/// presunto fallback strutturale (`closure_verdict`) non scattava MAI, quindi
/// il ramo lessicale era di fatto l'UNICO decisore, non una difesa in
/// profondita'. Il vocabolario non va spostato nel DB: configurarlo
/// confermerebbe la premessa sbagliata che si riconosca dalla prosa se restano
/// passi da fare.
pub fn unfulfilled_signal_with(
    declared_outcome_present: bool,
    had_tools_available: bool,
    no_tool_call_this_turn: bool,
    action_oriented: bool,
    iteration: i64,
    max_iteration: i64,
) -> bool {
    if declared_outcome_present {
        return false;
    }
    crate::decisions::structural_unfulfilled_signal(
        had_tools_available,
        no_tool_call_this_turn,
        action_oriented,
        iteration,
        max_iteration,
    )
}

/// Segnale "esito non compiuto" per lo stato/config del grafo: vedi
/// [`unfulfilled_signal_with`] per la precedenza. Usato da `route_after_executor`
/// (reroute G1).
pub fn unfulfilled_signal(state: &AgentState, cfg: &RoutingConfig) -> bool {
    unfulfilled_signal_with(
        super::declared_outcome_kind(state).is_some(),
        super::had_tools(state),
        !super::has_pending(state),
        crate::decisions::turn_action_oriented(state.action_oriented),
        super::iterations(state),
        cfg.tool_choice_forcing_max_iteration,
    )
}

// ──────────────────────────────────────────────────────────────────────────
//  Eleggibilita' final gate (_is_software_task / _final_gate_eligible)
// ──────────────────────────────────────────────────────────────────────────

/// True se il run va trattato come task software. Vedi `_is_software_task`.
/// Due segnali in OR: mutazione filesystem strutturale, oppure intent in whitelist.
/// In Python l'intent e' `state.user_intent or state.intent`: il campo `intent`
/// (non promosso a campo nativo) vive nello schema aperto `extra`.
pub fn is_software_task(state: &AgentState, cfg: &RoutingConfig) -> bool {
    // (1) STRUTTURALE primario: ha mutato il filesystem/progetto.
    if has_filesystem_mutation_in_history(&state.messages, cfg) {
        return true;
    }
    // (1-bis) DELEGA a subagente = lavoro software per PROCURA (incidente run
    // 1daf83b3): il padre non tocca file in prima persona ma il figlio puo'
    // averlo fatto, e il suo summary puo' ALLUCINARE il completamento. Senza
    // questo segnale il final_gate veniva SALTATO in silenzio (pass-through su
    // is_software_task=false) e un run chiudeva 'completed' sulla parola del
    // subagente, senza alcuna verifica oggettiva. Prefix-match STRUTTURALE sul
    // nome del tool di delega (dispatch_subagent / dispatch_subagents), mai
    // sul testo (regola M).
    if ai_tool_use_names(&state.messages)
        .into_iter()
        .any(|name| name.starts_with("dispatch_subagent"))
    {
        return true;
    }
    // (2) Whitelist intent (user_intent, fallback su extra["intent"]).
    let intent = state
        .user_intent
        .as_deref()
        .filter(|s| !s.is_empty())
        .or_else(|| state.extra.get("intent").and_then(Value::as_str))
        .unwrap_or("")
        .to_lowercase();
    if intent.is_empty() {
        return false;
    }
    cfg.final_gate_software_intents.contains(&intent)
}

/// True se per questo stato e' eleggibile la verifica E2E pre-chiusura.
/// Vedi `_final_gate_eligible` (routing.py): esclude plan_phase, richiede gate
/// abilitato + task software + ciclo final_gate sotto il cap.
pub fn final_gate_eligible(state: &AgentState, cfg: &RoutingConfig) -> bool {
    if state.plan_phase_active.unwrap_or(false) {
        return false;
    }
    if !cfg.final_gate_enabled || !is_software_task(state, cfg) {
        return false;
    }
    let cycle = state.final_gate_cycle.unwrap_or(0);
    cycle < cfg.final_gate_max_cycles
}

// ──────────────────────────────────────────────────────────────────────────
//  Isolamento todo (todo_isolation_active, orchestrator_config.py)
// ──────────────────────────────────────────────────────────────────────────

/// True se il run deve eseguire i todo come sub-run ISOLATE sequenziali.
/// Vedi `todo_isolation_active`. Richiede TUTTE e tre: plan_phase_active True,
/// modalita' autonoma, setting abilitato.
pub fn todo_isolation_active(state: &AgentState, cfg: &RoutingConfig) -> bool {
    if state.plan_phase_active != Some(true) {
        return false;
    }
    if !state.is_autonomous_run() {
        return false;
    }
    cfg.todo_isolation_enabled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AutomationMode, ToolUse};
    use serde_json::json;

    fn ai_with_tool(name: &str) -> Message {
        Message::Ai {
            content: MessageContent::text(""),
            tool_calls: vec![ToolUse {
                id: "c1".into(),
                name: name.into(),
                input: json!({}),
                thought_signature: None,
            }],
            reasoning: None,
            thinking_signature: None,
        }
    }

    #[test]
    fn summarize_azioni_conteggio_e_ordine() {
        // Conteggio per nome, ordine di prima apparizione, formato "N azioni (...)".
        let msgs = vec![
            ai_with_tool("write_file"),
            ai_with_tool("run_command"),
            ai_with_tool("write_file"),
        ];
        assert_eq!(
            summarize_actions_in_history(&msgs).as_deref(),
            Some("3 azioni (write_file x2, run_command)")
        );
        // Nessun tool_use -> None (turno senza azioni).
        assert_eq!(summarize_actions_in_history(&[]), None);
    }

    fn ai_with_block_tool(name: &str) -> Message {
        Message::Ai {
            content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                id: "c1".into(),
                name: name.into(),
                input: json!({}),
                thought_signature: None,
            }]),
            tool_calls: vec![],
            reasoning: None,
            thinking_signature: None,
        }
    }

    #[test]
    fn productive_action_da_tool_calls() {
        assert!(has_productive_action_in_history(&[ai_with_tool(
            "write_file"
        )]));
        // Solo esplorazione -> non produttivo.
        assert!(!has_productive_action_in_history(&[ai_with_tool(
            "read_file"
        )]));
    }

    #[test]
    fn productive_action_da_blocchi() {
        // Forma Anthropic (== anthropic_content Python).
        assert!(has_productive_action_in_history(&[ai_with_block_tool(
            "edit_file"
        )]));
        assert!(!has_productive_action_in_history(&[ai_with_block_tool(
            "grep"
        )]));
    }

    // Helper: AIMessage con tool_use (forma anthropic_content) + input.
    fn ai_tool_input(name: &str, input: Value) -> Message {
        Message::Ai {
            content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                id: "c1".into(),
                name: name.into(),
                input,
                thought_signature: None,
            }]),
            tool_calls: vec![],
            reasoning: None,
            thinking_signature: None,
        }
    }

    // Helper: HumanMessage con anthropic_content tool_result strutturato.
    fn human_tool_result(exit_code: Option<i64>, is_error: bool, text: &str) -> Message {
        Message::Human {
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: Value::String(text.into()),
                is_error,
                exit_code,
            }]),
        }
    }

    // Helper: ToolMessage langchain con content testuale.
    fn tool_msg(text: &str) -> Message {
        Message::Tool {
            tool_call_id: "c1".into(),
            content: MessageContent::text(text),
        }
    }

    /// Il testo di un tool_result di ricerca nella forma che il tool emette:
    /// `payload_ricerca(...).to_string()`. `zavorra` allunga il testo dei chunk,
    /// per riprodurre un payload REALE (con 4 chunk sta ben oltre i 4000
    /// caratteri di [`OUTPUT_COMPARE_CAP`]).
    fn testo_ricerca(query: &str, hits: &[(&str, i64)], zavorra: usize) -> String {
        let h: Vec<Value> = hits
            .iter()
            .map(|(fonte, chunk)| {
                json!({
                    "source_id": fonte,
                    "chunk_index": chunk,
                    "score": 0.9,
                    "text": "x".repeat(zavorra),
                })
            })
            .collect();
        json!({"query": query, "count": h.len(), "hits": h}).to_string()
    }

    /// Un giro di ricerca: la tool call e il suo risultato.
    fn giro_ricerca(query: &str, hits: &[(&str, i64)], zavorra: usize) -> Vec<Message> {
        vec![
            ai_tool_input("nexus_search_semantic", json!({"query": query})),
            human_tool_result(None, false, &testo_ricerca(query, hits, zavorra)),
        ]
    }

    const RICERCA: &str = "nexus_search_semantic";

    #[test]
    fn stesso_esito_con_query_diverse_e_stallo() {
        // IL FATTO MISURATO (prova-fix-10-08): query sempre diverse, stessi hit.
        let stessi = [("index.html", 0), ("index.html", 2)];
        let mut msgs = Vec::new();
        for q in ["card prodotto", "card prodotto product card", "card prodotto HTML"] {
            msgs.extend(giro_ricerca(q, &stessi, 0));
        }
        let attivi = vec![RICERCA.to_string()];
        assert!(
            ricerca_senza_nuovi_risultati(&msgs, &attivi, 24, 3).is_some(),
            "tre risposte identiche a tre domande diverse sono uno stallo"
        );
        // Il presidio storico non lo vede, ed e' la ragione per cui questo esiste.
        let firme: Vec<String> = ["card prodotto", "card prodotto product card", "card prodotto HTML"]
            .iter()
            .map(|q| build_signature(RICERCA, &json!({"query": q})))
            .collect();
        assert_eq!(
            firme.iter().collect::<std::collections::HashSet<_>>().len(),
            3,
            "le firme d'INPUT restano tutte diverse"
        );
    }

    #[test]
    fn payload_oltre_il_cap_del_confronto_testuale_resta_leggibile() {
        // REGRESSIONE DI PROGETTO: con il taglio a OUTPUT_COMPARE_CAP il JSON
        // arriva troncato e non si parsa -> il criterio tacerebbe proprio sulle
        // ricerche piu' ricche, che sono quelle che costano. Quattro chunk da
        // 2000 caratteri: ~8000, il doppio del cap.
        let stessi = [("a.md", 0), ("a.md", 1), ("b.md", 0), ("b.md", 1)];
        let mut msgs = Vec::new();
        for q in ["uno", "due", "tre"] {
            msgs.extend(giro_ricerca(q, &stessi, 2000));
        }
        assert!(
            testo_ricerca("uno", &stessi, 2000).len() > OUTPUT_COMPARE_CAP * 2,
            "il payload di prova deve superare il cap, altrimenti non prova nulla"
        );
        assert!(
            ricerca_senza_nuovi_risultati(&msgs, &[RICERCA.to_string()], 24, 3).is_some(),
            "il payload va letto INTERO: un JSON troncato non si deserializza"
        );
    }

    #[test]
    fn una_scrittura_in_mezzo_azzera_la_serie() {
        // Ricerca ripetuta DOPO lavoro: e' una verifica, e puo' legittimamente
        // dare lo stesso esito.
        let stessi = [("index.html", 0)];
        let mut msgs = giro_ricerca("uno", &stessi, 0);
        msgs.extend(giro_ricerca("due", &stessi, 0));
        msgs.push(ai_tool_input("edit_file", json!({"path": "index.html"})));
        msgs.push(human_tool_result(None, false, "ok"));
        msgs.extend(giro_ricerca("tre", &stessi, 0));
        assert_eq!(
            ricerca_senza_nuovi_risultati(&msgs, &[RICERCA.to_string()], 24, 3),
            None,
            "dopo una scrittura la ri-verifica riparte da zero"
        );
    }

    #[test]
    fn niente_stallo_se_il_turno_non_cerca_piu() {
        let stessi = [("index.html", 0)];
        let mut msgs = Vec::new();
        for q in ["uno", "due", "tre"] {
            msgs.extend(giro_ricerca(q, &stessi, 0));
        }
        // Il turno corrente scrive: la ripetizione passata non e' un motivo per
        // fermarlo adesso.
        assert_eq!(
            ricerca_senza_nuovi_risultati(&msgs, &["edit_file".to_string()], 24, 3),
            None
        );
    }

    #[test]
    fn esiti_diversi_non_sono_stallo() {
        let mut msgs = giro_ricerca("uno", &[("a.md", 0)], 0);
        msgs.extend(giro_ricerca("due", &[("b.md", 0)], 0));
        msgs.extend(giro_ricerca("tre", &[("c.md", 0)], 0));
        assert_eq!(
            ricerca_senza_nuovi_risultati(&msgs, &[RICERCA.to_string()], 24, 3),
            None,
            "ogni giro ha trovato qualcosa di nuovo"
        );
        // Zero risultati: variare la query e' esplorazione legittima.
        let mut vuoti = Vec::new();
        for q in ["uno", "due", "tre"] {
            vuoti.extend(giro_ricerca(q, &[], 0));
        }
        assert_eq!(
            ricerca_senza_nuovi_risultati(&vuoti, &[RICERCA.to_string()], 24, 3),
            None
        );
    }

    #[test]
    fn turno_con_piu_tool_non_produce_firma_d_esito() {
        // Il risultato non e' attribuibile al singolo tool_use (la history non
        // porta qui il tool_use_id): su un'attribuzione incerta non si decide.
        let stessi = [("index.html", 0)];
        let mut msgs = Vec::new();
        for q in ["uno", "due", "tre"] {
            msgs.push(Message::Ai {
                content: MessageContent::Blocks(vec![
                    ContentBlock::ToolUse {
                        id: "c1".into(),
                        name: RICERCA.into(),
                        input: json!({"query": q}),
                        thought_signature: None,
                    },
                    ContentBlock::ToolUse {
                        id: "c2".into(),
                        name: "read_file".into(),
                        input: json!({"path": "index.html"}),
                        thought_signature: None,
                    },
                ]),
                tool_calls: vec![],
                reasoning: None,
                thinking_signature: None,
            });
            msgs.push(human_tool_result(None, false, &testo_ricerca(q, &stessi, 0)));
        }
        assert_eq!(
            ricerca_senza_nuovi_risultati(&msgs, &[RICERCA.to_string()], 24, 3),
            None
        );
    }

    #[test]
    fn runtime_observation_tools_e_sottoinsieme_di_exploration_only() {
        for tool in RUNTIME_OBSERVATION_TOOLS {
            assert!(
                EXPLORATION_ONLY_TOOLS.contains(tool),
                "{tool} deve comparire anche in EXPLORATION_ONLY_TOOLS"
            );
        }
    }

    #[test]
    fn recent_ai_turn_counts_ignora_produttivo_sempre_fallito() {
        // Un tool produttivo (fuori da EXPLORATION_ONLY_TOOLS) il cui esito e'
        // SEMPRE errore non e' evidenza di progresso: non deve contare come
        // "produttivo" (chiude il buco minore: prima si guardava solo il NOME).
        let msgs = vec![
            ai_tool_input("edit_file", json!({"path": "a.rs"})),
            human_tool_result(Some(1), true, "errore: file non trovato"),
        ];
        assert_eq!(recent_ai_turn_counts(&msgs, 16), (1, 0, 0));
    }

    #[test]
    fn recent_ai_turn_counts_monitoraggio_runtime_conta_solo_se_successo() {
        // Tool di osservazione runtime con esito RIUSCITO -> monitoring_turns=1.
        let ok = vec![
            ai_tool_input(READ_SERVICE_OUTPUT_TOOL, json!({})),
            human_tool_result(None, false, "log stabile"),
        ];
        assert_eq!(recent_ai_turn_counts(&ok, 16), (1, 0, 1));

        // Stesso tool ma esito FALLITO -> non e' monitoraggio legittimo.
        let failing = vec![
            ai_tool_input(READ_SERVICE_OUTPUT_TOOL, json!({})),
            human_tool_result(None, true, "errore: servizio non raggiungibile"),
        ];
        assert_eq!(recent_ai_turn_counts(&failing, 16), (1, 0, 0));

        // Turno che mischia un tool di osservazione runtime con un tool
        // statico (read_file): non e' "tutto monitoraggio runtime".
        let mixed = Message::Ai {
            content: MessageContent::Blocks(vec![
                ContentBlock::ToolUse {
                    id: "c1".into(),
                    name: READ_SERVICE_OUTPUT_TOOL.into(),
                    input: json!({}),
                    thought_signature: None,
                },
                ContentBlock::ToolUse {
                    id: "c2".into(),
                    name: "read_file".into(),
                    input: json!({"path": "a.rs"}),
                    thought_signature: None,
                },
            ]),
            tool_calls: vec![],
            reasoning: None,
            thinking_signature: None,
        };
        let mixed_msgs = vec![mixed, human_tool_result(None, false, "ok")];
        assert_eq!(recent_ai_turn_counts(&mixed_msgs, 16), (1, 0, 0));
    }

    #[test]
    fn verifying_after_productive_work_richiede_lavoro_pregresso_e_monitoraggio_totale() {
        // Storia: 1 round produttivo + 2 round di monitoraggio runtime riuscito.
        let messages = vec![
            ai_tool_input("write_file", json!({"path": "a.rs"})),
            human_tool_result(None, false, "scritto"),
            ai_tool_input(READ_SERVICE_OUTPUT_TOOL, json!({})),
            human_tool_result(None, false, "log 1"),
            ai_tool_input(TAIL_SERVICE_LOGS_TOOL, json!({})),
            human_tool_result(None, false, "log 2"),
        ];
        // Finestra di 4 messaggi = ultimi 2 round, ENTRAMBI monitoraggio: il
        // lavoro produttivo e' fuori finestra ma resta visibile nella history
        // intera -> credito concesso.
        let (ai_turns, productive, monitoring) = recent_ai_turn_counts(&messages, 4);
        assert_eq!((ai_turns, productive, monitoring), (2, 0, 2));
        assert!(verifying_after_productive_work(
            &messages, ai_turns, monitoring
        ));

        // Stessa finestra ma SENZA alcun lavoro produttivo in tutta la history
        // (il round write_file non esiste) -> nessun credito: un run che si
        // limita a monitorare senza aver mai agito resta soggetto al gate.
        let solo_monitoraggio = &messages[2..];
        let (ai_turns_sm, _prod_sm, monitoring_sm) = recent_ai_turn_counts(solo_monitoraggio, 4);
        assert!(!verifying_after_productive_work(
            solo_monitoraggio,
            ai_turns_sm,
            monitoring_sm
        ));

        // Un tool STATICO (read_file, non in RUNTIME_OBSERVATION_TOOLS) nella
        // finestra rompe "tutto monitoraggio": niente credito, il loop
        // descrittivo generico resta intercettato.
        let mut mixed = messages[..4].to_vec();
        mixed.push(ai_tool_input("read_file", json!({"path": "b.rs"})));
        mixed.push(human_tool_result(None, false, "contenuto"));
        let (ai_turns_mx, _prod_mx, monitoring_mx) = recent_ai_turn_counts(&mixed, 4);
        assert!(!verifying_after_productive_work(
            &mixed,
            ai_turns_mx,
            monitoring_mx
        ));
    }

    #[test]
    fn completed_subagent_dispatch_da_history() {
        // Payload REALE del ponte figlio->padre (finalize_success): il tool_result
        // e' una STRINGA JSON con `subagent_run_id` + `status`.
        let ok = r#"{"subagent_run_id":"11111111-1111-1111-1111-111111111111","kind":"rust_implementer","status":"completed","summary":"riscritto AppointmentsTab","iterations":56,"cost_usd":0.1}"#;
        // Forma HumanMessage con blocco tool_result (content = stringa JSON).
        assert!(has_completed_subagent_dispatch(&[human_tool_result(
            None, false, ok
        )]));
        // Forma ToolMessage langchain con content testuale.
        assert!(has_completed_subagent_dispatch(&[tool_msg(ok)]));
        // Solo `status == "completed"` conta: paused/failed/timeout/running NO.
        for st in ["paused", "failed", "timeout", "running"] {
            let other = format!(r#"{{"subagent_run_id":"abc","status":"{st}","summary":"x"}}"#);
            assert!(
                !has_completed_subagent_dispatch(&[tool_msg(&other)]),
                "status {st} non deve contare come completamento"
            );
        }
        // Un tool_result NON-subagente (nessun subagent_run_id) non conta.
        assert!(!has_completed_subagent_dispatch(&[tool_msg(
            r#"{"status":"completed","output":"build ok"}"#
        )]));
        // Payload gia' strutturato (Value::Object) invece di stringa JSON.
        let structured = Message::Human {
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: json!({"subagent_run_id": "abc", "status": "completed"}),
                is_error: false,
                exit_code: None,
            }]),
        };
        assert!(has_completed_subagent_dispatch(&[structured]));
        // History senza sub-run -> false.
        assert!(!has_completed_subagent_dispatch(&[tool_msg(
            "nessun subrun"
        )]));
    }

    #[test]
    fn outputs_similar_confronto_strutturale() {
        // Stesso errore con la sola riga di durata diversa -> SIMILI.
        let a = "vite build\nerror TS2345 in bookingService.ts:42\nfailed\nDone in 741ms\nl1\nl2\nl3\nl4\nl5\nl6";
        let b = "vite build\nerror TS2345 in bookingService.ts:42\nfailed\nDone in 488ms\nl1\nl2\nl3\nl4\nl5\nl6";
        assert!(outputs_similar(a, b));
        // Errori DIVERSI -> non simili (progresso).
        let c = "vite build\nerror TS2551 in LoginPage.tsx:10\nfailed\nDone in 500ms";
        assert!(!outputs_similar(a, c));
        // Entrambi vuoti -> simili.
        assert!(outputs_similar("", ""));
    }

    #[test]
    fn build_ripetuta_con_errori_diversi_non_e_stallo() {
        // REGRESSIONE "run_command: npm run build si ripeteva senza ulteriore
        // progresso": la STESSA firma (input identico) fallita 3 volte ma con
        // OUTPUT DIVERSI (un errore corretto per volta) e' PROGRESSO -> il
        // conteggio riparte a ogni esito nuovo e il detector NON scatta.
        let build = || ai_tool_input("run_command", json!({"command": "npm run build"}));
        let err = |t: &str| human_tool_result(Some(1), true, t);
        let msgs = vec![
            build(),
            err("error TS1 in a.ts:1\nriga2\nriga3\nriga4"),
            build(),
            err("error TS2 in b.ts:9\naltra2\naltra3\naltra4"),
            build(),
            err("error TS3 in c.ts:5\nancora2\nancora3\nancora4"),
        ];
        let hit = detect_repeated_action_detailed(&msgs, 24);
        assert!(
            hit.as_ref().map(|h| h.count).unwrap_or(0) < 2,
            "build con errori diversi non deve contare come ripetizione: {hit:?}"
        );
    }

    #[test]
    fn build_ripetuta_con_stesso_errore_e_stallo() {
        // Contro-prova: 3 build fallite con lo STESSO output -> stallo vero.
        let build = || ai_tool_input("run_command", json!({"command": "npm run build"}));
        let stesso = "error TS1 in a.ts:1\nriga2\nriga3\nriga4";
        let msgs = vec![
            build(),
            human_tool_result(Some(1), true, stesso),
            build(),
            human_tool_result(Some(1), true, stesso),
            build(),
            human_tool_result(Some(1), true, stesso),
        ];
        let hit = detect_repeated_action_detailed(&msgs, 24).expect("stallo atteso");
        assert!(hit.count >= 3);
        assert!(hit.failed);
    }

    /// Il difetto reale (30/07/2026): il vecchio `is_build_or_test_label` di
    /// `progress_controller` cercava le keyword su TUTTA la label
    /// (`"{tool}: {bersaglio}"`), quindi un bersaglio che CONTIENE "test" per
    /// caso (un path, un nome di pacchetto) bastava a far scattare il ramo
    /// "e' un build/test" — che nel force_diagnose ordina di NON dichiararsi
    /// bloccati anche quando la causa e' davvero esterna. Il segnale
    /// STRUTTURATO deve venire dal tool_name + dal PRIMO TOKEN del comando,
    /// mai da un `contains()` sull'intero bersaglio.
    ///
    /// MUTAZIONE: tornare a un `contains("test")` sull'intero `target` in
    /// [`is_build_or_test_command`] rende rosso questo test.
    #[test]
    fn is_build_or_test_ignora_il_resto_del_bersaglio() {
        let cerca = || {
            ai_tool_input(
                "run_command",
                json!({"command": "ls packages/frontend-test-utils"}),
            )
        };
        let msgs = vec![
            cerca(),
            human_tool_result(Some(1), true, "ENOENT"),
            cerca(),
            human_tool_result(Some(1), true, "ENOENT"),
        ];
        let hit = detect_repeated_action_detailed(&msgs, 24).expect("stallo atteso");
        assert!(
            !hit.is_build_or_test,
            "'ls' non e' un comando di build/test anche se il bersaglio contiene 'test': {hit:?}"
        );
    }

    /// Contro-prova: un comando di build/test VERO viene riconosciuto dal
    /// primo token, anche con un path davanti al binario.
    #[test]
    fn is_build_or_test_riconosce_il_comando_vero() {
        let cerca = || {
            ai_tool_input(
                "run_command",
                json!({"command": "./node_modules/.bin/pnpm test --filter api"}),
            )
        };
        let msgs = vec![
            cerca(),
            human_tool_result(Some(1), true, "1 failing"),
            cerca(),
            human_tool_result(Some(1), true, "1 failing"),
        ];
        let hit = detect_repeated_action_detailed(&msgs, 24).expect("stallo atteso");
        assert!(hit.is_build_or_test, "'pnpm test' e' un comando di test: {hit:?}");
    }

    /// Un tool DIVERSO da run_command/run_service/run_in_terminal non e' MAI
    /// build/test, a prescindere da cosa contenga il bersaglio: elimina per
    /// costruzione il falso positivo su path (`edit_file` su un file il cui
    /// nome contiene "test").
    #[test]
    fn is_build_or_test_falso_per_costruzione_su_tool_non_comando() {
        assert!(!is_build_or_test_command(
            "edit_file",
            "cargo test --workspace"
        ));
        assert!(!is_build_or_test_command(
            "read_file",
            "src/build_test_runner.rs"
        ));
    }

    #[test]
    fn signature_output_progress_true_su_esiti_diversi() {
        let sig = build_signature("run_command", &json!({"command": "npm run build"}));
        let build = || ai_tool_input("run_command", json!({"command": "npm run build"}));
        // Esiti diversi -> progresso.
        let msgs = vec![
            build(),
            human_tool_result(Some(1), true, "error TS1 in a.ts\nx\ny\nz"),
            build(),
            human_tool_result(Some(1), true, "error TS2 in b.ts\nk\nw\nq"),
        ];
        assert!(repeated_signature_output_progress(&msgs, &sig, 24));
        // Esiti uguali -> nessun progresso.
        let stesso = "error TS1 in a.ts\nx\ny\nz";
        let msgs2 = vec![
            build(),
            human_tool_result(Some(1), true, stesso),
            build(),
            human_tool_result(Some(1), true, stesso),
        ];
        assert!(!repeated_signature_output_progress(&msgs2, &sig, 24));
        // Una sola occorrenza -> nessun progresso dichiarabile.
        let msgs3 = vec![build(), human_tool_result(Some(1), true, stesso)];
        assert!(!repeated_signature_output_progress(&msgs3, &sig, 24));
    }

    #[test]
    fn has_tool_calls_history() {
        assert!(has_tool_calls_in_history(&[ai_with_tool("read_file")]));
        assert!(!has_tool_calls_in_history(&[Message::Ai {
            content: MessageContent::text("solo testo"),
            tool_calls: vec![],
            reasoning: None,
            thinking_signature: None,
        }]));
    }

    #[test]
    fn outcome_after_exit_code_primario() {
        // tool_use seguito da tool_result con exit_code=0 -> successo.
        let msgs = vec![
            ai_tool_input("run_command", json!({"command": "ls"})),
            human_tool_result(Some(0), false, "ok"),
        ];
        assert_eq!(tool_result_outcome_after(&msgs, 0, 3), Some(false));
        // exit_code != 0 -> errore, anche senza is_error.
        let msgs2 = vec![
            ai_tool_input("run_command", json!({"command": "ls"})),
            human_tool_result(Some(2), false, "tutto bene a parole"),
        ];
        assert_eq!(tool_result_outcome_after(&msgs2, 0, 3), Some(true));
        // Nessun risultato dopo -> None.
        let msgs3 = vec![ai_tool_input("run_command", json!({"command": "ls"}))];
        assert_eq!(tool_result_outcome_after(&msgs3, 0, 3), None);
    }

    #[test]
    fn outcome_after_is_error_strutturato_vince_sul_testo() {
        // Niente exit_code, is_error=false STRUTTURATO: successo, anche se il
        // testo del risultato contiene parole della vecchia lista di hint
        // lessicali ("not found", "timeout", "error"). E' esattamente il difetto
        // corretto: un read_file riuscito che restituisce un sorgente con la
        // parola "timeout" NON deve contare come tool fallito (regola M).
        let msgs = vec![
            ai_tool_input("read_file", json!({"path": "retry.rs"})),
            human_tool_result(
                None,
                false,
                "fn handle_timeout() { /* not found in cache: error path */ }",
            ),
        ];
        assert_eq!(tool_result_outcome_after(&msgs, 0, 3), Some(false));
        // is_error=true STRUTTURATO: errore, anche con un content innocuo.
        let msgs_err = vec![
            ai_tool_input("run_command", json!({"command": "x"})),
            human_tool_result(None, true, "tutto bene a parole"),
        ];
        assert_eq!(tool_result_outcome_after(&msgs_err, 0, 3), Some(true));
    }

    #[test]
    fn tool_message_testo_piatto_legge_il_contratto_marker() {
        // Un ToolMessage (langchain) con content testuale, senza blocchi
        // strutturati: l'unico segnale disponibile e' il marker di dichiarazione
        // del fallimento (regola L: punto unico nexus_types::tool_outcome), mai
        // una parola pescata nel testo.
        let fallito = tool_msg("\u{274C} directory non leggibile");
        assert_eq!(message_tool_result_outcome(&fallito), Some(true));
        // Un tool RIUSCITO che nomina "error"/"not found" nel proprio output
        // (es. il contenuto di un file letto) non e' un fallimento.
        let riuscito = tool_msg("grep: 3 matches for \"error: not found\"");
        assert_eq!(message_tool_result_outcome(&riuscito), Some(false));
    }

    #[test]
    fn redaction_rejected_da_codice_strutturato() {
        // SEGNALE STRUTTURATO (regola M): un tool_result che porta il codice
        // sentinella [REDACTION_REJECTED] -> true; il testo umano del placeholder
        // ([REDACTED:...]) da solo NON basta (non e' il codice macchina).
        let rifiutato = vec![
            ai_tool_input("run_command", json!({"command": "x"})),
            human_tool_result(
                None,
                true,
                "\u{274C} [REDACTION_REJECTED] [BLOCCATO — placeholder di redazione nell'input]",
            ),
        ];
        assert!(recent_redaction_rejected(&rifiutato, 16));
        // Un tool_result che MENZIONA il placeholder umano ma NON porta il codice
        // strutturato non conta (evita falsi positivi da prosa/log).
        let solo_prosa = vec![
            ai_tool_input("read_file", json!({"path": ".env"})),
            human_tool_result(Some(0), false, "ADMIN_EMAIL=[REDACTED:email_pii]"),
        ];
        assert!(!recent_redaction_rejected(&solo_prosa, 16));
        // Nessun tool_result -> false.
        assert!(!recent_redaction_rejected(&[], 16));
    }

    #[test]
    fn conta_request_port_senza_filtro_label() {
        let msgs = vec![
            ai_tool_input("request_port", json!({"label": "web"})),
            ai_tool_input("request_port", json!({"label": "api"})),
            ai_tool_input("read_file", json!({"path": "a"})),
        ];
        assert_eq!(count_recent_request_port(&msgs, 16), 2);
        assert!(has_active_resources_in_history(&msgs, 24));
        // Senza request_port/servizi -> nessuna risorsa attiva.
        let solo_read = vec![ai_tool_input("read_file", json!({"path": "a"}))];
        assert!(!has_active_resources_in_history(&solo_read, 24));
    }

    #[test]
    fn recent_tool_error_solo_tool_message() {
        // Ultimo ToolMessage col marker di fallimento -> errore.
        assert!(detect_recent_tool_error(
            &[tool_msg("\u{274C} build failed")],
            4
        ));
        // ToolMessage pulito -> nessun errore, anche se nomina "error" nel testo
        // (un log riportato, non una dichiarazione di fallimento del TOOL).
        assert!(!detect_recent_tool_error(
            &[tool_msg("done ok, 0 error in log")],
            4
        ));
    }

    #[test]
    fn tool_error_stats_conta_errori_e_streak() {
        // Nessun tool_result -> (0, 0).
        assert_eq!(tool_error_stats(&[], 40), (0, 0));
        // Tre ok consecutivi in coda -> error_count 0, streak 3.
        let all_ok = vec![
            tool_msg("done ok"),
            tool_msg("done ok"),
            tool_msg("done ok"),
        ];
        assert_eq!(tool_error_stats(&all_ok, 40), (0, 3));
        // Un errore in coda -> error_count 1, streak 0 (l'ultimo e' errore). Il
        // tool DICHIARA il fallimento col marker (regola L/M): un tool riuscito
        // che nomina "error" nel proprio output non deve contare (era il difetto
        // corretto altrove in questo file).
        let last_err = vec![tool_msg("done ok"), tool_msg("\u{274C} build failed")];
        assert_eq!(tool_error_stats(&last_err, 40), (1, 0));
        // Errore in mezzo, poi due ok in coda -> error_count 1, streak 2 (la streak
        // parte dall'ultimo all'indietro e si ferma al primo errore).
        let mixed = vec![
            tool_msg("\u{274C} fallito"),
            tool_msg("done ok"),
            tool_msg("done ok"),
        ];
        assert_eq!(tool_error_stats(&mixed, 40), (1, 2));
    }

    #[test]
    fn repeated_failed_command_stessa_signature() {
        // Stesso comando fallito 2 volte -> rilevato. _detect_repeated_failed_command
        // valuta SOLO i ToolMessage successivi (1:1 col Python, che guarda
        // isinstance(nm, ToolMessage)), quindi qui usiamo tool_msg. Il fallimento
        // e' il contratto macchina (marker), non la prosa "error: ...".
        let msgs = vec![
            ai_tool_input(
                "run_command",
                json!({"command": "npm i", "working_dir": "/p"}),
            ),
            tool_msg("\u{274C} build failed"),
            ai_tool_input(
                "run_command",
                json!({"command": "npm i", "working_dir": "/p"}),
            ),
            tool_msg("\u{274C} build failed"),
        ];
        let (cmd, count) = detect_repeated_failed_command(&msgs, 12);
        assert_eq!(cmd.as_deref(), Some("npm i"));
        assert_eq!(count, 2);
        // Comando RIUSCITO (ToolMessage pulito) -> non contato.
        let ok = vec![
            ai_tool_input("run_command", json!({"command": "npm i"})),
            tool_msg("done ok"),
        ];
        assert_eq!(detect_repeated_failed_command(&ok, 12), (None, 0));
    }

    #[test]
    fn repeated_action_esclude_signature_riuscita() {
        // edit_file applicato con successo poi ri-emesso e fallito: la prima
        // occorrenza riuscita ESCLUDE la signature dal conteggio (falso-doppione).
        let msgs = vec![
            ai_tool_input("edit_file", json!({"path": "a.rs"})),
            human_tool_result(Some(0), false, "applied"),
            ai_tool_input("edit_file", json!({"path": "a.rs"})),
            human_tool_result(None, true, "old_string non trovato"),
        ];
        assert_eq!(detect_repeated_action(&msgs, 24), (None, 0));
        // Stessa scrittura ripetuta SENZA mai riuscire -> stallo rilevato.
        let stallo = vec![
            ai_tool_input("write_file", json!({"path": "b.rs"})),
            human_tool_result(None, true, "permission denied"),
            ai_tool_input("write_file", json!({"path": "b.rs"})),
            human_tool_result(None, true, "permission denied"),
        ];
        let (label, count) = detect_repeated_action(&stallo, 24);
        assert_eq!(label.as_deref(), Some("write_file: b.rs"));
        assert_eq!(count, 2);
    }

    #[test]
    fn repeated_action_edit_old_string_diverso_non_stallo() {
        // Due edit_file sullo STESSO path ma con old_string DIVERSI: il secondo
        // e' la CORREZIONE del primo, non una ripetizione a vuoto. Con la
        // signature sensibile al contenuto sono azioni DISTINTE -> count 1
        // ciascuna -> nessuno stallo (soglia 2 non raggiunta da nessuna).
        let msgs = vec![
            ai_tool_input(
                "edit_file",
                json!({"path": "src/lib.rs", "old_string": "fn alpha() {}"}),
            ),
            human_tool_result(None, true, "old_string non trovato"),
            ai_tool_input(
                "edit_file",
                json!({"path": "src/lib.rs", "old_string": "fn beta() {}"}),
            ),
            human_tool_result(None, true, "old_string non trovato"),
        ];
        let hit = detect_repeated_action_detailed(&msgs, 24);
        // La signature vincente ha count 1: sotto la soglia 2, l'executor non
        // considera stallo. Verifichiamo che nessuna signature arrivi a 2.
        assert_eq!(hit.as_ref().map(|h| h.count), Some(1));
    }

    #[test]
    fn repeated_action_edit_old_string_identico_stallo() {
        // Due edit_file IDENTICI (stesso path + stesso old_string) entrambi
        // falliti: e' una ripetizione a vuoto reale -> count 2 -> stallo.
        let msgs = vec![
            ai_tool_input(
                "edit_file",
                json!({"path": "src/lib.rs", "old_string": "fn alpha() {}"}),
            ),
            human_tool_result(None, true, "old_string non trovato"),
            ai_tool_input(
                "edit_file",
                json!({"path": "src/lib.rs", "old_string": "fn alpha() {}"}),
            ),
            human_tool_result(None, true, "old_string non trovato"),
        ];
        let hit = detect_repeated_action_detailed(&msgs, 24).expect("stallo atteso");
        assert_eq!(hit.label, "edit_file: src/lib.rs");
        assert_eq!(hit.count, 2);
        assert_eq!(hit.tool_name, "edit_file");
        assert!(hit.failed, "l'ultima occorrenza e' fallita");
    }

    #[test]
    fn repeated_action_edit_identico_riuscito_non_stallo() {
        // Stesso path + stesso old_string ma la PRIMA occorrenza RIESCE:
        // ridondanza innocua (falso-doppione), nessuno stallo.
        let msgs = vec![
            ai_tool_input(
                "edit_file",
                json!({"path": "src/lib.rs", "old_string": "fn alpha() {}"}),
            ),
            human_tool_result(Some(0), false, "applied"),
            ai_tool_input(
                "edit_file",
                json!({"path": "src/lib.rs", "old_string": "fn alpha() {}"}),
            ),
            human_tool_result(None, true, "old_string non trovato"),
        ];
        assert_eq!(detect_repeated_action(&msgs, 24), (None, 0));
    }

    #[test]
    fn repeated_action_read_only_riuscito_e_stallo() {
        // FIX #2 (NON-convergenza, regola H): una LETTURA ripetuta IDENTICA, anche
        // RIUSCITA entrambe le volte, e' stallo per i read-only (l'agente rilegge
        // lo stesso file senza avanzare). A differenza dei produttivi, la prima
        // occorrenza riuscita NON la esclude dal conteggio.
        let msgs = vec![
            ai_tool_input("read_file", json!({"path": "src/main.rs"})),
            human_tool_result(Some(0), false, "fn main() {}"),
            ai_tool_input("read_file", json!({"path": "src/main.rs"})),
            human_tool_result(Some(0), false, "fn main() {}"),
        ];
        let hit = detect_repeated_action_detailed(&msgs, 24).expect("stallo read-only atteso");
        assert_eq!(hit.label, "read_file: src/main.rs");
        assert_eq!(hit.count, 2);
        assert_eq!(hit.tool_name, "read_file");
        // Letture su path DIVERSI: esplorazione legittima, nessuno stallo.
        let diversi = vec![
            ai_tool_input("read_file", json!({"path": "a.rs"})),
            human_tool_result(Some(0), false, "..."),
            ai_tool_input("read_file", json!({"path": "b.rs"})),
            human_tool_result(Some(0), false, "..."),
        ];
        let hit2 = detect_repeated_action_detailed(&diversi, 24);
        assert_eq!(
            hit2.map(|h| h.count),
            Some(1),
            "path diversi -> nessuno stallo"
        );
    }

    #[test]
    fn repeated_action_read_dopo_edit_e_verifica_non_stallo() {
        // FIX (regola H, incidente vite.config.ts): leggi -> MODIFICA -> rileggi per
        // verificare e' un pattern SANO, non uno stallo. La rilettura read-only DOPO
        // un edit RIUSCITO sullo stesso file NON deve contare come repeated_action
        // (prima faceva scattare l'ABORT a soglia 2 su un task GIA' risolto, con recap
        // falso "File toccati: nessuno").
        let msgs = vec![
            ai_tool_input("read_file", json!({"path": "vite.config.ts"})),
            human_tool_result(Some(0), false, "port: 35198"),
            ai_tool_input(
                "edit_file",
                json!({"path": "vite.config.ts", "old_string": "35198"}),
            ),
            human_tool_result(Some(0), false, "applied"),
            ai_tool_input("read_file", json!({"path": "vite.config.ts"})),
            human_tool_result(Some(0), false, "port: process.env.PORT"),
        ];
        let count = detect_repeated_action_detailed(&msgs, 24)
            .map(|h| h.count)
            .unwrap_or(0);
        assert!(
            count < 2,
            "read-dopo-edit e' verifica, non stallo; count={count}"
        );
        // Controprova: due read IDENTICHE senza edit in mezzo restano stallo.
        let loop_msgs = vec![
            ai_tool_input("read_file", json!({"path": "vite.config.ts"})),
            human_tool_result(Some(0), false, "port: 35198"),
            ai_tool_input("read_file", json!({"path": "vite.config.ts"})),
            human_tool_result(Some(0), false, "port: 35198"),
        ];
        assert_eq!(
            detect_repeated_action_detailed(&loop_msgs, 24).map(|h| h.count),
            Some(2),
            "due read senza edit in mezzo restano stallo"
        );
    }

    #[test]
    fn repeated_action_read_consecutive_senza_progresso_resta_stallo() {
        // CASO 1 (anti-regressione): due read_file IDENTICHE CONSECUTIVE, senza alcuna
        // azione in mezzo, restano stallo reale (count 2). L'esclusione rilettura-dopo-
        // progresso NON deve indebolire l'anti-loop quando l'agente rilegge a vuoto.
        let msgs = vec![
            ai_tool_input("read_file", json!({"path": "backend/index.js"})),
            human_tool_result(Some(0), false, "app.listen(...)"),
            ai_tool_input("read_file", json!({"path": "backend/index.js"})),
            human_tool_result(Some(0), false, "app.listen(...)"),
        ];
        let hit = detect_repeated_action_detailed(&msgs, 24)
            .expect("due read consecutive senza progresso restano stallo");
        assert_eq!(hit.count, 2);
        assert_eq!(hit.tool_name, "read_file");
    }

    #[test]
    fn repeated_action_read_dopo_azione_produttiva_non_stallo() {
        // CASO 2 (fix): read_file A -> run_command (produttiva) -> read_file A identica.
        // La produttiva in mezzo "scusa" la rilettura (verifica/debugging), quindi la
        // signature read-only resta a count 1 -> sotto la soglia 2, nessuno stallo.
        let msgs = vec![
            ai_tool_input("read_file", json!({"path": "backend/index.js"})),
            human_tool_result(Some(0), false, "app.listen(...)"),
            ai_tool_input(
                "run_command",
                json!({"command": "curl -s localhost:3000/health"}),
            ),
            human_tool_result(Some(0), false, "500"),
            ai_tool_input("read_file", json!({"path": "backend/index.js"})),
            human_tool_result(Some(0), false, "app.listen(...)"),
        ];
        let count = detect_repeated_action_detailed(&msgs, 24)
            .map(|h| h.count)
            .unwrap_or(0);
        assert!(
            count < 2,
            "read-dopo-azione-produttiva e' verifica, non stallo; count={count}"
        );
    }

    #[test]
    fn repeated_action_caso_reale_debugging_500_non_stallo() {
        // CASO 3 (incidente deepseek-v4-pro ridotto): durante il debug di un HTTP 500
        // l'agente legge il backend, esegue azioni produttive di diagnosi (curl, psql),
        // poi RILEGGE lo stesso file per verificare. Due read_file identiche intervallate
        // da azioni produttive NON sono uno stallo: l'agente sta CONVERGENDO.
        let msgs = vec![
            ai_tool_input("read_file", json!({"path": "backend/index.js"})),
            human_tool_result(Some(0), false, "..."),
            ai_tool_input(
                "run_command",
                json!({"command": "curl -s localhost:3000/users"}),
            ),
            human_tool_result(Some(0), false, "500 Internal Server Error"),
            ai_tool_input("run_command", json!({"command": "psql -c 'select 1'"})),
            human_tool_result(Some(0), false, "1 row"),
            ai_tool_input("read_file", json!({"path": "backend/index.js"})),
            human_tool_result(Some(0), false, "..."),
        ];
        let count = detect_repeated_action_detailed(&msgs, 24)
            .map(|h| h.count)
            .unwrap_or(0);
        assert!(
            count < 2,
            "rilettura dopo azioni di diagnosi e' debugging, non stallo; count={count}"
        );
    }

    #[test]
    fn repeated_action_run_command_falliti_resta_stallo() {
        // CASO 4 (anti-regressione tool produttivi): due run_command IDENTICI falliti
        // restano stallo (count 2). I tool produttivi NON sono toccati dall'esclusione
        // rilettura-dopo-progresso: contano sempre.
        let msgs = vec![
            ai_tool_input("run_command", json!({"command": "npm run build"})),
            human_tool_result(None, true, "build failed"),
            ai_tool_input("run_command", json!({"command": "npm run build"})),
            human_tool_result(None, true, "build failed"),
        ];
        let hit = detect_repeated_action_detailed(&msgs, 24)
            .expect("due run_command identici falliti restano stallo");
        assert_eq!(hit.count, 2);
        assert_eq!(hit.tool_name, "run_command");
        assert!(hit.failed, "l'ultima occorrenza e' fallita");
    }

    #[test]
    fn repeated_action_grep_identico_e_stallo() {
        // grep stesso pattern ripetuto -> stallo (bersaglio = pattern).
        let msgs = vec![
            ai_tool_input("grep", json!({"pattern": "TODO", "path": "src"})),
            human_tool_result(Some(0), false, "match..."),
            ai_tool_input("grep", json!({"pattern": "TODO", "path": "src"})),
            human_tool_result(Some(0), false, "match..."),
        ];
        let hit = detect_repeated_action_detailed(&msgs, 24).expect("stallo grep atteso");
        assert_eq!(hit.tool_name, "grep");
        assert_eq!(hit.count, 2);
    }

    #[test]
    fn repeated_action_read_file_porzioni_diverse_non_stallo() {
        // Causa radice del falso-stallo "crea utente" (regola H): leggere PORZIONI
        // diverse dello stesso file (zoom progressivo limit:50 -> 30-330 -> 314-320)
        // e' esplorazione LEGITTIMA, non un loop. Con la signature sensibile al range
        // le tre letture sono azioni DISTINTE (count 1 ciascuna), sotto la soglia 2.
        let progressivo = vec![
            ai_tool_input("read_file", json!({"path": "src/big.ts", "limit": 50})),
            human_tool_result(Some(0), false, "..."),
            ai_tool_input(
                "read_file",
                json!({"path": "src/big.ts", "start_line": 30, "end_line": 330}),
            ),
            human_tool_result(Some(0), false, "..."),
            ai_tool_input(
                "read_file",
                json!({"path": "src/big.ts", "start_line": 314, "end_line": 320}),
            ),
            human_tool_result(Some(0), false, "..."),
        ];
        assert_eq!(
            detect_repeated_action_detailed(&progressivo, 24).map(|h| h.count),
            Some(1),
            "porzioni diverse dello stesso file -> nessuno stallo"
        );
        // Controprova: la STESSA porzione (range identico) ripetuta resta stallo reale.
        let identico = vec![
            ai_tool_input(
                "read_file",
                json!({"path": "src/big.ts", "start_line": 314, "end_line": 320}),
            ),
            human_tool_result(Some(0), false, "..."),
            ai_tool_input(
                "read_file",
                json!({"path": "src/big.ts", "start_line": 314, "end_line": 320}),
            ),
            human_tool_result(Some(0), false, "..."),
        ];
        let hit = detect_repeated_action_detailed(&identico, 24)
            .expect("stallo atteso su range identico");
        assert_eq!(hit.count, 2);
        assert_eq!(hit.tool_name, "read_file");
    }

    #[test]
    fn repeated_action_identita_universale_per_ogni_tool() {
        // CONTROLLO UNIVERSALE (regola L): per OGNI tool tracciato, due chiamate che
        // differiscono anche in UN SOLO argomento sono azioni DISTINTE (no falso
        // loop), mentre due chiamate IDENTICHE sono un loop. L'identita' deriva
        // dall'input COMPLETO (build_signature), quindi la proprieta' vale per
        // qualunque tool e argomento SENZA whitelist da mantenere. Questo test e' il
        // guard contro la reintroduzione di firme parziali (causa storica dei falsi
        // loop su read_file/range ed edit_file/old_string). Esito fallito ovunque
        // per neutralizzare l'esclusione "falso-doppione" dei tool produttivi.
        let cases: &[(&str, Value, Value)] = &[
            (
                "read_file",
                json!({"path": "a.ts"}),
                json!({"path": "a.ts", "start_line": 50}),
            ),
            (
                "read_file_lines",
                json!({"path": "a.ts", "start_line": 1, "end_line": 50}),
                json!({"path": "a.ts", "start_line": 51, "end_line": 100}),
            ),
            (
                "list_files",
                json!({"dir": "src"}),
                json!({"dir": "src/app"}),
            ),
            (
                "grep",
                json!({"pattern": "TODO", "path": "src"}),
                json!({"pattern": "TODO", "path": "lib"}),
            ),
            (
                "search_in_files",
                json!({"query": "auth", "path": "a"}),
                json!({"query": "auth", "path": "b"}),
            ),
            (
                "edit_file",
                json!({"path": "a.ts", "old_string": "x"}),
                json!({"path": "a.ts", "old_string": "y"}),
            ),
            (
                "write_file",
                json!({"path": "a.ts", "content": "x"}),
                json!({"path": "a.ts", "content": "y"}),
            ),
            (
                "run_command",
                json!({"command": "ls"}),
                json!({"command": "pwd"}),
            ),
        ];
        for (tool, base, variato) in cases {
            // Un argomento diverso -> azioni distinte -> nessuna signature a soglia 2.
            let diversi = vec![
                ai_tool_input(tool, base.clone()),
                human_tool_result(None, true, "..."),
                ai_tool_input(tool, variato.clone()),
                human_tool_result(None, true, "..."),
            ];
            let count_diversi = detect_repeated_action_detailed(&diversi, 24)
                .map(|h| h.count)
                .unwrap_or(0);
            assert!(
                count_diversi < 2,
                "{tool}: input diverso NON deve contare come loop (count {count_diversi})"
            );
            // Input IDENTICO ripetuto -> loop reale (count 2).
            let identici = vec![
                ai_tool_input(tool, base.clone()),
                human_tool_result(None, true, "..."),
                ai_tool_input(tool, base.clone()),
                human_tool_result(None, true, "..."),
            ];
            let hit = detect_repeated_action_detailed(&identici, 24)
                .unwrap_or_else(|| panic!("{tool}: input identico DEVE essere loop"));
            assert_eq!(hit.count, 2, "{tool}: input identico -> count 2");
        }
    }

    #[test]
    fn fs_mutation_da_config() {
        let cfg = RoutingConfig::default();
        assert!(has_filesystem_mutation_in_history(
            &[ai_with_tool("rename_file")],
            &cfg
        ));
        // read_file non e' un mutator.
        assert!(!has_filesystem_mutation_in_history(
            &[ai_with_tool("read_file")],
            &cfg
        ));
    }

    #[test]
    fn unfulfilled_signal_with_declared_outcome_precede_su_tutto() {
        // ADR 0034: qualunque dichiarazione decide, anche quando i segnali
        // strutturali "griderebbero" unfulfilled (mai riscritta dall'euristica).
        assert!(!unfulfilled_signal_with(true, true, true, true, 1, 2));
    }

    #[test]
    fn unfulfilled_signal_with_ha_smesso_senza_fare_nulla() {
        // Nessuna dichiarazione, tool disponibili, nessuna tool call in questo
        // turno, turno action-oriented, entro la finestra -> unfulfilled.
        assert!(unfulfilled_signal_with(false, true, true, true, 1, 2));
    }

    #[test]
    fn unfulfilled_signal_with_non_action_oriented_mai_unfulfilled() {
        assert!(!unfulfilled_signal_with(false, true, true, false, 1, 2));
    }

    #[test]
    fn unfulfilled_signal_with_oltre_la_finestra_non_unfulfilled() {
        assert!(!unfulfilled_signal_with(false, true, true, true, 5, 2));
    }

    #[test]
    fn unfulfilled_signal_di_stato_ignora_una_parola_pending_nel_testo() {
        // MUTAZIONE del vecchio lessicale (_PENDING_STEPS_LABELS, rimosso): prima
        // "prossimi passi necessari:\n1...\n2..." nel testo del modello bastava DA
        // SOLO a riaprire il turno. Ora il testo del `result` non e' nemmeno
        // consultato: senza tool disponibili (tools_json vuoto) il segnale
        // strutturale e' strutturalmente falso, a prescindere dalla prosa.
        let cfg = RoutingConfig::default();
        let state = AgentState {
            result: Some(
                "Stato attuale: ok.\nProssimi passi necessari:\n1. Verificare X\n2. Eseguire Y"
                    .into(),
            ),
            action_oriented: Some(true),
            iterations: Some(1),
            ..Default::default()
        };
        assert!(!unfulfilled_signal(&state, &cfg));
    }

    #[test]
    fn unfulfilled_signal_di_stato_declared_outcome_precede_lo_strutturale() {
        let cfg = RoutingConfig::default();
        let state = AgentState {
            declared_outcome: Some(json!({"outcome": "partial", "summary": "fatto in parte"})),
            tools_json: Some(vec![json!({"name": "read_file"})]),
            pending_tool_uses: Some(Vec::new()),
            action_oriented: Some(true),
            iterations: Some(1),
            ..Default::default()
        };
        assert!(!unfulfilled_signal(&state, &cfg));
    }

    #[test]
    fn unfulfilled_signal_di_stato_strutturale_senza_dichiarazione() {
        let cfg = RoutingConfig::default();
        let state = AgentState {
            declared_outcome: None,
            tools_json: Some(vec![json!({"name": "read_file"})]),
            pending_tool_uses: Some(Vec::new()),
            action_oriented: Some(true),
            iterations: Some(1),
            ..Default::default()
        };
        assert!(unfulfilled_signal(&state, &cfg));
    }

    #[test]
    fn software_task_da_intent() {
        let cfg = RoutingConfig::default();
        let mut state = AgentState {
            user_intent: Some("debug".into()),
            ..Default::default()
        };
        assert!(is_software_task(&state, &cfg));
        state.user_intent = Some("architecture".into());
        assert!(!is_software_task(&state, &cfg));
    }

    #[test]
    fn software_task_da_intent_extra() {
        let cfg = RoutingConfig::default();
        let mut state = AgentState::default();
        state.extra.insert("intent".into(), json!("frontend"));
        assert!(is_software_task(&state, &cfg));
    }

    #[test]
    fn software_task_da_delega_subagente() {
        // REGRESSIONE run 1daf83b3: il padre delega tutto a un subagente (zero
        // mutazioni proprie, intent fuori whitelist) -> il final_gate veniva
        // SALTATO e la dichiarazione allucinata del figlio chiudeva 'completed'
        // senza verifica. La delega e' lavoro software per procura: gate attivo.
        let cfg = RoutingConfig::default();
        let mut state = AgentState {
            user_intent: Some("architecture".into()), // fuori whitelist
            ..Default::default()
        };
        state.messages = vec![Message::Ai {
            content: MessageContent::text(""),
            tool_calls: vec![ToolUse {
                id: "c1".to_string(),
                name: "dispatch_subagent".to_string(),
                input: json!({"task": "correggi gli errori"}),
                thought_signature: None,
            }],
            reasoning: None,
            thinking_signature: None,
        }];
        assert!(is_software_task(&state, &cfg));
        // Anche la variante plurale (fan-out) attiva il gate.
        if let Some(Message::Ai { tool_calls, .. }) = state.messages.first_mut() {
            tool_calls[0].name = "dispatch_subagents".to_string();
        }
        assert!(is_software_task(&state, &cfg));
    }

    #[test]
    fn final_gate_eligible_esclude_plan_phase() {
        let cfg = RoutingConfig::default();
        let state = AgentState {
            user_intent: Some("code".into()),
            plan_phase_active: Some(true),
            ..Default::default()
        };
        assert!(!final_gate_eligible(&state, &cfg));
    }

    #[test]
    fn todo_isolation_richiede_tutte() {
        let cfg_off = RoutingConfig::default();
        let mut state = AgentState {
            plan_phase_active: Some(true),
            automation_mode: Some(AutomationMode::Automatic),
            ..Default::default()
        };
        // Setting OFF -> false.
        assert!(!todo_isolation_active(&state, &cfg_off));
        let cfg_on = RoutingConfig {
            todo_isolation_enabled: true,
            ..RoutingConfig::default()
        };
        assert!(todo_isolation_active(&state, &cfg_on));
        // Modalita' non autonoma -> false.
        state.automation_mode = Some(AutomationMode::Confirm);
        assert!(!todo_isolation_active(&state, &cfg_on));
    }
}

/// Golden di parita' 1:1 vs Python per i detector strutturali. Carica
/// `/tmp/golden_executor_detectors.json` (vedi `gen_golden_executor_detectors.py`).
#[cfg(test)]
mod golden {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;

    /// Forma INTERMEDIA di un messaggio (replica i raw spec dello script Python).
    #[derive(Debug, Deserialize)]
    #[serde(tag = "kind")]
    enum RawMsg {
        #[serde(rename = "ai_tool")]
        AiTool {
            name: String,
            #[serde(default)]
            input: Value,
        },
        #[serde(rename = "ai_text")]
        AiText {
            #[serde(default)]
            text: String,
        },
        #[serde(rename = "tool")]
        Tool {
            #[serde(default)]
            text: String,
        },
        #[serde(rename = "human_result")]
        HumanResult {
            #[serde(default)]
            exit_code: Option<i64>,
            #[serde(default)]
            is_error: bool,
            #[serde(default)]
            text: String,
        },
    }

    impl RawMsg {
        fn to_message(&self) -> Message {
            match self {
                RawMsg::AiTool { name, input } => Message::Ai {
                    content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                        id: "golden".into(),
                        name: name.clone(),
                        input: if input.is_null() {
                            json!({})
                        } else {
                            input.clone()
                        },
                        thought_signature: None,
                    }]),
                    tool_calls: vec![],
                    reasoning: None,
                    thinking_signature: None,
                },
                RawMsg::AiText { text } => Message::Ai {
                    content: MessageContent::text(text.clone()),
                    tool_calls: vec![],
                    reasoning: None,
                    thinking_signature: None,
                },
                RawMsg::Tool { text } => Message::Tool {
                    tool_call_id: "golden".into(),
                    content: MessageContent::text(text.clone()),
                },
                RawMsg::HumanResult {
                    exit_code,
                    is_error,
                    text,
                } => Message::Human {
                    content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                        tool_use_id: "golden".into(),
                        content: Value::String(text.clone()),
                        is_error: *is_error,
                        exit_code: *exit_code,
                    }]),
                },
            }
        }
    }

    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        group: String,
        case_id: String,
        messages: Vec<RawMsg>,
        output: Value,
    }

    /// Mappa `Option<bool>` Python (True/False/None) al `Value` JSON corrispondente.
    fn opt_bool(v: Option<bool>) -> Value {
        match v {
            Some(b) => Value::Bool(b),
            None => Value::Null,
        }
    }

    #[test]
    #[ignore = "richiede /tmp/golden_executor_detectors.json generato da gen_golden_executor_detectors.py"]
    fn golden_executor_detectors() {
        let Some(raw) = crate::golden_util::load_golden(
            "golden_executor_detectors.json",
            "gen_golden_executor_detectors.py",
        ) else {
            return;
        };
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        assert!(
            cases.len() >= 20,
            "attesi >= 20 casi, trovati {}",
            cases.len()
        );

        let cfg = RoutingConfig::default();
        let mut checked = 0usize;
        for c in &cases {
            let msgs: Vec<Message> = c.messages.iter().map(RawMsg::to_message).collect();
            let got: Value = match c.group.as_str() {
                "has_filesystem_mutation" => {
                    Value::Bool(has_filesystem_mutation_in_history(&msgs, &cfg))
                }
                "has_tool_calls_in_history" => Value::Bool(has_tool_calls_in_history(&msgs)),
                "tool_result_outcome_after" => opt_bool(tool_result_outcome_after(&msgs, 0, 3)),
                "detect_repeated_failed_command" => {
                    let (cmd, count) = detect_repeated_failed_command(&msgs, 12);
                    json!({ "command": cmd, "count": count })
                }
                "detect_repeated_action" => {
                    let (label, count) = detect_repeated_action(&msgs, 24);
                    json!({ "label": label, "count": count })
                }
                "count_recent_request_port" => Value::from(count_recent_request_port(&msgs, 16)),
                "has_active_resources_in_history" => {
                    Value::Bool(has_active_resources_in_history(&msgs, 24))
                }
                "detect_recent_tool_error" => Value::Bool(detect_recent_tool_error(&msgs, 4)),
                other => panic!("gruppo golden sconosciuto: {other} (caso {})", c.case_id),
            };
            assert_eq!(
                got, c.output,
                "PARITA' FALLITA {} / {}:\n  rust   = {}\n  python = {}",
                c.group, c.case_id, got, c.output
            );
            checked += 1;
        }
        println!("golden executor_detectors: {checked} casi verificati, tutti verdi");
    }
}
