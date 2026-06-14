// Punto unico (regola L) delle etichette umane dei tool agente mostrate nella
// chat. I componenti di rendering (agent-status-bubbles, agent-meta-step-card,
// agent-steps-panel) delegano qui invece di duplicare la mappa o mostrare il
// nome tecnico grezzo (es. "request_port", "nexus_mcp_tool_search"), che lascia
// l'utente senza capire cosa sta facendo l'agente. Le etichette sono brevi, in
// italiano, orientate all'utente che SEGUE il lavoro — non sono le description
// tecniche del tool_schema (quelle servono all'LLM, scopo diverso).

const TOOL_LABELS: Record<string, string> = {
  // File
  read_file: "Legge un file",
  read_file_lines: "Legge righe di un file",
  write_file: "Scrive un file",
  create_file: "Crea un file",
  patch_file: "Applica una patch",
  edit_file: "Modifica un file",
  list_files: "Elenca i file",
  search_in_files: "Cerca nel codice",
  search_files: "Cerca file",
  delete_file: "Elimina un file",
  rename_file: "Rinomina un file",
  fs_mkdir: "Crea una cartella",
  fs_copy: "Copia un file",
  fs_move: "Sposta un file",
  format_file: "Formatta un file",
  // Comandi e servizi
  run_command: "Esegue un comando",
  run_in_terminal: "Esegue un comando",
  run_service: "Avvia un servizio",
  stop_service: "Ferma un servizio",
  service_restart: "Riavvia un servizio",
  read_service_output: "Legge l'output di un servizio",
  tail_service_logs: "Legge i log di un servizio",
  list_active_services: "Elenca i servizi attivi",
  // Porte
  request_port: "Richiede una porta",
  nexus_list_ports: "Elenca le porte allocate",
  // Test
  run_tests: "Esegue i test",
  run_specific_test: "Esegue un test",
  run_playwright_tests: "Esegue i test Playwright",
  run_lint_fix: "Corregge il lint",
  // Discovery tool
  nexus_mcp_tool_search: "Cerca gli strumenti disponibili",
  nexus_mcp_tool_call: "Usa uno strumento",
  // Git
  git_status: "Controlla lo stato Git",
  git_stage: "Git: prepara i file",
  git_commit: "Git: crea un commit",
  git_push: "Git: pubblica i commit",
  git_pull: "Git: scarica i commit",
  git_remote_add: "Git: aggiunge un remote",
  // Sotto-agenti
  dispatch_subagent: "Avvia un sotto-agente",
  dispatch_subagents: "Avvia piu' sotto-agenti",
  nexus_subagent_poll: "Controlla i sotto-agenti",
  nexus_subagent_resume: "Riprende un sotto-agente",
  // Ricerca semantica / conoscenza
  search_codebase_semantic: "Ricerca semantica nel codice",
  search_file_semantic: "Ricerca semantica in un file",
  knowledge_search: "Cerca nella conoscenza",
  recall_context: "Recupera il contesto",
  nexus_search_semantic: "Ricerca semantica",
  // Allegati
  nexus_list_attachments: "Elenca gli allegati",
  nexus_inspect_attachment: "Analizza un allegato",
  nexus_read_attachment: "Legge un allegato",
  nexus_extract_pdf_text: "Estrae testo da PDF",
  nexus_extract_docx_text: "Estrae testo da Word",
  nexus_extract_xlsx_data: "Estrae dati da Excel",
  nexus_extract_figma_structure: "Analizza il file Figma",
  nexus_extract_figma_code: "Estrae il codice da Figma",
  nexus_describe_image_attachment: "Descrive un'immagine",
  nexus_install_shadcn_components: "Installa componenti UI",
  // Database
  nexus_db_query: "Interroga il database",
  nexus_db_tables: "Elenca le tabelle",
  nexus_db_describe: "Descrive una tabella",
  // Qualita' / scaffold / doc
  scan_code_quality: "Analizza la qualita' del codice",
  nexus_doc_generate: "Genera documentazione",
  nexus_dev_server_diagnose: "Diagnostica il dev server",
  nexus_verify_scaffold: "Verifica lo scaffold",
  nexus_todo_write: "Aggiorna la lista di attivita'",
  nexus_get_worklog: "Legge il diario di lavoro",
  // Supervisione
  supervisor_check: "Verifica del supervisore",
};

/** Etichetta umana (italiano) per un tool agente. Fallback per i tool MCP o
 *  dinamici non mappati: nome con gli underscore sostituiti da spazi. */
export function toolLabel(name: string): string {
  if (!name) return "";
  return TOOL_LABELS[name] || name.replace(/_/g, " ");
}
