//! Dispatch centrale dei tool agente: mappa nome-tool -> handler.
//!
//! Estratto da mod.rs (refactor god-file). Nessun cambiamento di routing:
//! stessi nomi tool mappati agli stessi handler.

use serde_json::Value;

use super::profile_tools::{tool_create_profile, tool_update_profile};
use super::quality_tools::{tool_batch_analyze_code, tool_scan_code_quality};
use super::semantic_tools::{
    tool_recall_context, tool_search_codebase_semantic, tool_search_file_semantic,
};
use super::{
    archive_tools, attachment_inspector, attachments, audio_tools, command, dev_diagnostics,
    dispatcher, document_tools, figma_tools, files, git, image_tools, knowledge, ports,
    project_db_query, rag_search, sandbox, scaffold_verifier, service, shadcn_setup,
    subagent_native, testing, todos, tool_not_found, ui_patterns, ui_reference_search, ui_styling,
    verify,
    video_tools, vision_tools, visual_compare, AgentToolContext,
};

/// Esegue un tool per conto dell'agente.
/// Ritorna sempre una stringa: il risultato in caso di successo, o un messaggio d'errore.
/// Esegue un tool agente e ne restituisce la risposta con l'esito in un CAMPO
/// (regola Q): il testo e' testo, `esito` ed `exit_code` sono dati.
///
/// I tool non ancora migrati ritornano `String` e passano dal ponte
/// [`RispostaTool::da_testo_legacy`], che ricostruisce l'esito dal marker: e'
/// l'UNICO punto autorizzato a farlo, ed e' debito dichiarato. Un tool migrato
/// costruisce la propria `RispostaTool` e non passa di li'.
pub async fn execute_agent_tool(
    ctx: &AgentToolContext,
    name: &str,
    input: &Value,
) -> nexus_types::tool_outcome::RispostaTool {
    if let Some(risposta) = esegui_tool_migrato(ctx, name, input).await {
        return risposta;
    }
    esegui_tool_legacy(ctx, name, input).await
}

/// I tool MIGRATI alla regola Q: costruiscono la propria [`RispostaTool`] e non
/// passano dal ponte, perche' non c'e' nulla da ricostruire — l'esito e' gia' un
/// campo. `None` = il nome non e' fra questi, decide il dispatch legacy.
///
/// E' un elenco che CRESCE di un nome a ogni tool migrato: quando copre tutto,
/// spariscono insieme il dispatch legacy, il ponte e il marker.
async fn esegui_tool_migrato(
    ctx: &AgentToolContext,
    name: &str,
    input: &Value,
) -> Option<nexus_types::tool_outcome::RispostaTool> {
    // Spezzato per FAMIGLIA e non per comodita' di lettura: questo match cresce
    // di un braccio a ogni tool migrato, quindi senza una divisione la funzione
    // supera le soglie del gate qualita' per costruzione — e ricompattarla a
    // ogni lotto sarebbe rimandare la stessa domanda.
    if let Some(risposta) = migrati_esecuzione(ctx, name, input).await {
        return Some(risposta);
    }
    migrati_contenuti(ctx, name, input).await
}

/// I tool migrati che ESEGUONO qualcosa: comandi, servizi, suite di test, git.
/// Cio' che li accomuna e' il campo `exit_code` e la distinzione fra «il tool ha
/// fatto il suo lavoro» e «il comando e' andato bene».
async fn migrati_esecuzione(
    ctx: &AgentToolContext,
    name: &str,
    input: &Value,
) -> Option<nexus_types::tool_outcome::RispostaTool> {
    match name {
        // Lo stato d'uscita di un comando e' il dato su cui decidono il
        // final_gate e la catena di verifica: viaggia nel campo `exit_code`,
        // non nel testo "EXIT CODE: N" (che resta per il modello).
        "run_command" => Some(command::tool_run_command(ctx, input).await),
        // L'elenco servizi non ha un esito da annunciare nel testo: la resa la
        // compone `service_listing` dai campi, e il fallimento (DB illeggibile)
        // vive in `esito`. Un marker in testa alla prosa qui sarebbe tornato a
        // essere un campo travestito.
        "list_active_services" => Some(service::tool_list_active_services(ctx, input).await),
        // Il resto di `service.rs`. Il lancio di un servizio e' il caso in cui
        // l'esito nel campo cambia di piu': i due rami di quota ritornavano un
        // messaggio senza marker, cioe' un avvio BLOCCATO che l'agente riceveva
        // come conferma.
        //
        // `run_in_terminal` e' un alias storico che il catalogo non dichiara —
        // il modello non lo puo' invocare — ma che il dispatch accetta e altre
        // parti del sistema nominano (step_gate, cache dei mutatori, UI). Resta,
        // e il contratto d'ingresso lo legge col NOME INVOCATO, cosi' un errore
        // di parametri non rimanda allo schema di un tool diverso.
        "run_in_terminal" => Some(service::tool_run_service(ctx, input, "task").await),
        "run_service" => Some(service::tool_run_service(ctx, input, "service").await),
        "read_terminal_output" => Some(service::tool_read_service_output(ctx, input).await),
        // DIVERGENZA CHIUSA: l'handler ripiegava sull'ultimo processo quando
        // `process_id` mancava, mentre il catalogo lo dichiara obbligatorio. La
        // capacita' resta dove e' PROMESSA, in `tail_service_logs`.
        "read_service_output" => Some(service::tool_read_service_output(ctx, input).await),
        "stop_service" => Some(service::tool_stop_service(ctx, input).await),
        "service_restart" => Some(service::tool_service_restart(ctx, input).await),
        "tail_service_logs" => Some(service::tool_tail_service_logs(ctx, input).await),
        "build_project_image" => Some(service::tool_build_project_image(ctx).await),
        // `testing.rs` e i due tool di comando che condividono la sua catena.
        // Qui l'esito nel campo recupera un dato che era MUTO: lo stato d'uscita
        // finiva nel testo come "Exit code: N" e "(exit code: N)", mentre il
        // ponte legacy cerca "EXIT CODE: " MAIUSCOLO. Le due scritture non si
        // sono mai incontrate, quindi per questi quattro tool il campo su cui il
        // final_gate decide se rieseguire un criterio o correggere il codice e'
        // stato `None` sempre.
        "run_playwright_tests" => Some(testing::tool_run_playwright_tests(ctx, input).await),
        "run_specific_test" => Some(testing::tool_run_specific_test(ctx, input).await),
        "run_lint_fix" => Some(testing::tool_run_lint_fix(ctx, input).await),
        "format_file" => Some(testing::tool_format_file(ctx, input).await),
        "run_tests" => Some(command::tool_run_tests(ctx, input).await),
        // Catena di verifica post-modifica (ADR 0019 L3): gli step del profilo
        // per-ambiente, fail-fast al primo rosso. Sta fra chi ESEGUE perche'
        // lancia N processi, ma non ha un `exit_code` sull'aggregato: uno solo
        // per N comandi andrebbe inventato, e gli stati d'uscita veri stanno
        // per step nel report. RAMO NUDO CHIUSO: `scope: quick` su un profilo
        // in cui nessuno step e' marcato per il gate percorreva una catena
        // VUOTA e usciva `passed: true` con `steps: []` — un verde che nessuno
        // aveva misurato, consegnato come prova che la modifica compila.
        "nexus_verify_change" => Some(verify::tool_nexus_verify_change(ctx, input).await),
        // FASE 2 "resa Figma Make": screenshot dell'app viva contro il design.
        // Sta qui per la stessa ragione di `nexus_verify_change` — lancia un
        // processo (node che pilota Chromium) — e come quello non ha un
        // `exit_code` da riportare: lo stato d'uscita dello script e' un
        // dettaglio interno, non l'esito del confronto. RAMO NUDO CHIUSO: una
        // risposta vision FUORI dal formato imposto usciva come successo con
        // `similarity_score: null`, cioe' il confronto — l'intero compito della
        // chiamata — non era stato fatto e il modello leggeva "riuscito". E i
        // tre modi di non ottenere lo screenshot (ambiente senza Chromium, url
        // che non risponde, pagina troppo lenta) condividevano un unico
        // suggerimento, che su due casi su tre mandava dalla parte sbagliata.
        "nexus_visual_compare" => Some(visual_compare::tool_nexus_visual_compare(ctx, input).await),
        // `git.rs`: CINQUE handler su sei componevano `[git <verbo> error: ...]`
        // senza marker, cioe' un commit rifiutato, un push respinto e un pull in
        // conflitto arrivavano all'agente come esecuzioni riuscite. Anche il
        // rifiuto "non e' un repository git" usciva nudo, da tutti e sei.
        "git_status" => Some(git::tool_git_status(ctx).await),
        "git_stage" => Some(git::tool_git_stage(ctx, input).await),
        "git_commit" => Some(git::tool_git_commit(ctx, input).await),
        "git_push" => Some(git::tool_git_push(ctx).await),
        "git_pull" => Some(git::tool_git_pull(ctx).await),
        "git_remote_add" => Some(git::tool_git_remote_add(ctx, input).await),
        // `document_tools.rs`: i tre estrattori avevano la stessa sequenza
        // ripetuta e con essa gli stessi quattro rami d'errore, tutti degradati
        // a un'unica natura implicita. Ora l'allegato inesistente (id sbagliato,
        // rimediabile) e il file che il DB dichiara ma lo storage non consegna
        // (del sistema) sono due cose distinte, e il messaggio del primo nomina
        // il tool che restituisce gli id validi.
        "nexus_extract_pdf_text" => Some(document_tools::tool_nexus_extract_pdf_text(ctx, input).await),
        "nexus_extract_docx_text" => Some(document_tools::tool_nexus_extract_docx_text(ctx, input).await),
        "nexus_extract_xlsx_data" => Some(document_tools::tool_nexus_extract_xlsx_data(ctx, input).await),
        // L'elenco delle entry di un archivio: stessi due helper degli
        // estrattori di documenti, ora condivisi (`documento_da_allegato`,
        // `uuid_allegato`, accanto a `load_attachment`).
        "nexus_list_archive_entries" => Some(archive_tools::tool_nexus_list_archive_entries(ctx, input).await),
        // I tre tool di lente UI. `ui_styling_audit` e' quello dove l'esito nel
        // campo cambia una lettura: `VocabolarioAssente` non e' un verdetto
        // sullo stile del progetto, e' l'assenza dello strumento con cui lo si
        // giudica — e usciva come un successo il cui corpo portava un campo
        // `error`.
        "ui_layout_patterns" => Some(ui_patterns::tool_ui_layout_patterns(&ctx.core.db, input).await),
        "ui_reference_search" => Some(ui_reference_search::tool_ui_reference_search(&ctx.core, input).await),
        "ui_styling_audit" => Some(ui_styling::tool_ui_styling_audit(&ctx.core, input).await),
        // I due generatori di media. Sono GEMELLI, e metterli accanto ha fatto
        // vedere la divergenza: sul caso "il provider ha restituito solo una
        // URL" `generate_image` dichiarava il fallimento e `generate_video`
        // ritornava un JSON con `note` e nessun marker — cioe' un successo, per
        // un tool il cui compito era salvare il file nel progetto e che non
        // l'aveva salvato. `size` inoltre e' un ENUM nel catalogo che l'handler
        // leggeva come stringa libera: i valori non promessi passavano di qui e
        // fallivano un salto piu' in la', dal provider.
        "nexus_generate_image" => Some(image_tools::tool_nexus_generate_image(&ctx.core, input).await),
        "nexus_generate_video" => Some(video_tools::tool_nexus_generate_video(&ctx.core, input).await),
        // I tre handler AUTONOMI di `knowledge.rs` (gli altri sei dipendono da
        // helper che inghiottono gli errori del DB con `.unwrap_or_default()`, e
        // vanno portati a `Result` prima di poter dichiarare un esito onesto).
        // Due promesse del catalogo restano tali e sono annotate nel codice:
        // `knowledge_set_relevance` dichiara `relevance_score` e non lo legge,
        // `knowledge_get_note` annuncia di aggiornare `access_count` e fa una
        // sola SELECT — quella colonna non esiste in nessuna migrazione.
        "code_doc" => Some(knowledge::tool_code_doc(&ctx.core, input).await),
        "knowledge_get_note" => Some(knowledge::tool_knowledge_get_note(&ctx.core, input).await),
        "knowledge_set_relevance" => Some(knowledge::tool_knowledge_set_relevance(&ctx.core, input).await),
        _ => None,
    }
}

/// I tool migrati che LEGGONO o TRASFORMANO contenuti: file, documenti,
/// archivi, media, lenti UI, note della KB. Nessuno di loro esegue processi,
/// quindi nessuno ha un `exit_code` da riportare.
async fn migrati_contenuti(
    ctx: &AgentToolContext,
    name: &str,
    input: &Value,
) -> Option<nexus_types::tool_outcome::RispostaTool> {
    match name {
        // Ogni suo fallimento e' RIMEDIABILE e lo dichiara nel campo `natura`:
        // e' il primo tool a farlo, ed e' quello su cui la mancanza si
        // misurava (11% di `old_string non trovato` seguiti da una ripetizione
        // identica, 07/08/2026).
        "edit_file" => Some(files::tool_edit_file(ctx, input).await),
        // Migrato insieme al contratto d'ingresso: un percorso sbagliato o un
        // file troppo grande sono entrambi rimediabili dall'agente, e il
        // messaggio dice come.
        "read_file" => Some(files::tool_read_file(ctx, input).await),
        "write_file" => Some(files::tool_write_file(ctx, input).await),
        // Gli estremi dell'intervallo arrivano dal contratto: gli alias
        // `offset`/`limit`, che il catalogo non ha mai promesso e che il prompt
        // del supervisore dichiara inesistenti dalla mig 0060, non sono piu'
        // accettati in silenzio.
        "read_file_lines" => Some(files::tool_read_file_lines(ctx, input).await),
        // I due tool di sola LETTURA della struttura: una directory vuota e' un
        // successo, una assente e' un fallimento, e ora la differenza sta nel
        // campo invece che in un marker anteposto al testo.
        "list_files" => Some(files::tool_list_files(ctx, input).await),
        "search_in_files" => Some(files::tool_search_in_files(ctx, input).await),
        // I tre tool che MUTANO il filesystem senza scrivere contenuto. La
        // natura del loro fallimento non e' una scelta caso per caso: viene da
        // `NaturaFallimento::da_errore_io`, che la legge dal `ErrorKind`
        // (regola M) invece che dal messaggio del sistema operativo — che e'
        // localizzato e diverso fra Windows e Linux.
        "delete_file" => Some(files::tool_delete_file(ctx, input).await),
        "rename_file" => Some(files::tool_rename_file(ctx, input).await),
        "fs_mkdir" => Some(files::tool_fs_mkdir(ctx, input).await),
        // Copia e spostamento chiudono `files.rs`: la loro natura viene dal
        // `ErrorKind` come per gli altri tre, tranne dove il messaggio nomina il
        // parametro che rimedia (`overwrite:true`) — li' e' rimediabile per
        // costruzione, ed e' il tool a saperlo.
        "fs_copy" => Some(files::tool_fs_copy(ctx, input).await),
        "fs_move" => Some(files::tool_fs_move(ctx, input).await),
        // I due estrattori Figma. Il secondo aveva un ramo NUDO che si vede solo
        // mettendo insieme due campi del suo stesso manifest: quando l'estrattore
        // non sa leggere NESSUNA delle scritture file presenti nel .make, il
        // code-snapshot dell'app e' perso per intero e il tool rispondeva
        // ugualmente "questo .make non contiene un code-snapshot ricostruibile" —
        // cioe' un successo. Ora quel caso e' DEL SISTEMA (il formato e' ignoto,
        // non c'e' parametro da correggere) e resta distinto dal .make che
        // davvero non contiene codice, che e' un successo con zero file.
        "nexus_extract_figma_structure" => {
            Some(figma_tools::tool_nexus_extract_figma_structure(ctx, input).await)
        }
        "nexus_extract_figma_code" => {
            Some(figma_tools::tool_nexus_extract_figma_code(ctx, input).await)
        }
        // La descrizione di un'immagine allegata. Il ramo NUDO era la risposta
        // VUOTA del modello vision: usciva come successo con `description: ""`,
        // cioe' l'agente leggeva «questa immagine non contiene nulla» dove il
        // modello non aveva prodotto nulla — su un mockup o uno screenshot e' una
        // conclusione falsa su cui prosegue, e il gateway non la puo' distinguere
        // (HTTP 200 come per una risposta piena). Gli altri cinque esiti erano
        // gia' dichiarati ma tutti con la stessa natura implicita: ora l'id
        // sbagliato e il kind non-immagine sono rimediabili e nominano il tool con
        // cui rimediare, il limite di `settings` e lo storage muto sono del
        // sistema, e il gateway esaurito e' transitorio.
        "nexus_describe_image_attachment" => {
            Some(vision_tools::tool_nexus_describe_image_attachment(ctx, input).await)
        }
        // I due tool audio. Nessun ramo NUDO da chiudere — ogni fallimento era
        // gia' dichiarato — ma due messaggi mentivano, e il campo `natura` li ha
        // resi visibili: una dimensione NEGATIVA in DB cadeva nello stesso ramo
        // del limite e usciva come «audio troppo grande», invitando ad alzare un
        // limite che nessuna dimensione negativa rispetta; e il limite stesso,
        // quando `settings` non era leggibile o portava un valore non numerico,
        // veniva ripiegato in silenzio sul default e poi CITATO nel messaggio
        // come se fosse configurato. Ora l'ignoto e' dichiarato invece di
        // degradare a un numero (regola Q), e «non e' un audio» nomina
        // `nexus_inspect_attachment` invece di rimandare a un tool senza nome.
        "nexus_transcribe_audio" => Some(audio_tools::tool_nexus_transcribe_audio(ctx, input).await),
        "nexus_text_to_speech" => Some(audio_tools::tool_nexus_text_to_speech(ctx, input).await),
        // I due tool sui profili utente: non eseguono processi, scrivono una
        // riga di `user_profiles`. I loro due esiti piu' probabili — nome gia'
        // preso, profilo inesistente — uscivano NUDI, cioe' come successi, e ora
        // dichiarano il fallimento nel campo insieme al tool che lo rimedia.
        "create_profile" => Some(tool_create_profile(ctx, input).await),
        "update_profile" => Some(tool_update_profile(ctx, input).await),
        // Legge un log e lo confronta coi pattern di `nexus_dev_diagnostics`:
        // nessun processo eseguito, quindi nessun exit_code. Il suo ramo senza
        // pattern attivi usciva NUDO — `findings: []` come un successo, cioe'
        // indistinguibile dalla diagnosi vera in cui i pattern ci sono e nessuno
        // matcha — e ora dichiara `DelSistema`, che e' quel che l'agente puo'
        // farci: niente, se non leggere il log da solo.
        "nexus_dev_server_diagnose" => {
            Some(dev_diagnostics::tool_nexus_dev_server_diagnose(ctx, input).await)
        }
        // Scrive stub TSX: tocca il filesystem ma non esegue processi. Chiudeva
        // NUDO il ramo che conta — una `fs::write` fallita finiva in un warn di
        // log e in `unsupported`, la stessa lista dei componenti che non
        // esistono, e la risposta restava un JSON senza dichiarazione d'esito:
        // su un target in sola lettura il modello leggeva `written: []` come un
        // successo. Ora la natura viene dal `ErrorKind` (regola M), e «nome
        // sconosciuto» resta distinto da «non sono riuscito a scrivere».
        "nexus_install_shadcn_components" => {
            Some(shadcn_setup::tool_nexus_install_shadcn_components(ctx, input).await)
        }
        // ── Dispatcher centrale (pilotaggio pannelli) ──────────────────────
        // Emettono eventi e scrivono un flag: nessun processo, quindi nessun
        // exit_code da riportare. Non avevano rami nudi; cio' che la migrazione
        // chiude sta all'INGRESSO — `severity` obbligatoria per catalogo e
        // ripiegata su "info" dall'handler, `payload` promesso oggetto e
        // accettato come qualunque JSON, le due durate lette con `as_u64` che
        // su un negativo faceva sparire il parametro invece di rifiutarlo.
        "dispatcher_emit_event" => Some(dispatcher::tool_dispatcher_emit_event(ctx, input).await),
        "dispatcher_post_notification" => {
            Some(dispatcher::tool_dispatcher_post_notification(ctx, input).await)
        }
        "dispatcher_set_flag" => Some(dispatcher::tool_dispatcher_set_flag(ctx, input).await),
        "dispatcher_update_monitor" => {
            Some(dispatcher::tool_dispatcher_update_monitor(ctx, input).await)
        }
        "dispatcher_highlight_panel" => {
            Some(dispatcher::tool_dispatcher_highlight_panel(ctx, input).await)
        }
        // `quality_tools.rs`. Il ramo che cambia di piu' e' la lettura dei
        // findings di progetto: `match rows { Ok(non vuoto) => ..., _ => ... }`
        // dava lo STESSO testo a uno scan senza risultati (successo) e a una
        // query fallita (DB giu'), quindi un guasto invitava l'agente a rifare
        // una scansione dal pannello. `severity_filter` inoltre e' un ENUM che
        // l'handler leggeva come stringa libera, e la lente SQL lo ignorava.
        "scan_code_quality" => Some(tool_scan_code_quality(ctx, input).await),
        // Il `task` del batch e' obbligatorio per catalogo e l'handler ripiegava
        // su "analyze": una richiesta di documentazione poteva tornare una
        // revisione. E un batch terminato con TUTTE le richieste in errore
        // usciva come successo, perche' il testo non era vuoto.
        "batch_analyze_code" => Some(tool_batch_analyze_code(ctx, input).await),
        // `scaffold_verifier.rs`. Scrive file (auto-apply dei fix deterministici)
        // ma non esegue processi, quindi non ha un `exit_code` da riportare.
        // RAMO NUDO CHIUSO: senza package.json ritornava un report che DICEVA
        // "blocker" nel testo e usciva come successo — una verifica mai eseguita
        // che l'agente riceveva come superata. Idem per i fix che il verifier
        // promette di applicare e non riesce a scrivere: lo scaffold resta rotto
        // e il turno proseguiva verso `npm start`.
        "nexus_verify_scaffold" => {
            Some(scaffold_verifier::tool_nexus_verify_scaffold(ctx, input).await)
        }
        // `todos.rs`: la TODO list del piano. Scrive righe, non esegue processi,
        // quindi non ha un `exit_code`. Le sue nature sono due e prima
        // arrivavano indistinte: un vocabolario violato o un `run_id` sbagliato
        // sono RIMEDIABILI e il messaggio porta i valori ammessi, mentre un DB
        // che non risponde e' DEL SISTEMA. La distinzione non era cosmetica —
        // due letture inghiottivano l'errore di query e lo raccontavano come
        // «run_id non trovato» e «piano inesistente», mandando l'agente a
        // correggere cio' che era gia' giusto.
        "nexus_todo_write" => Some(todos::tool_nexus_todo_write(ctx, input).await),
        // La magic-byte detection di un allegato: legge 32 KB e classifica,
        // nessun processo eseguito. Il ramo che la migrazione corregge non era
        // nudo ma BUGIARDO: la lookup dell'id per NOME appiattiva «nessun
        // allegato con quel nome» e «query non eseguita» nella stessa stringa, e
        // l'handler le rendeva col medesimo messaggio — «passa l'UUID oppure il
        // nome esatto del file» — cioe' davanti a un DB muto mandava l'agente a
        // correggere una chiamata gia' giusta, e a ripeterla.
        "nexus_inspect_attachment" => {
            Some(attachment_inspector::tool_nexus_inspect_attachment(ctx, input).await)
        }
        // `project_db_query.rs`: i tre tool sul DB APPLICATIVO del progetto.
        // Eseguono SQL, non processi, quindi non hanno un `exit_code`. Ogni loro
        // fallimento era gia' dichiarato da un marker, ma tutti con la stessa
        // natura implicita: ora una connessione non configurata e una query
        // fallita su `information_schema` sono DEL SISTEMA (ripeterle non cambia
        // nulla), mentre l'SQL sbagliato, il timeout e la tabella inesistente
        // sono RIMEDIABILI e nominano il tool con cui rimediare. Il timeout in
        // particolare NON e' transitorio: ritentare la stessa query pesante la fa
        // scadere di nuovo dopo altri 30 secondi. Chiuso anche un errore
        // inghiottito: una `pg_indexes` fallita usciva come "questa tabella non
        // ha indici".
        "nexus_db_query" => Some(project_db_query::tool_nexus_db_query(ctx, input).await),
        "nexus_db_tables" => Some(project_db_query::tool_nexus_db_tables(ctx, input).await),
        "nexus_db_describe" => Some(project_db_query::tool_nexus_db_describe(ctx, input).await),
        // La ricerca semantica: interroga un indice, non esegue processi. Il ramo
        // NUDO era lo zero hit con una o piu' collection FALLITE — usciva come
        // successo, e la sola traccia del guasto era un campo `hint` dentro il
        // JSON, che nessuno dei consumatori dell'esito legge. "Non ho trovato
        // niente" e "non ho potuto guardare" portano a decisioni opposte, e con
        // un hit trovato la fonte muta resta un successo: i risultati ci sono.
        // All'ingresso si chiudono i due filtri che venivano SCARTATI in
        // silenzio (`filter_session_id` non-UUID, `filter_attachment_id` vuoto)
        // e il `top_k` negativo, che diventava il default: in tutti e tre i casi
        // il modello credeva di aver ristretto la ricerca e riceveva altro.
        "nexus_search_semantic" => Some(rag_search::tool_nexus_search_semantic(ctx, input).await),
        _ => None,
    }
}

/// Il dispatch dei tool che ancora ritornano `String`. Sparisce quando l'ultimo
/// tool e' migrato; finche' esiste, e' la superficie che il ponte deve coprire.
async fn esegui_tool_legacy(
    ctx: &AgentToolContext,
    name: &str,
    input: &Value,
) -> nexus_types::tool_outcome::RispostaTool {
    let testo = match name {
        // Fix M51: tool dedicato per allocazione porta (evita curl via run_command).
        "request_port" => ports::tool_request_port(ctx, input).await,
        // Tool read-only per verifica/audit dello stato porte (bucket + allocazioni).
        "nexus_list_ports" => ports::tool_nexus_list_ports(ctx, input).await,
        // Sub-agents NATIVI (zero-Python): il sub-run gira sul grafo Rust
        // (crate::native_engine::run_native) in-process, niente piu' chiamata al
        // brain /agent/subagent-run. L'orchestrazione vive in mcp-core perche'
        // richiede native_engine (regola gerarchia crate); le guard
        // enabled/whitelist/depth/cost sono replicate DB-driven (regola G).
        "dispatch_subagent" => subagent_native::tool_dispatch_subagent(ctx, input).await,
        // Batch parallelo di sub-agent nativi (base del DAG scheduler).
        "dispatch_subagents" => subagent_native::tool_dispatch_subagents(ctx, input).await,
        // Poll (DB-only) + resume (ri-esecuzione nativa) dei sub-agent.
        "nexus_subagent_poll" => subagent_native::tool_nexus_subagent_poll(ctx, input).await,
        "nexus_subagent_resume" => subagent_native::tool_nexus_subagent_resume(ctx, input).await,
        // Tool dedicato ai cicli test-fix-test: esecuzione sincrona con
        // timeout esteso (raccomandato dai prompt al posto di run_command).
        "set_sandbox_config" => sandbox::tool_set_sandbox_config(ctx, input).await,
        "get_sandbox_config" => sandbox::tool_get_sandbox_config(ctx).await,
        "search_codebase_semantic" => {
            let query = input
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let limit = input
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(8)
                .min(20) as usize;
            tool_search_codebase_semantic(ctx, &query, limit).await
        }
        "search_file_semantic" => {
            let path = input
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let query = input
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let top_k = input
                .get("top_k")
                .and_then(Value::as_u64)
                .unwrap_or(5)
                .min(10) as usize;
            let chunk_lines = input
                .get("chunk_lines")
                .and_then(Value::as_u64)
                .unwrap_or(50)
                .clamp(10, 200) as usize;
            tool_search_file_semantic(ctx, &path, &query, top_k, chunk_lines).await
        }
        "recall_context" => {
            let query = input
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let source = input
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("all")
                .to_string();
            let limit = input
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(5)
                .min(10) as usize;
            tool_recall_context(ctx, &query, &source, limit).await
        }
        // Unico tool che guarda FUORI dal progetto: cio' che torna e' DATO, e
        // arriva gia' dichiarato come non fidato (vedi il modulo).
        // ── Knowledge Base per-progetto ────────────────────────────────────
        "knowledge_search" => knowledge::tool_knowledge_search(ctx, input).await,
        "knowledge_create_note" => knowledge::tool_knowledge_create_note(ctx, input).await,
        // Comp.0: navigazione/modifica del grafo KB (link, sottografo, pertinenza)
        "knowledge_get_links" => knowledge::tool_knowledge_get_links(ctx, input).await,
        "knowledge_get_subgraph" => knowledge::tool_knowledge_get_subgraph(ctx, input).await,
        "knowledge_create_link" => knowledge::tool_knowledge_create_link(ctx, input).await,
        // Comp.2: import di grafi esterni nella KB (JSON node-link / Mermaid / DOT)
        "knowledge_import_graph" => knowledge::tool_knowledge_import_graph(ctx, input).await,
        // ── Allegati chat (ADR 0010) ───────────────────────────────────────
        "nexus_list_attachments" => attachments::tool_nexus_list_attachments(ctx, input).await,
        "nexus_read_attachment" => attachments::tool_nexus_read_attachment(ctx, input).await,
        // ── Ingestion intelligente allegati (ADR 0011) ─────────────────────
        "nexus_read_archive_entry" => {
            archive_tools::tool_nexus_read_archive_entry(ctx, input).await
        }
        // PR6b-2: genera un'immagine dal prompt e la salva path-safe nel progetto.
        // PR6e: genera un video dal prompt (text-to-video, Veo async) e lo salva nel progetto.
        // Worklog di sessione (mig 0411): drill-down on-demand della storia di
        // lavoro — il digest compatto sta nel system, il dettaglio vive qui.
        // VINCOLO: deve restare in _ALWAYS_ON_TOOLS (profile_loader.py) cosi'
        // il modello puo' sempre approfondire oltre il digest (contratto D8).
        "nexus_get_worklog" => crate::session_worklog::tool_nexus_get_worklog(ctx, input).await,
        // ── Nexus Builtin tool (prefisso nexus_*) ──────────────────────────
        // Dispatch verso nexus_builtin::execute_with_neural per usare
        // la ricerca semantica quando neural è disponibile (Qdrant).
        // Caso speciale: nexus_mcp_tool_call con server_id="builtin" reindirizza
        // ricorsivamente a execute_agent_tool, consentendo al modello di
        // invocare via mcp_tool_call qualsiasi tool builtin (es. quelli
        // suggeriti da next_action_recommended di nexus_inspect_attachment)
        // senza doverli avere in toolspec. Sistema lazy discovery preservato.
        "nexus_mcp_tool_call" => {
            let server_id = input
                .get("server_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if server_id.eq_ignore_ascii_case("builtin")
                || server_id == "00000000-0000-0000-0000-000000000000"
            {
                let inner_tool = input
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                if inner_tool.is_empty() {
                    return nexus_types::tool_outcome::RispostaTool::da_testo_legacy(
                        serde_json::json!({
                            "error": "tool_name richiesto per nexus_mcp_tool_call con server_id=builtin"
                        })
                        .to_string(),
                    );
                }
                if inner_tool == "nexus_mcp_tool_call" {
                    return nexus_types::tool_outcome::RispostaTool::da_testo_legacy(
                        serde_json::json!({
                            "error": "ricorsione builtin -> builtin non permessa"
                        })
                        .to_string(),
                    );
                }
                let inner_args = input
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                // Ricorsione dall'INGRESSO, non dal solo mondo legacy: per questa
                // via il modello puo' invocare qualunque builtin, e un tool
                // migrato deve arrivarci col proprio esito nei campi. Ripartire
                // da `esegui_tool_legacy` lo lascerebbe fuori dal dispatch (il
                // suo braccio non e' piu' li') e lo farebbe cadere nel fallback
                // "tool non esiste".
                return Box::pin(execute_agent_tool(ctx, inner_tool, &inner_args)).await;
            }
            crate::nexus_builtin::execute_with_neural(
                &ctx.db,
                ctx.user_id,
                ctx.project_id,
                &ctx.user_role,
                &ctx.neural,
                "nexus_mcp_tool_call",
                input.clone(),
            )
            .await
        }
        other if other.starts_with("nexus_") => {
            crate::nexus_builtin::execute_with_neural(
                &ctx.db,
                ctx.user_id,
                ctx.project_id,
                &ctx.user_role,
                &ctx.neural,
                other,
                input.clone(),
            )
            .await
        }
        // Tool non cablato: delega al punto unico tool-not-found resolver
        // (regola L). Sostituisce la tabella alias hardcoded con un LOOKUP REALE
        // (builtin fuzzy + connettori installati + catalog non installato) e
        // garantisce il marker '\u{274C}' -> is_error=true (gap1). neural=Some:
        // abilita anche il match semantico Qdrant best-effort.
        other => {
            tool_not_found::resolve_tool_not_found(
                &ctx.db,
                Some(&ctx.neural),
                ctx.user_id,
                ctx.project_id,
                &ctx.user_role,
                other,
            )
            .await
        }
    };
    // Il ponte, in un punto solo: l'esito di un tool legacy si legge dal marker
    // che ha scritto nel proprio testo, perche' altro canale non ne ha.
    nexus_types::tool_outcome::RispostaTool::da_testo_legacy(testo)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Contesto minimale senza infrastruttura (punto unico in `test_support`:
    /// lo condividono i test dei tool che vogliono attraversare il dispatcher
    /// per la strada della produzione).
    fn ctx_for_dispatch_tests(root: std::path::PathBuf) -> AgentToolContext {
        crate::test_support::ctx_di_tool_test(root)
    }

    /// Regressione: `run_tests` e' esposto al modello (tool_schema, prompt
    /// test-fix-test, whitelist migrazioni 0218/0286) ma il braccio nel
    /// dispatcher era assente — ogni invocazione cadeva nel fallback
    /// "Tool non esiste". Su una root vuota e senza comando esplicito
    /// l'implementazione risponde con l'errore di auto-detection, senza
    /// toccare DB ne' sandbox: basta a provare il ricablaggio.
    #[tokio::test]
    async fn run_tests_e_dispatchato_e_non_cade_nel_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ctx_for_dispatch_tests(dir.path().to_path_buf());
        let out = execute_agent_tool(&ctx, "run_tests", &serde_json::json!({})).await;
        assert!(
            !out.testo.contains("non esiste"),
            "run_tests caduto nel fallback del dispatcher: {}",
            out.testo
        );
        assert!(
            out.testo.contains("impossibile rilevare il comando test"),
            "output inatteso da tool_run_tests: {}",
            out.testo
        );
    }

    /// La ricorsione di `nexus_mcp_tool_call` riparte dall'INGRESSO, non dal solo
    /// mondo legacy.
    ///
    /// Il braccio di un tool MIGRATO non vive piu' dentro `esegui_tool_legacy`:
    /// ripartire da li' lo lascerebbe fuori dal dispatch e lo farebbe cadere nel
    /// fallback "tool non esiste" — cioe' il modello, chiamando un builtin per
    /// questa via, riceverebbe un errore inventato invece dell'esito del tool.
    /// E' il difetto gia' accaduto due volte in questa catena, qui in una terza
    /// forma: non un braccio dimenticato, ma un braccio irraggiungibile da una
    /// sola porta d'ingresso.
    ///
    /// MUTAZIONE: rimettendo `esegui_tool_legacy` al posto di
    /// `execute_agent_tool` nella ricorsione, questo test rosseggia con
    /// "Tool 'run_command' non esiste".
    #[tokio::test]
    async fn la_ricorsione_del_mcp_tool_call_raggiunge_i_tool_migrati() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ctx_for_dispatch_tests(dir.path().to_path_buf());
        let out = execute_agent_tool(
            &ctx,
            "nexus_mcp_tool_call",
            &serde_json::json!({
                "server_id": "builtin",
                "tool_name": "run_command",
                "arguments": { "command": "" },
            }),
        )
        .await;
        assert!(
            !out.testo.contains("non esiste"),
            "un tool migrato non e' raggiungibile dalla ricorsione: {}",
            out.testo
        );
    }

    /// Gemello del precedente per `ui_styling_audit` (mig 0655), e per la stessa
    /// ragione: il prompt della figura di interfaccia e quello del revisore lo
    /// PROMETTONO, e un tool promesso senza braccio nel dispatcher torna al
    /// modello come "Tool non esiste". E' il difetto gia' accaduto due volte in
    /// questa catena — `run_tests` qui sopra, e `advisory_verdict` perso dalla
    /// whitelist della figura UI (mig 0653), che l'ha resa muta per un giorno.
    ///
    /// Il nome NON e' scritto a mano nell'assert: si legge dal catalogo esposto
    /// al modello, cosi' il test misura le due estremita' della stessa catena.
    /// Rinominarne una sola lo fa rosseggiare, che e' esattamente il caso in cui
    /// oggi nessuno se ne accorgerebbe fino al primo run.
    ///
    /// Si passa un `target_dir` inesistente di proposito: il tool rifiuta il
    /// percorso PRIMA di leggere il vocabolario, quindi la prova che il braccio
    /// c'e' costa zero. Senza questo accorgimento il test impiegava 150 secondi
    /// — cinque letture di settings su un pool lazy mai connesso, ciascuna a
    /// consumare il proprio timeout — e un test lento in una suite condivisa e'
    /// un costo che pagano tutti, a ogni `pnpm verify`.
    /// Che il vocabolario mancante produca `vocabolario_assente` e' gia' provato
    /// dal test del criterio, dove si verifica senza alcuna infrastruttura.
    #[tokio::test]
    async fn ui_styling_audit_e_dispatchato_e_non_cade_nel_fallback() {
        let catalogo: serde_json::Value =
            serde_json::from_str(nexus_agent_tools::tool_schema::AGENT_TOOLS_JSON)
                .expect("il catalogo dei tool deve parsare");
        let nome = catalogo
            .as_array()
            .expect("catalogo = array")
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .find(|n| *n == "ui_styling_audit")
            .expect("ui_styling_audit deve essere esposto al modello nel catalogo");

        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ctx_for_dispatch_tests(dir.path().to_path_buf());
        let out = execute_agent_tool(
            &ctx,
            nome,
            &serde_json::json!({ "target_dir": "cartella_che_non_esiste" }),
        )
        .await;
        // Il messaggio e' del TOOL: se il nome cadesse nel fallback del
        // dispatcher la risposta sarebbe "Tool ... non esiste" col marker di
        // errore, e questa asserzione fallirebbe.
        assert!(
            out.testo.contains("target_dir 'cartella_che_non_esiste' non esiste nel progetto"),
            "atteso l'errore del tool sul target_dir, non il fallback del dispatcher: {}",
            out.testo
        );
    }

    /// Contro-prova: un nome sconosciuto cade ancora nel fallback.
    ///
    /// GAP1: l'output DEVE iniziare con il marker '\u{274C}' (con eventuale
    /// trim_start) cosi' `tool_runner_server` deriva is_error=true. Il pool e'
    /// lazy non connesso: le query DB del resolver degradano (no panic) e resta
    /// il messaggio base + nudge tool_search.
    #[tokio::test]
    async fn tool_sconosciuto_cade_nel_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ctx_for_dispatch_tests(dir.path().to_path_buf());
        let out = execute_agent_tool(&ctx, "tool_che_non_esiste", &serde_json::json!({})).await;
        assert!(
            out.testo.contains("non esiste"),
            "fallback atteso, ottenuto: {}",
            out.testo
        );
        assert!(
            out.esito.e_fallito(),
            "GAP1: un tool inesistente e' un FALLIMENTO dichiarato nel campo: {}",
            out.testo
        );
        assert!(
            out.testo.contains("nexus_mcp_tool_search"),
            "GAP3: nudge a tool_search sempre presente: {}",
            out.testo
        );
    }

    /// GAP1 (bug chiuso): un `nexus_*` INESISTENTE passa per
    /// nexus_builtin::execute_with_neural -> execute -> fallback `_`. Prima
    /// ritornava "[Nexus Builtin] Tool ... non riconosciuto." SENZA marker ->
    /// is_error=FALSE -> finto successo. Ora il resolver antepone U+274C.
    #[tokio::test]
    async fn nexus_tool_inesistente_ha_marker_errore() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ctx_for_dispatch_tests(dir.path().to_path_buf());
        let out = execute_agent_tool(
            &ctx,
            "nexus_tool_inventato_dal_modello",
            &serde_json::json!({}),
        )
        .await;
        assert!(
            out.esito.e_fallito(),
            "GAP1: un nexus_* inesistente e' un fallimento dichiarato nel campo: {}",
            out.testo
        );
        assert!(
            !out.testo.contains("non riconosciuto"),
            "il vecchio messaggio senza marker non deve piu' comparire: {}",
            out.testo
        );
    }

    /// GAP2: il fuzzy reale (non piu' alias hardcoded) suggerisce il builtin
    /// corretto per un nome storpiato, end-to-end attraverso il dispatcher.
    #[tokio::test]
    async fn fuzzy_storpiato_suggerisce_builtin() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ctx_for_dispatch_tests(dir.path().to_path_buf());
        let out = execute_agent_tool(&ctx, "read_fil", &serde_json::json!({})).await;
        assert!(
            out.esito.e_fallito(),
            "fallimento atteso nel campo esito: {}",
            out.testo
        );
        assert!(
            out.testo.contains("read_file"),
            "GAP2: 'read_fil' deve suggerire read_file: {}",
            out.testo
        );
    }
}
