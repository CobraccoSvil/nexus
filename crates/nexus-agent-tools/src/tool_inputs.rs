//! I contratti d'ingresso dei tool: una dichiarazione per tool, e i vocabolari
//! dei valori ammessi che si dividono fra loro.
//!
//! # Perche' un modulo a parte
//!
//! [`crate::input_contract`] e' la MACCHINA (la macro, i tipi, il trait); questo
//! e' il MATERIALE. Tenerli insieme avrebbe sepolto duecento righe di meccanismo
//! sotto mille di dichiarazioni, e sono due cose che si leggono in momenti
//! diversi: la macchina quando si estende, il materiale quando si migra un tool.
//!
//! # Come sono nati
//!
//! Non a mano: li genera `scripts/genera-contratti-tool.py` DAL catalogo che il
//! modello legge davvero ([`crate::tool_schema::AGENT_TOOLS_JSON`]), e il test
//! in fondo verifica che ognuno vi coincida ancora. Scriverli a mano avrebbe
//! introdotto divergenze proprio mentre si costruiva lo strumento che esiste per
//! impedirle (regola O).
//!
//! # Dichiarato non vuol dire migrato
//!
//! Un contratto qui dice che lo schema e il parsing di quel tool sono la stessa
//! scrittura. NON dice che il suo handler lo usi: la migrazione dell'handler e'
//! un lavoro a parte, che per ogni punto d'uscita deve scegliere la NATURA del
//! fallimento — e quella domanda («l'agente puo' farci qualcosa?») non si
//! genera. L'elenco di chi e' passato davvero e' in
//! `mcp-core::agent_tools::dispatch::esegui_tool_migrato`.
//!
//! # Chi non e' qui, e perche'
//!
//! Lo dichiara lo script a ogni esecuzione, con il motivo per nome. Le famiglie:
//! enum rigenerati a runtime dal progetto o dal DB (fissarli qui riporterebbe il
//! difetto in forma nuova), strutture annidate che la macro non esprime ancora,
//! tool senza parametri, e i campi in cui catalogo e handler non dicono la
//! stessa cosa — dove generare vorrebbe dire scegliere chi ha ragione.

crate::tool_enum! {
    Action {
        Create => "create";
        Check => "check";
        Add => "add";
        Update => "update";
    }
}

crate::tool_enum! {
    Blocker {
        Dependency => "dependency";
        Credential => "credential";
        Permission => "permission";
        Service => "service";
        RequestAmbiguity => "request_ambiguity";
        Safety => "safety";
    }
}

crate::tool_enum! {
    Direction {
        MustBeAbsent => "must_be_absent";
        MustBePresent => "must_be_present";
    }
}

crate::tool_enum! {
    DocType {
        FunctionalAnalysis => "functional_analysis";
        TechnicalAnalysis => "technical_analysis";
        ErDiagram => "er_diagram";
        ProjectManagement => "project_management";
        ReleaseNotes => "release_notes";
    }
}

crate::tool_enum! {
    DocsUpdated {
        Updated => "updated";
        NotNeeded => "not_needed";
        Missing => "missing";
    }
}

crate::tool_enum! {
    Encoding {
        Auto => "auto";
        Text => "text";
        Base64 => "base64";
    }
}

crate::tool_enum! {
    Format {
        Json => "json";
    }
}

crate::tool_enum! {
    Intent {
        Feature => "feature";
        Requirement => "requirement";
        Decision => "decision";
        Domain => "domain";
        UserStory => "user_story";
        Architecture => "architecture";
        Fix => "fix";
        Refactor => "refactor";
        Docs => "docs";
        Other => "other";
    }
}

crate::tool_enum! {
    Kind {
        FileTouched => "file_touched";
        Command => "command";
        Error => "error";
        RetryOk => "retry_ok";
        FailedAttempt => "failed_attempt";
        Status => "status";
        Decision => "decision";
    }
}

crate::tool_enum! {
    Method {
        Get => "GET";
        Post => "POST";
        Put => "PUT";
        Patch => "PATCH";
        Delete => "DELETE";
    }
}

crate::tool_enum! {
    NetworkMode {
        None => "none";
        Bridge => "bridge";
        Host => "host";
    }
}

crate::tool_enum! {
    NotificationSeverity {
        Info => "info";
        Success => "success";
        Warning => "warning";
        Error => "error";
    }
}

crate::tool_enum! {
    Outcome {
        Done => "done";
        Blocked => "blocked";
        Partial => "partial";
        NeedsInput => "needs_input";
    }
}

crate::tool_enum! {
    Priority {
        High => "high";
        Normal => "normal";
        Low => "low";
    }
}

crate::tool_enum! {
    RelType {
        Followup => "followup";
        Correction => "correction";
        Refinement => "refinement";
        Duplicate => "duplicate";
        Blocks => "blocks";
        BlockedBy => "blocked_by";
        Relates => "relates";
    }
}

crate::tool_enum! {
    SeverityFilter {
        All => "all";
        High => "high";
        Medium => "medium";
    }
}

crate::tool_enum! {
    Source {
        Conversation => "conversation";
        Project => "project";
        All => "all";
    }
}

crate::tool_enum! {
    Status {
        Pending => "pending";
        InProgress => "in_progress";
        Completed => "completed";
        Blocked => "blocked";
        Skipped => "skipped";
    }
}

crate::tool_enum! {
    Task {
        Document => "document";
        Optimize => "optimize";
        Analyze => "analyze";
    }
}

crate::tool_enum! {
    Type {
        RunCommand => "run_command";
        Http => "http";
        FileExists => "file_exists";
    }
}

crate::tool_object! {
    BatchAnalyzeCodeFile {
        obbligatori {
            path: String, "Percorso relativo del file";
        }
        opzionali {
            content: String, "Contenuto del file (lasciare vuoto per leggere automaticamente)";
        }
    }
}

crate::tool_object! {
    NexusTodoWriteAcceptanceCriteria {
        obbligatori {
        }
        opzionali {
            r#type: Type, "run_command: exit code 0 = passa; http: url + expected_status; file_exists: path.";
            command: String, "";
            expected: String, "";
            url: String, "";
            expected_status: i64, "Per type=http: status atteso (default 200).";
            path: String, "";
        }
    }
}

crate::tool_object! {
    NexusTodoWriteTodo {
        obbligatori {
        }
        opzionali {
            id: String, "UUID del todo (obbligatorio per check/update)";
            seq: i64, "Ordinamento, opzionale (auto per create/add)";
            content: String, "Descrizione atomica e verificabile del todo";
            status: Status, "";
            priority: Priority, "Default 'normal'";
            acceptance_criteria: Vec<NexusTodoWriteAcceptanceCriteria>, 
                "Criteri di accettazione ESEGUIBILI della voce. Forma piatta: type + il campo del tipo.";
        }
    }
}

crate::tool_object! {
    NexusVisualCompareViewport {
        obbligatori {
        }
        opzionali {
            width: i64, "";
            height: i64, "";
        }
    }
}

crate::tool_object! {
    TaskCompleteEndpoint {
        obbligatori {
            method: Method, "Metodo HTTP.";
            url: String, 
                "URL assoluto, con host e porta REALI del servizio che hai avviato — leggi la porta da quella "
                "allocata al servizio, non copiarla da questo esempio (es. http://localhost:8080/api/articoli).";
        }
        opzionali {
            body: serde_json::Map<String, serde_json::Value>, 
                "Corpo JSON della richiesta. OBBLIGATORIO per POST/PUT/PATCH: senza, la chiamata misura la "
                "validazione dell'input e non l'endpoint, e la voce viene scartata.";
            status: i64, "Status atteso, se diverso da un 2xx (200/201/202/204 sono accettati per default).";
        }
    }
}

crate::tool_input! {
    BatchAnalyzeCodeInput for "batch_analyze_code" {
        obbligatori {
            files: Vec<BatchAnalyzeCodeFile>, "Lista di file da analizzare (massimo 20 file per batch)";
            task: Task, 
                "Tipo di analisi: 'document' genera docstring/commenti, 'optimize' suggerisce ottimizzazioni, "
                "'analyze' revisione architetturale e potenziali bug";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    BuildProjectImageInput for "build_project_image" {
        obbligatori {
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    CodeDocInput for "code_doc" {
        obbligatori {
            file_path: String, "Path del file (relativo alla root del progetto, es. 'src/auth/login.ts').";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    CreateProfileInput for "create_profile" {
        obbligatori {
            name: String, "Nome breve del profilo (es. 'Sviluppatore C#', 'Code Reviewer', 'DevOps Engineer')";
            system_prompt: String, 
                "Istruzioni specializzate per questo profilo. Devono descrivere expertise, stile di risposta, "
                "framework preferiti, best practice da seguire.";
        }
        opzionali {
            emoji: String, "Emoji rappresentativa del profilo (es. '🦀', '🔍', '⚙️'). Default: '🤖'";
            description: String, "Descrizione breve del profilo e del suo scopo";
            default_provider: String, 
                "Provider AI preferito per questo profilo ('anthropic', 'openai', 'google', 'auto'). Ometti per "
                "ereditare il globale.";
            default_model: String, "Modello AI preferito per questo profilo. Ometti per ereditare il globale.";
            default_automation: String, 
                "Modalita' automazione preferita ('automatic', 'confirm', 'study'). Ometti per ereditare il "
                "globale.";
            set_as_default: bool, "Se true, imposta questo profilo come predefinito per l'utente.";
        }
    }
}

crate::tool_input! {
    DeleteFileInput for "delete_file" {
        obbligatori {
            path: String, "Percorso relativo alla root del file o directory da eliminare";
        }
        opzionali {
            recursive: bool, "Se true, elimina ricorsivamente (necessario per directory non vuote). Default: false";
        }
    }
}

crate::tool_input! {
    DispatcherEmitEventInput for "dispatcher_emit_event" {
        obbligatori {
            kind: String, "Nome logico dell'evento (es. 'analysis_done', 'config_reloaded')";
        }
        opzionali {
            resource: String, "Categoria della risorsa (es. 'quality', 'deploy', 'custom')";
            payload: serde_json::Map<String, serde_json::Value>, 
                "Dati liberi associati all'evento (verranno serializzati come JSON)";
        }
    }
}

crate::tool_input! {
    DispatcherHighlightPanelInput for "dispatcher_highlight_panel" {
        obbligatori {
            panel: String, "Nome del pannello (playwright|ports|problems|services|database|monitor|files|git)";
        }
        opzionali {
            duration_ms: i64, "Durata del flash in ms (default 800, max 5000)";
        }
    }
}

crate::tool_input! {
    DispatcherPostNotificationInput for "dispatcher_post_notification" {
        obbligatori {
            severity: NotificationSeverity, "Severita' del toast";
            message: String, "Testo del messaggio (in italiano)";
        }
        opzionali {
            panel: String, "Pannello opzionale da evidenziare (playwright|ports|problems|services|database|...)";
            ttl_ms: i64, "Durata visibilita' in ms (default frontend: 5000)";
        }
    }
}

crate::tool_input! {
    DispatcherSetFlagInput for "dispatcher_set_flag" {
        obbligatori {
            key: String, "Nome del flag (es. 'build_in_progress', 'test_suite_running')";
        }
        opzionali {
            value: serde_json::Value, "Valore JSON (boolean, number, string o null per cancellare il flag)";
        }
    }
}

crate::tool_input! {
    DispatcherUpdateMonitorInput for "dispatcher_update_monitor" {
        obbligatori {
            monitor_id: String, "ID univoco del widget (es. 'build_progress', 'http_qps')";
            value: serde_json::Value, "Valore corrente (number, string o object)";
        }
        opzionali {
            label: String, "Etichetta human-readable opzionale";
        }
    }
}

crate::tool_input! {
    /// L'input di `edit_file`, primo tool a passare dal contratto.
    ///
    /// PERCHE' LUI PER PRIMO: e' quello su cui i difetti di questa famiglia si
    /// sono misurati (11% di `old_string non trovato`, con l'estratto che
    /// mostrava la zona sbagliata del file), ed e' gia' migrato a
    /// `RispostaTool` — quindi il contratto d'ingresso completa un giro che era
    /// gia' fatto per l'uscita.
    EditFileInput for "edit_file" {
        obbligatori {
            path: String, "Percorso del file relativo alla root";
            old_string: String, "Stringa esatta da sostituire (deve esistere esattamente una volta nel file)";
            new_string: String, "Stringa con cui sostituire old_string";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    FormatFileInput for "format_file" {
        obbligatori {
            path: String, "Percorso del file da formattare, relativo alla root";
        }
        opzionali {
            check_only: bool, "Se true, verifica senza modificare. Default: false";
        }
    }
}

crate::tool_input! {
    FsCopyInput for "fs_copy" {
        obbligatori {
            from: String, "Percorso sorgente relativo alla root";
            to: String, "Percorso destinazione relativo alla root";
        }
        opzionali {
            overwrite: bool, "Se true, sovrascrive la destinazione se esiste. Default: false";
        }
    }
}

crate::tool_input! {
    FsMkdirInput for "fs_mkdir" {
        obbligatori {
            path: String, "Percorso directory da creare, relativo alla root del progetto (es. 'src/services/auth')";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    FsMoveInput for "fs_move" {
        obbligatori {
            from: String, "Percorso sorgente relativo alla root";
            to: String, "Percorso destinazione relativo alla root";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    GetSandboxConfigInput for "get_sandbox_config" {
        obbligatori {
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    GitCommitInput for "git_commit" {
        obbligatori {
            message: String, "Messaggio di commit";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    GitPullInput for "git_pull" {
        obbligatori {
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    GitPushInput for "git_push" {
        obbligatori {
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    GitRemoteAddInput for "git_remote_add" {
        obbligatori {
            url: String, "URL del remote (https://, git@, ssh://). Obbligatorio.";
        }
        opzionali {
            name: String, "Nome del remote (default 'origin'). Solo alfanumerico/dash/underscore.";
        }
    }
}

crate::tool_input! {
    GitStageInput for "git_stage" {
        obbligatori {
            paths: Vec<String>, "Lista di percorsi file da aggiungere allo staging (relativi alla root)";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    GitStatusInput for "git_status" {
        obbligatori {
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    KnowledgeCreateLinkInput for "knowledge_create_link" {
        obbligatori {
            from_note_id: String, "UUID nota sorgente";
            to_note_id: String, "UUID nota destinazione";
            rel_type: RelType, "Tipo di relazione";
        }
        opzionali {
            confidence: f64, "Confidenza 0-1 (default 1.0)";
        }
    }
}

crate::tool_input! {
    KnowledgeCreateNoteInput for "knowledge_create_note" {
        obbligatori {
            title: String, "Titolo breve (1-200 char)";
            body_md: String, "Contenuto Markdown della nota";
        }
        opzionali {
            intent: Intent, "Categoria semantica della nota (default 'feature')";
            tags: Vec<String>, "Tag opzionali per facilitare ricerca";
            file_paths: Vec<String>, "Path file correlati (opzionale)";
        }
    }
}

crate::tool_input! {
    KnowledgeGetLinksInput for "knowledge_get_links" {
        obbligatori {
            note_id: String, "UUID della nota di cui leggere i link";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    KnowledgeGetNoteInput for "knowledge_get_note" {
        obbligatori {
            note_id: String, "UUID della nota (dal risultato di knowledge_search)";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    KnowledgeGetSubgraphInput for "knowledge_get_subgraph" {
        obbligatori {
        }
        opzionali {
            query: String, "Testo seed: trova le note radice per similarita' semantica. Alternativo a note_id.";
            note_id: String, "UUID di una nota radice. Alternativo a query.";
            rel_types: Vec<RelType>, 
                "Filtra le relazioni. Default: tutte. Per le dipendenze di esecuzione usa "
                "[\"blocks\",\"blocked_by\"].";
            depth: i64, "Profondita' espansione BFS (default 2, max 4)";
            max_nodes: i64, "Numero massimo di nodi (default 30, max 100)";
        }
    }
}

crate::tool_input! {
    KnowledgeImportGraphInput for "knowledge_import_graph" {
        obbligatori {
            format: Format, 
                "Formato: json (node-link {nodes,edges}). Mermaid e DOT non sono ancora supportati "
                "dall'importatore: erano dichiarati qui e rifiutati all'esecuzione.";
            content: String, "Contenuto del grafo nel formato indicato";
        }
        opzionali {
            source_id: String, "Identificatore della sorgente (opzionale, per tracciare l'origine)";
        }
    }
}

crate::tool_input! {
    KnowledgeSearchInput for "knowledge_search" {
        obbligatori {
            query: String, "Testo da cercare (es. 'autenticazione OAuth Google', 'fix bug timezone'). Max 2000 char.";
        }
        opzionali {
            top_k: i64, "Numero massimo di hit (default 5, max 20)";
            min_score: f64, "Soglia minima similarita' 0-1 (default 0.4)";
        }
    }
}

crate::tool_input! {
    KnowledgeSetRelevanceInput for "knowledge_set_relevance" {
        obbligatori {
            note_id: String, "UUID della nota";
            off_topic: bool, "true = fuori tema (esclusa dal grafo), false = pertinente";
        }
        opzionali {
            relevance_score: f64, "Punteggio di pertinenza 0-1 (opzionale)";
        }
    }
}

crate::tool_input! {
    ListActiveServicesInput for "list_active_services" {
        obbligatori {
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    ListFilesInput for "list_files" {
        obbligatori {
        }
        opzionali {
            directory: String, "Directory da listare (relativa alla root). Ometti o usa '' per la root del progetto.";
        }
    }
}

crate::tool_input! {
    NexusDbDescribeInput for "nexus_db_describe" {
        obbligatori {
            table: String, "Nome tabella.";
        }
        opzionali {
            schema: String, "Schema (default 'public').";
        }
    }
}

crate::tool_input! {
    NexusDbQueryInput for "nexus_db_query" {
        obbligatori {
            sql: String, "Statement SQL. Una sola statement per chiamata.";
        }
        opzionali {
            params: Vec<serde_json::Value>, 
                "Parametri posizionali per $1,$2,... Bindati come testo; usa cast nel SQL per tipi non-testo.";
            max_rows: i64, "Max righe ritornate da una SELECT (default e cap: 1000).";
        }
    }
}

crate::tool_input! {
    NexusDbTablesInput for "nexus_db_tables" {
        obbligatori {
        }
        opzionali {
            schema: String, "Schema da listare (default 'public').";
        }
    }
}

crate::tool_input! {
    NexusDescribeImageAttachmentInput for "nexus_describe_image_attachment" {
        obbligatori {
            attachment_id: String, "";
        }
        opzionali {
            question: String, "Domanda opzionale al modello vision (es. 'estrai i testi UI', 'descrivi il layout').";
        }
    }
}

crate::tool_input! {
    NexusDevServerDiagnoseInput for "nexus_dev_server_diagnose" {
        obbligatori {
        }
        opzionali {
            log_path: String, 
                "Path file log da scansionare (es. '/tmp/bb-app.log'). Relativo a project root oppure assoluto. "
                "Letti ultimi 200KB.";
            log: String, "ALTERNATIVA a log_path: stringa di log inline (es. da read_service_output).";
            port: i64, "Porta del dev server (per nota nel risultato, non usata per matching).";
        }
    }
}

crate::tool_input! {
    NexusDocGenerateInput for "nexus_doc_generate" {
        obbligatori {
            doc_type: DocType, "Tipo di documento da generare";
            content_json: serde_json::Map<String, serde_json::Value>, 
                "Contenuto strutturato: { sections: [{ title: string, content: string, subsections?: [...] }] }";
        }
        opzionali {
            title: String, "Titolo del documento (opzionale, default da template)";
        }
    }
}

crate::tool_input! {
    NexusExtractDocxTextInput for "nexus_extract_docx_text" {
        obbligatori {
            attachment_id: String, "";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    NexusExtractFigmaCodeInput for "nexus_extract_figma_code" {
        obbligatori {
            attachment_id: String, "UUID dell'allegato .make.";
        }
        opzionali {
            target_subdir: String, 
                "Sottocartella relativa alla project_root dove scrivere i file estratti. Default 'figma_export'.";
        }
    }
}

crate::tool_input! {
    NexusExtractFigmaStructureInput for "nexus_extract_figma_structure" {
        obbligatori {
            attachment_id: String, "";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    NexusExtractPdfTextInput for "nexus_extract_pdf_text" {
        obbligatori {
            attachment_id: String, "";
        }
        opzionali {
            page_start: i64, "Pagina di inizio 1-based (default 1).";
            page_end: i64, "Pagina di fine inclusa (default ultima pagina).";
        }
    }
}

crate::tool_input! {
    NexusExtractXlsxDataInput for "nexus_extract_xlsx_data" {
        obbligatori {
            attachment_id: String, "";
        }
        opzionali {
            sheet_name: String, "Nome del foglio (es. 'sheet1', 'sheet2'). Default 'sheet1'.";
        }
    }
}

crate::tool_input! {
    NexusGenerateVideoInput for "nexus_generate_video" {
        obbligatori {
            prompt: String, "Descrizione testuale del video da generare. Obbligatorio.";
        }
        opzionali {
            duration_seconds: i64, 
                "Durata richiesta del video in secondi (opzionale). Se omessa si usa il default del provider.";
            filename: String, 
                "Nome file desiderato senza percorso (opzionale): viene salvato in .nexus/generated/ con "
                "estensione .mp4. Se omesso si genera un nome timestampato.";
        }
    }
}

crate::tool_input! {
    NexusGetWorklogInput for "nexus_get_worklog" {
        obbligatori {
        }
        opzionali {
            kind: Kind, "Filtra per tipo di evento. Default: tutti.";
            run_id: String, "Restringe agli eventi di un singolo run (UUID).";
            limit: i64, "Numero massimo di eventi per pagina (default e cap da settings agent.worklog.tool_page_size).";
            offset: i64, "Offset di paginazione (default 0). Gli eventi sono ordinati dal piu' recente.";
        }
    }
}

crate::tool_input! {
    NexusInspectAttachmentInput for "nexus_inspect_attachment" {
        obbligatori {
            attachment_id: String, "UUID dell'allegato (ottenuto da nexus_list_attachments).";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    NexusInstallShadcnComponentsInput for "nexus_install_shadcn_components" {
        obbligatori {
        }
        opzionali {
            components: Vec<String>, 
                "Lista nomi componenti da creare (es. ['button','input','card']). Se omesso, installa il set "
                "base: button/input/label/card/alert/tabs/sonner.";
            target_dir: String, 
                "Path relativo alla project root dove creare gli stub. Default: 'src/components/ui'. Per "
                "progetti con struttura figma_export usa 'figma_export/src/app/components/ui'.";
            overwrite: bool, "Se true, sovrascrive file esistenti. Default false (skip se esiste).";
        }
    }
}

crate::tool_input! {
    NexusListArchiveEntriesInput for "nexus_list_archive_entries" {
        obbligatori {
            attachment_id: String, "UUID dell'allegato.";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    NexusListAttachmentsInput for "nexus_list_attachments" {
        obbligatori {
        }
        opzionali {
            session_id: String, "UUID della sessione chat. Ometti per usare la sessione corrente.";
        }
    }
}

crate::tool_input! {
    NexusListPortsInput for "nexus_list_ports" {
        obbligatori {
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    NexusMcpToolCallInput for "nexus_mcp_tool_call" {
        obbligatori {
            server_id: String, 
                "UUID del server MCP esterno, oppure la sentinella \"builtin\" per i tool interni Nexus "
                "(consigliato per i tool restituiti da next_action_recommended)";
            tool_name: String, "Nome originale del tool (es. 'list_issues', 'create_branch')";
            arguments: serde_json::Map<String, serde_json::Value>, 
                "Argomenti JSON per il tool secondo il suo input_schema";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    NexusMcpToolSearchInput for "nexus_mcp_tool_search" {
        obbligatori {
            query: String, "Query in linguaggio naturale (es. 'esegui test cargo', 'leggi file', 'crea branch git')";
        }
        opzionali {
            limit: i64, "Numero massimo di risultati (default: 10, max: 50)";
        }
    }
}

crate::tool_input! {
    NexusReadArchiveEntryInput for "nexus_read_archive_entry" {
        obbligatori {
            attachment_id: String, "UUID dell'allegato archivio.";
            entry_path: String, 
                "Percorso esatto della entry dentro l'archivio (es. 'word/document.xml', 'src/main.rs').";
        }
        opzionali {
            encoding: Encoding, "Forma del contenuto. Default 'auto'.";
        }
    }
}

crate::tool_input! {
    NexusReadAttachmentInput for "nexus_read_attachment" {
        obbligatori {
            attachment_id: String, "UUID dell'allegato (da nexus_list_attachments). Obbligatorio.";
        }
        opzionali {
            encoding: Encoding, 
                "Forma del contenuto restituito. Default 'auto' (text per mime testuali, altrimenti base64).";
            offset: i64, "Byte offset da cui iniziare la lettura (default 0).";
            length: i64, "Byte massimi da leggere (default 102400, hard cap 102400).";
        }
    }
}

crate::tool_input! {
    NexusSubagentPollInput for "nexus_subagent_poll" {
        obbligatori {
            subagent_run_id: String, "UUID del sub-agent run (ritornato da dispatch_subagent).";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    NexusSubagentResumeInput for "nexus_subagent_resume" {
        obbligatori {
            subagent_run_id: String, "UUID del sub-agent run da riprendere.";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    NexusTextToSpeechInput for "nexus_text_to_speech" {
        obbligatori {
            text: String, "Testo da convertire in audio.";
        }
        opzionali {
            voice: String, 
                "Voce/timbro del modello TTS (es. 'alloy', 'nova'), opzionale: se omessa usa il default del "
                "provider.";
            filename: String, 
                "Nome file desiderato (senza estensione/directory), opzionale: se omesso usa un nome "
                "timestampato. L'estensione e' decisa dal sistema in base al formato audio.";
        }
    }
}

crate::tool_input! {
    NexusTodoWriteInput for "nexus_todo_write" {
        obbligatori {
            action: Action, 
                "Operazione: create=reset+ricrea piano, check=marca completati, add=appende todos, "
                "update=aggiorna status arbitrari.";
            run_id: String, "UUID dell'agent_run corrente (passato dal brain via state.thread_id). Obbligatorio.";
            todos: Vec<NexusTodoWriteTodo>, 
                "Array di todo. Per create/add: content+status+priority+acceptance_criteria. Per check/update: "
                "id obbligatorio.";
        }
        opzionali {
            planner_model: String, "Modello LLM usato dal planner (per audit, opzionale).";
            plan_acceptance_criteria: Vec<serde_json::Value>, 
                "Acceptance criteria globali del plan (opzionale, action=create).";
        }
    }
}

crate::tool_input! {
    NexusTranscribeAudioInput for "nexus_transcribe_audio" {
        obbligatori {
            attachment_id: String, "";
        }
        opzionali {
            language: String, 
                "Lingua dell'audio in ISO-639-1 (es. 'it', 'en'), opzionale: migliora accuratezza e velocita'. "
                "Se omessa il modello la rileva automaticamente.";
        }
    }
}

crate::tool_input! {
    NexusVerifyScaffoldInput for "nexus_verify_scaffold" {
        obbligatori {
        }
        opzionali {
            target_dir: String, 
                "Path relativo alla project root del progetto scaffolded. Default: '.' (root). Per "
                "Beauty-Book/figma_export usa 'figma_export'.";
        }
    }
}

crate::tool_input! {
    NexusVisualCompareInput for "nexus_visual_compare" {
        obbligatori {
            url: String, 
                "URL locale dell'app avviata da screenshottare (es. 'http://localhost:29348/' o una route "
                "specifica). Obbligatorio.";
        }
        opzionali {
            reference: String, 
                "attachment_id del design di riferimento: se e' un .make Figma viene usato il suo thumbnail.png, "
                "se e' un'immagine viene usata direttamente. Ometti per ottenere solo lo screenshot senza "
                "confronto.";
            viewport: NexusVisualCompareViewport, 
                "Dimensioni viewport {width, height}. Default 1280x800 (configurabile in settings "
                "agent.visual_compare.viewport_*).";
            wait_ms: i64, 
                "Attesa (ms) dopo il load prima dello scatto. Default da settings "
                "(agent.visual_compare.wait_ms).";
        }
    }
}

crate::tool_input! {
    ReadFileInput for "read_file" {
        obbligatori {
            path: String, "Percorso del file relativo alla root del progetto (es. 'src/main.rs' o 'README.md')";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    /// L'input di `read_file_lines`.
    ///
    /// Gli estremi sono `i64` e non `usize`: il modello puo' scrivere un numero
    /// negativo, e un tipo che non lo rappresenta trasformerebbe un input
    /// sbagliato in un errore di deserializzazione oscuro invece che in un
    /// controllo di dominio con un messaggio utile.
    ReadFileLinesInput for "read_file_lines" {
        obbligatori {
            path: String, "Percorso del file relativo alla root del progetto";
            start_line: i64, "Riga di inizio (1-based, inclusa). Es: 39 per iniziare dalla riga 39.";
            end_line: i64, 
                "Riga di fine (1-based, inclusa). Es: 80 per leggere fino alla riga 80. Massimo 400 righe per "
                "chiamata.";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    ReadServiceOutputInput for "read_service_output" {
        obbligatori {
            process_id: String, "ID del processo restituito da run_service";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    RecallContextInput for "recall_context" {
        obbligatori {
            query: String, 
                "Descrizione in linguaggio naturale di cosa stai cercando (es. 'output del comando npm install', "
                "'struttura del database', 'errore di autenticazione discusso prima')";
        }
        opzionali {
            source: Source, 
                "Dove cercare: 'conversation' (turni conversazionali correnti), 'project' (contesto e "
                "documentazione del progetto), 'all' (entrambi). Default: 'all'";
            limit: i64, "Numero massimo di risultati (default: 5, max: 10)";
        }
    }
}

crate::tool_input! {
    RenameFileInput for "rename_file" {
        obbligatori {
            from: String, "Percorso sorgente relativo alla root";
            to: String, "Percorso destinazione relativo alla root";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    RequestPortInput for "request_port" {
        obbligatori {
            label: String, 
                "Etichetta logica del servizio (es. 'backend-dev', 'frontend-dev', 'api', 'web'). Obbligatorio.";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    RunCommandInput for "run_command" {
        obbligatori {
            command: String, "Comando da eseguire (es. 'cargo build', 'npm test', 'python -m pytest', './my-server')";
        }
        opzionali {
            working_dir: String, 
                "Sottodirectory in cui eseguire il comando (relativa alla root). Ometti per usare la root del "
                "progetto. Il comando gira GIA' in questa cartella: NON ripeterla nel comando (niente 'cd "
                "<questa>' ne prefissi '<questa>/' nei path), o i percorsi si sommano (es. working_dir=frontend "
                "+ 'frontend/x' esegue in frontend/frontend/x).";
            background: bool, 
                "Se true, il comando viene avviato come servizio server-side in background. Usa per: server "
                "(dotnet run, npm run dev, flask run, ./my-app), watcher (cargo watch, tsc --watch), e qualsiasi "
                "processo che non termina da solo. Default: false.";
        }
    }
}

crate::tool_input! {
    RunLintFixInput for "run_lint_fix" {
        obbligatori {
        }
        opzionali {
            check_only: bool, "Se true, esegue solo il controllo senza applicare fix. Default: false";
            working_dir: String, "Sottodirectory. Ometti per la root del progetto.";
            timeout_secs: i64, "Timeout in secondi. Default: 120, max: 300";
        }
    }
}

crate::tool_input! {
    RunPlaywrightTestsInput for "run_playwright_tests" {
        obbligatori {
        }
        opzionali {
            filter: String, "Filtro per eseguire solo alcuni test (es. 'auth' esegue tutti i file con 'auth' nel nome)";
            project: String, 
                "Progetto Playwright (es. 'chromium', 'firefox', 'webkit'). Ometti per eseguire tutti i browser "
                "configurati.";
            base_url: String, 
                "URL base del server da testare (es. 'http://localhost:3000'). Se omesso, viene letto dalla "
                "porta allocata da Nexus per questo progetto.";
            workers: i64, "Numero di worker paralleli (default: 1)";
            reporter: String, "Formato output: 'list' (default), 'line', 'dot'";
            timeout_secs: i64, "Timeout totale per l'intero run in secondi (default: 600, max: 900)";
            test_timeout_ms: i64, 
                "Timeout per il singolo test in millisecondi (default: 10000 = 10s, max: 60000). Con backend non "
                "disponibile, 10s è sufficiente (connection refused < 1s). Aumentare a 30000 se i test "
                "richiedono caricamento lento o upload di file.";
            auto_start_server: bool, 
                "Se true e il dev server non è raggiungibile, lo avvia automaticamente con run_service prima dei "
                "test (default: false)";
            config_path: String, 
                "Directory relativa alla root del progetto contenente playwright.config.ts (es. 'app'). Se "
                "omesso, Nexus sceglie automaticamente la directory con piu' test spec tra radice e "
                "sottodirectory comuni.";
            cleanup_stale_configs: bool, 
                "Se true (default), rimuove automaticamente config wrapper stale alla radice quando la suite "
                "reale e' in una sottodirectory (es. playwright.config.ts alla radice con 0 test mentre i veri "
                "test sono in app/e2e/).";
        }
    }
}

crate::tool_input! {
    RunServiceInput for "run_service" {
        obbligatori {
            command: String, "Comando da eseguire (es. 'dotnet run', 'npm run dev', 'cargo watch -x run')";
        }
        opzionali {
            working_dir: String, 
                "Sottodirectory in cui eseguire il comando (relativa alla root del progetto). Ometti per usare "
                "la root. Il comando gira GIA' in questa cartella: NON ripeterla nel comando (niente 'cd "
                "<questa>' ne prefissi '<questa>/' nei path), o i percorsi si sommano (es. working_dir=frontend "
                "+ 'frontend/x' esegue in frontend/frontend/x).";
            label: String, 
                "Etichetta breve che descrive ESATTAMENTE quello che fa il comando, derivata dal "
                "package.json/Cargo.toml/pom.xml del progetto. NON inventare nomi (es. NON usare 'Backend .NET' "
                "se il progetto e' Node). Apparira' nel pannello Servizi. Riusa lo stesso label per riavviare un "
                "servizio gia' attivo invece di crearne un duplicato.";
        }
    }
}

crate::tool_input! {
    RunSpecificTestInput for "run_specific_test" {
        obbligatori {
            test_name: String, "Nome o pattern del test da eseguire (es. 'test_auth_login', 'describe auth')";
        }
        opzionali {
            working_dir: String, "Sottodirectory in cui eseguire. Ometti per usare la root del progetto.";
            timeout_secs: i64, "Timeout in secondi. Default: 120, max: 600";
        }
    }
}

crate::tool_input! {
    RunTestsInput for "run_tests" {
        obbligatori {
        }
        opzionali {
            command: String, 
                "Comando test esplicito (es. 'npm test', 'cargo test', 'pytest'). Se omesso, viene auto-rilevato "
                "dal progetto.";
            working_dir: String, "Sottodirectory in cui eseguire (relativa alla root). Ometti per la root.";
            timeout_secs: i64, "Timeout in secondi (default: 120, max: 300).";
            filter: String, 
                "Filtro per eseguire solo test specifici (nome test, file, modulo). Viene aggiunto al comando "
                "del framework.";
        }
    }
}

crate::tool_input! {
    ScanCodeQualityInput for "scan_code_quality" {
        obbligatori {
        }
        opzionali {
            file_path: String, 
                "Path del file da analizzare relativo alla root del progetto. Se omesso, scansiona l'intero "
                "progetto e ritorna i top findings.";
            severity_filter: SeverityFilter, "Filtra per severità minima. Default: all";
        }
    }
}

crate::tool_input! {
    SearchCodebaseSemanticInput for "search_codebase_semantic" {
        obbligatori {
            query: String, "Descrizione in linguaggio naturale di cosa stai cercando nel codebase";
        }
        opzionali {
            limit: i64, "Numero massimo di risultati (default: 8, max: 20)";
        }
    }
}

crate::tool_input! {
    SearchFileSemanticInput for "search_file_semantic" {
        obbligatori {
            path: String, "Percorso del file da analizzare (relativo alla root del progetto o assoluto)";
            query: String, "Cosa stai cercando nel file, in linguaggio naturale";
        }
        opzionali {
            top_k: i64, "Numero massimo di sezioni rilevanti da restituire (default: 5, max: 10)";
            chunk_lines: i64, 
                "Righe per chunk (default: 50). Usa valori più bassi per file strutturati, più alti per log.";
        }
    }
}

crate::tool_input! {
    SearchInFilesInput for "search_in_files" {
        obbligatori {
            pattern: String, "Stringa o pattern regex da cercare";
        }
        opzionali {
            path: String, "Directory in cui cercare (relativa alla root). Ometti per cercare in tutto il progetto.";
        }
    }
}

crate::tool_input! {
    ServiceRestartInput for "service_restart" {
        obbligatori {
            label: String, 
                "Label esatto del servizio da riavviare (deve coincidere con un servizio gia' avviato via "
                "run_service). Usa list_active_services per vedere i label disponibili.";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    SetSandboxConfigInput for "set_sandbox_config" {
        obbligatori {
        }
        opzionali {
            memory_mb: i64, "Limite memoria container in MB. Es: 512, 1024, 2048, 4096. Default: 1024";
            cpus: f64, "Limite CPU in core. Es: 0.5, 1.0, 2.0, 4.0. Default: 2.0";
            network_mode: NetworkMode, 
                "Modalità rete Docker. none=isolamento totale (default sicuro), bridge=accesso internet (per npm "
                "install, curl), host=condivide rete host (per servizi che devono comunicare tra loro)";
            extra_env: serde_json::Map<String, serde_json::Value>, 
                "Variabili d'ambiente aggiuntive iniettate in ogni processo. Es: {\"NODE_ENV\": \"development\", "
                "\"PORT\": \"3000\"}";
        }
    }
}

crate::tool_input! {
    StopServiceInput for "stop_service" {
        obbligatori {
            process_id: String, "ID del processo da fermare";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    TailServiceLogsInput for "tail_service_logs" {
        obbligatori {
        }
        opzionali {
            process_id: String, "ID del processo. Se omesso, usa l'ultimo processo del progetto.";
            max_chars: i64, "Numero massimo di caratteri da restituire. Default: 8000";
            follow_seconds: i64, "Secondi di follow in tempo reale (0 = lettura singola, max 60). Default: 0";
        }
    }
}

crate::tool_input! {
    TaskCompleteInput for "task_complete" {
        obbligatori {
            outcome: Outcome, 
                "Esito macchina del task: done = completato e verificato; blocked = fermo per causa esterna; "
                "partial = completato in parte; needs_input = serve input umano.";
            summary: String, "Resoconto umano finale del lavoro svolto (o del blocco). Mostrato all'utente.";
        }
        opzionali {
            next_step: String, "Cosa resta da fare (obbligatorio con outcome=partial, utile con blocked/needs_input).";
            blocked_by: String, "Descrizione testuale della causa del blocco (solo display).";
            blocker: Blocker, "Categoria macchina della causa del blocco (con outcome=blocked).";
            refusal: bool,
                "true se stai rifiutando il task per ragioni di safety/policy (non per incapacita' "
                "tecnica).";
            docs_updated: DocsUpdated, "Docs (README, docs/) aggiornate in questo change?";
            files_touched: Vec<String>, "Percorsi dei file che hai creato o modificato in questo run.";
            endpoints: Vec<TaskCompleteEndpoint>, 
                "Endpoint HTTP che il tuo lavoro ha creato o reso funzionanti. La verifica finale LI CHIAMA "
                "DAVVERO prima di chiudere, quindi dichiarali tutti, non solo quelli che hai gia' provato tu: "
                "soprattutto quelli di SCRITTURA (POST/PUT/PATCH), dove sta la maggior parte dei guasti. Ogni "
                "voce deve essere eseguibile da sola e ripetibile; se una dipende da un'altra (es. DELETE dopo "
                "POST) elencale in quest'ordine. Una POST di prova crea un dato vero nell'applicazione: usa "
                "valori di prova riconoscibili.";
            rendered_container: String,
                "Selettore CSS dell'elemento che il JavaScript della pagina RIEMPIE all'avvio (es. "
                "'#courses-grid', '.product-list'). Dichiaralo quando il contenuto visibile non sta nell'HTML "
                "ma lo genera il codice: la verifica finale apre la pagina in un browser vero e pretende che "
                "quell'elemento contenga qualcosa. Senza dichiarazione restano gli altri controlli, ma un "
                "contenitore che resta vuoto per un id sbagliato non lo vede nessuno.";
        }
    }
}

crate::tool_input! {
    UiLayoutPatternsInput for "ui_layout_patterns" {
        obbligatori {
        }
        opzionali {
            app_type: String, 
                "Tipo di schermata ('crud', 'dashboard', 'wizard', 'master_detail', 'settings'). Omettilo per "
                "l'indice.";
        }
    }
}

crate::tool_input! {
    UiReferenceSearchInput for "ui_reference_search" {
        obbligatori {
            query: String, "Dominio da cercare, in una frase (es. 'gestione spese personali'). Max 300 caratteri.";
        }
        opzionali {
        }
    }
}

crate::tool_input! {
    UiStylingAuditInput for "ui_styling_audit" {
        obbligatori {
        }
        opzionali {
            target_dir: String, 
                "Sottocartella da esaminare, relativa alla radice del progetto (es. 'frontend'). Omettilo per "
                "l'intero progetto.";
        }
    }
}

crate::tool_input! {
    UpdateProfileInput for "update_profile" {
        obbligatori {
            profile_name: String, "Nome esatto del profilo da aggiornare (deve corrispondere esattamente)";
        }
        opzionali {
            system_prompt: String, "Nuovo system prompt aggiornato per il profilo";
            description: String, "Nuova descrizione del profilo";
            emoji: String, "Nuova emoji per il profilo";
        }
    }
}

crate::tool_input! {
    WriteFileInput for "write_file" {
        obbligatori {
            path: String, "Percorso del file relativo alla root del progetto";
            content: String, "Contenuto completo del file da scrivere";
        }
        opzionali {
        }
    }
}

crate::tool_enum! {
    /// Le proporzioni ammesse per un'immagine generata.
    ///
    /// L'UNICO enum scritto a mano: i suoi valori non sono identificatori, e da
    /// `1024x1024` non discende un nome di variante: il generatore si ferma
    /// invece di produrne uno inventato (`Px1024x1024` non direbbe nulla a
    /// nessuno). Qui il nome puo' dire cosa distingue davvero i tre valori, che
    /// e' la FORMA — ed e' esattamente cio' che un modello deve scegliere.
    ImageSize {
        Quadrata => "1024x1024";
        Orizzontale => "1792x1024";
        Verticale => "1024x1792";
    }
}

crate::tool_input! {
    NexusGenerateImageInput for "nexus_generate_image" {
        obbligatori {
            prompt: String, "Descrizione testuale dell'immagine da generare. Obbligatorio.";
        }
        opzionali {
            size: ImageSize,
                "Dimensione richiesta (opzionale). Valori tipici: '1024x1024', '1792x1024', "
                "'1024x1792'. Se omesso si usa il default del provider.";
            filename: String,
                "Nome file desiderato senza percorso (opzionale): viene salvato in "
                ".nexus/generated/ con estensione .png. Se omesso si genera un nome timestampato.";
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::input_contract::InputTool;

    use super::*;

    /// Ogni tool DICHIARATO, con lo schema che il suo contratto genera.
    fn dichiarati() -> Vec<(&'static str, serde_json::Value)> {
        vec![
            ("batch_analyze_code", <BatchAnalyzeCodeInput as InputTool>::schema()),
            ("build_project_image", <BuildProjectImageInput as InputTool>::schema()),
            ("code_doc", <CodeDocInput as InputTool>::schema()),
            ("create_profile", <CreateProfileInput as InputTool>::schema()),
            ("delete_file", <DeleteFileInput as InputTool>::schema()),
            ("dispatcher_emit_event", <DispatcherEmitEventInput as InputTool>::schema()),
            ("dispatcher_highlight_panel", <DispatcherHighlightPanelInput as InputTool>::schema()),
            ("dispatcher_post_notification", <DispatcherPostNotificationInput as InputTool>::schema()),
            ("dispatcher_set_flag", <DispatcherSetFlagInput as InputTool>::schema()),
            ("dispatcher_update_monitor", <DispatcherUpdateMonitorInput as InputTool>::schema()),
            ("edit_file", <EditFileInput as InputTool>::schema()),
            ("format_file", <FormatFileInput as InputTool>::schema()),
            ("fs_copy", <FsCopyInput as InputTool>::schema()),
            ("fs_mkdir", <FsMkdirInput as InputTool>::schema()),
            ("fs_move", <FsMoveInput as InputTool>::schema()),
            ("get_sandbox_config", <GetSandboxConfigInput as InputTool>::schema()),
            ("git_commit", <GitCommitInput as InputTool>::schema()),
            ("git_pull", <GitPullInput as InputTool>::schema()),
            ("git_push", <GitPushInput as InputTool>::schema()),
            ("git_remote_add", <GitRemoteAddInput as InputTool>::schema()),
            ("git_stage", <GitStageInput as InputTool>::schema()),
            ("git_status", <GitStatusInput as InputTool>::schema()),
            ("knowledge_create_link", <KnowledgeCreateLinkInput as InputTool>::schema()),
            ("knowledge_create_note", <KnowledgeCreateNoteInput as InputTool>::schema()),
            ("knowledge_get_links", <KnowledgeGetLinksInput as InputTool>::schema()),
            ("knowledge_get_note", <KnowledgeGetNoteInput as InputTool>::schema()),
            ("knowledge_get_subgraph", <KnowledgeGetSubgraphInput as InputTool>::schema()),
            ("knowledge_import_graph", <KnowledgeImportGraphInput as InputTool>::schema()),
            ("knowledge_search", <KnowledgeSearchInput as InputTool>::schema()),
            ("knowledge_set_relevance", <KnowledgeSetRelevanceInput as InputTool>::schema()),
            ("list_active_services", <ListActiveServicesInput as InputTool>::schema()),
            ("list_files", <ListFilesInput as InputTool>::schema()),
            ("nexus_db_describe", <NexusDbDescribeInput as InputTool>::schema()),
            ("nexus_db_query", <NexusDbQueryInput as InputTool>::schema()),
            ("nexus_db_tables", <NexusDbTablesInput as InputTool>::schema()),
            ("nexus_describe_image_attachment", <NexusDescribeImageAttachmentInput as InputTool>::schema()),
            ("nexus_dev_server_diagnose", <NexusDevServerDiagnoseInput as InputTool>::schema()),
            ("nexus_doc_generate", <NexusDocGenerateInput as InputTool>::schema()),
            ("nexus_extract_docx_text", <NexusExtractDocxTextInput as InputTool>::schema()),
            ("nexus_extract_figma_code", <NexusExtractFigmaCodeInput as InputTool>::schema()),
            ("nexus_extract_figma_structure", <NexusExtractFigmaStructureInput as InputTool>::schema()),
            ("nexus_extract_pdf_text", <NexusExtractPdfTextInput as InputTool>::schema()),
            ("nexus_extract_xlsx_data", <NexusExtractXlsxDataInput as InputTool>::schema()),
            ("nexus_generate_video", <NexusGenerateVideoInput as InputTool>::schema()),
            ("nexus_get_worklog", <NexusGetWorklogInput as InputTool>::schema()),
            ("nexus_inspect_attachment", <NexusInspectAttachmentInput as InputTool>::schema()),
            ("nexus_install_shadcn_components", <NexusInstallShadcnComponentsInput as InputTool>::schema()),
            ("nexus_list_archive_entries", <NexusListArchiveEntriesInput as InputTool>::schema()),
            ("nexus_list_attachments", <NexusListAttachmentsInput as InputTool>::schema()),
            ("nexus_list_ports", <NexusListPortsInput as InputTool>::schema()),
            ("nexus_mcp_tool_call", <NexusMcpToolCallInput as InputTool>::schema()),
            ("nexus_mcp_tool_search", <NexusMcpToolSearchInput as InputTool>::schema()),
            ("nexus_read_archive_entry", <NexusReadArchiveEntryInput as InputTool>::schema()),
            ("nexus_read_attachment", <NexusReadAttachmentInput as InputTool>::schema()),
            ("nexus_subagent_poll", <NexusSubagentPollInput as InputTool>::schema()),
            ("nexus_subagent_resume", <NexusSubagentResumeInput as InputTool>::schema()),
            ("nexus_text_to_speech", <NexusTextToSpeechInput as InputTool>::schema()),
            ("nexus_todo_write", <NexusTodoWriteInput as InputTool>::schema()),
            ("nexus_transcribe_audio", <NexusTranscribeAudioInput as InputTool>::schema()),
            ("nexus_verify_scaffold", <NexusVerifyScaffoldInput as InputTool>::schema()),
            ("nexus_visual_compare", <NexusVisualCompareInput as InputTool>::schema()),
            ("read_file", <ReadFileInput as InputTool>::schema()),
            ("read_file_lines", <ReadFileLinesInput as InputTool>::schema()),
            ("read_service_output", <ReadServiceOutputInput as InputTool>::schema()),
            ("recall_context", <RecallContextInput as InputTool>::schema()),
            ("rename_file", <RenameFileInput as InputTool>::schema()),
            ("request_port", <RequestPortInput as InputTool>::schema()),
            ("run_command", <RunCommandInput as InputTool>::schema()),
            ("run_lint_fix", <RunLintFixInput as InputTool>::schema()),
            ("run_playwright_tests", <RunPlaywrightTestsInput as InputTool>::schema()),
            ("run_service", <RunServiceInput as InputTool>::schema()),
            ("run_specific_test", <RunSpecificTestInput as InputTool>::schema()),
            ("run_tests", <RunTestsInput as InputTool>::schema()),
            ("scan_code_quality", <ScanCodeQualityInput as InputTool>::schema()),
            ("search_codebase_semantic", <SearchCodebaseSemanticInput as InputTool>::schema()),
            ("search_file_semantic", <SearchFileSemanticInput as InputTool>::schema()),
            ("search_in_files", <SearchInFilesInput as InputTool>::schema()),
            ("service_restart", <ServiceRestartInput as InputTool>::schema()),
            ("set_sandbox_config", <SetSandboxConfigInput as InputTool>::schema()),
            ("stop_service", <StopServiceInput as InputTool>::schema()),
            ("tail_service_logs", <TailServiceLogsInput as InputTool>::schema()),
            ("task_complete", <TaskCompleteInput as InputTool>::schema()),
            ("ui_layout_patterns", <UiLayoutPatternsInput as InputTool>::schema()),
            ("ui_reference_search", <UiReferenceSearchInput as InputTool>::schema()),
            ("ui_styling_audit", <UiStylingAuditInput as InputTool>::schema()),
            ("update_profile", <UpdateProfileInput as InputTool>::schema()),
            ("write_file", <WriteFileInput as InputTool>::schema()),
            ("nexus_generate_image", <NexusGenerateImageInput as InputTool>::schema()),
        ]
    }

    /// IL test della migrazione (regola O): per OGNI tool dichiarato, lo schema
    /// generato dal contratto e' lo STESSO che il catalogo consegna al modello.
    /// Non confronta due stringhe scritte a mano — prende il catalogo REALE.
    ///
    /// E' il ponte che rende la migrazione un'operazione verificabile invece di
    /// una riscrittura di cui fidarsi: un campo rinominato, una descrizione
    /// cambiata, un obbligatorio diventato opzionale fanno rosseggiare questo
    /// test prima che il modello se ne accorga in esercizio.
    ///
    /// MUTAZIONE: cambiando una descrizione o spostando un campo fra
    /// `obbligatori` e `opzionali`, rosseggia nominando il tool.
    #[test]
    fn ogni_schema_generato_coincide_con_il_catalogo() {
        let catalogo: serde_json::Value =
            serde_json::from_str(crate::tool_schema::AGENT_TOOLS_JSON).expect("catalogo valido");
        let elenco = catalogo.as_array().expect("array");

        for (nome, generato) in dichiarati() {
            let a_mano = elenco
                .iter()
                .find(|t| t["name"] == nome)
                .map(|t| t["input_schema"].clone())
                .unwrap_or_else(|| panic!("{nome} non e' nel catalogo"));

            assert_eq!(
                generato["properties"], a_mano["properties"],
                "properties divergenti per '{nome}'"
            );

            let ordina = |v: &serde_json::Value| {
                let mut r: Vec<String> = v
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                r.sort();
                r
            };
            assert_eq!(
                ordina(&generato["required"]),
                ordina(&a_mano["required"]),
                "obbligatori divergenti per '{nome}'"
            );
        }
    }

    /// La copertura cresce davvero: se qualcuno aggiunge un `tool_input!` senza
    /// registrarlo in `dichiarati()`, il conteggio lo dichiara. Non e' un test
    /// della logica — e' un promemoria che fallisce, che vale piu' di un
    /// commento.
    #[test]
    fn il_conteggio_dei_dichiarati_e_esplicito() {
        assert_eq!(
            dichiarati().len(),
            88,
            "aggiornare il conteggio quando si dichiara un tool (e aggiungerlo a dichiarati())"
        );
    }
}
