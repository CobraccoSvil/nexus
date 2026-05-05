-- Aggiunge colonna usage_context per fornire contesto all'assistente AI di editing prompt
ALTER TABLE nexus_prompt_templates
  ADD COLUMN IF NOT EXISTS usage_context TEXT;

-- Popola usage_context per i 17 prompt esistenti
UPDATE nexus_prompt_templates SET usage_context = $$Prompt di sistema principale dell'agente Nexus. Viene inviato come messaggio "system" ad ogni richiesta dell'agente che gestisce la chat utente nell'IDE. Definisce identità, regole di autonomia, gestione tool, gestione file grandi, avvio servizi, gestione errori e profili. Il modello che lo riceve è quello scelto dall'utente (Claude/GPT/Gemini). Non contiene placeholder. Lunghezza tipica: 2-4k caratteri. Modifiche errate impattano TUTTE le sessioni di chat — sii conservativo.$$
  WHERE key = 'system.nexus_base';

UPDATE nexus_prompt_templates SET usage_context = $$Regola di rilevamento N+1 query DB nei progetti. Viene incluso nel prompt del code-quality scanner che analizza i file sorgente cercando pattern di query in loop. La regola deve essere precisa: includere veri pattern N+1 backend (query DB dentro for/while/forEach su array di entità) ed escludere falsi positivi tipici (array.map in JSX, componenti React, codice frontend). Output del modello: lista di finding con file, riga, snippet. Modifiche al testo cambiano la sensibilità del detector.$$
  WHERE key = 'quality.n_plus_one';

UPDATE nexus_prompt_templates SET usage_context = $$Prompt del supervisor che monitora periodicamente l'avanzamento dell'agente Nexus durante un task lungo. Viene chiamato ogni N iterazioni del loop principale. Il modello (haiku veloce) deve decidere se: continuare, redirectare con istruzioni precise, o abbandonare il task come bloccato. Placeholder disponibili: {{task}} = task originale dell'utente, {{steps_summary}} = riassunto step eseguiti finora, {{anomaly_block}} = eventuali anomalie rilevate. Output atteso: JSON con campo "decision" ("continue"|"redirect"|"abort") e "instructions" opzionale.$$
  WHERE key = 'automation.supervisor_monitoring';

UPDATE nexus_prompt_templates SET usage_context = $$Meta-prompt che istruisce un LLM a generare il system prompt per un nuovo profilo utente. Usato dall'endpoint POST /api/users/profiles/suggest-system-prompt quando l'utente crea un profilo personalizzato. Placeholder: {{name}} = nome del profilo, {{desc}} = descrizione opzionale fornita dall'utente. Output atteso: testo italiano di 3-6 frasi che definisce ruolo, specializzazione, stile di risposta. NIENTE introduzioni o markdown — solo il system prompt finale.$$
  WHERE key = 'automation.profile_system_prompt_generator';

UPDATE nexus_prompt_templates SET usage_context = $$Prompt di code review approfondito eseguito in batch via Gemini su un intero progetto. Viene dato al modello insieme al contenuto dei file. Deve istruire il reviewer a cercare problemi di qualità, sicurezza, performance, best practice. Output atteso: JSON array di issue con fields: file, line, severity, category, message, suggestion. Modificarlo cambia il tipo di problemi rilevati nella deep review.$$
  WHERE key = 'quality.deep_review_code_analysis';

UPDATE nexus_prompt_templates SET usage_context = $$Istruzione iniettata nel system prompt quando l'utente seleziona la modalità "Studio". L'agente deve analizzare il codice e spiegare l'impatto delle modifiche proposte SENZA applicarle subito. Nessun placeholder. Lunghezza: 1-3 frasi. Modifiche cambiano il comportamento di tutte le sessioni in modalità studio.$$
  WHERE key = 'automation.mode_study_instruction';

UPDATE nexus_prompt_templates SET usage_context = $$Istruzione iniettata nel system prompt quando l'utente seleziona la modalità "Conferma". L'agente deve proporre le modifiche al codice ma chiedere conferma esplicita all'utente prima di applicarle. Nessun placeholder. Lunghezza: 1-3 frasi.$$
  WHERE key = 'automation.mode_confirm_instruction';

UPDATE nexus_prompt_templates SET usage_context = $$Istruzione iniettata nel system prompt quando l'utente seleziona la modalità "Automatica". L'agente esegue le modifiche immediatamente, segnalando solo le assunzioni rilevanti. Nessun placeholder. Lunghezza: 1-3 frasi.$$
  WHERE key = 'automation.mode_automatic_instruction';

UPDATE nexus_prompt_templates SET usage_context = $$Prompt iniettato quando un run dell'agente viene ripreso dopo un'interruzione (es. riavvio del backend). L'agente riceve la cronologia delle iterazioni precedenti e deve continuare esattamente da dove si era fermato senza ripetere lavoro già fatto. Placeholder: {{prev_iterations}} = numero di iterazioni completate prima dell'interruzione. Lunghezza: 2-4 frasi. Critico per la continuità dei task lunghi.$$
  WHERE key = 'automation.run_resume_instruction';

-- Profili tecnici (8) — usage context comune con variazione per stack
UPDATE nexus_prompt_templates SET usage_context = $$System prompt di un profilo specializzato per sviluppatore C# / .NET. Quando l'utente seleziona questo profilo nell'IDE, questo testo viene aggiunto al system prompt dell'agente per orientarne il comportamento verso lo stack Microsoft. Nessun placeholder. Deve coprire: linee guida Microsoft, principi SOLID, async/await idiomatico, Entity Framework, testing (xUnit/MSTest). Lunghezza tipica: 80-150 parole.$$
  WHERE key = 'profile.developer_csharp_dotnet';

UPDATE nexus_prompt_templates SET usage_context = $$System prompt di un profilo specializzato per sviluppatore React / TypeScript. Iniettato quando l'utente seleziona il profilo nell'IDE. Deve coprire: functional components, hooks (useState/useEffect/useMemo), TypeScript strict mode, state management (Zustand/Redux/Context), styling, testing (Vitest/Jest/Testing Library). Nessun placeholder. Lunghezza: 80-150 parole.$$
  WHERE key = 'profile.developer_react_typescript';

UPDATE nexus_prompt_templates SET usage_context = $$System prompt di un profilo specializzato per sviluppatore Python. Iniettato al cambio di profilo. Deve coprire: Python 3.10+ (type hints, match statements), framework web (FastAPI/Django/Flask), PEP 8, validazione dati (Pydantic), testing (pytest), package management (Poetry/uv/pip). Nessun placeholder. Lunghezza: 80-150 parole.$$
  WHERE key = 'profile.developer_python';

UPDATE nexus_prompt_templates SET usage_context = $$System prompt di un profilo specializzato per sviluppatore Rust. Iniettato al cambio di profilo. Deve coprire: ownership/borrowing model, async ecosystem (Tokio/async-std), framework web (Axum/Actix), Serde, SQLx, gestione errori con Result/?, testing. Nessun placeholder. Lunghezza: 80-150 parole.$$
  WHERE key = 'profile.developer_rust';

UPDATE nexus_prompt_templates SET usage_context = $$System prompt di un profilo specializzato per ingegnere DevOps / Infrastruttura. Iniettato al cambio di profilo. Deve coprire: container (Docker), orchestrazione (Kubernetes), Infrastructure as Code (Terraform/Pulumi/Ansible), CI/CD (GitHub Actions/GitLab CI/Jenkins), cloud (AWS/Azure/GCP), monitoring (Prometheus+Grafana). Nessun placeholder. Lunghezza: 80-150 parole.$$
  WHERE key = 'profile.devops_infrastructure';

UPDATE nexus_prompt_templates SET usage_context = $$System prompt di un profilo specializzato per sviluppatore Vue.js / Nuxt. Iniettato al cambio di profilo. Deve coprire: Vue 3 con Composition API, Pinia store, Nuxt 3 (SSR/SSG), TypeScript, testing (Vitest). Nessun placeholder. Lunghezza: 80-150 parole.$$
  WHERE key = 'profile.developer_vue_nuxt';

UPDATE nexus_prompt_templates SET usage_context = $$System prompt di un profilo specializzato per sviluppatore mobile cross-platform. Iniettato al cambio di profilo. Deve coprire: React Native (Expo), Flutter/Dart, state management (BLoC/Riverpod/Redux), differenze pratiche iOS vs Android, navigation, performance. Nessun placeholder. Lunghezza: 80-150 parole.$$
  WHERE key = 'profile.developer_mobile';

UPDATE nexus_prompt_templates SET usage_context = $$System prompt di un profilo specializzato per data scientist / ML engineer. Iniettato al cambio di profilo. Deve coprire: NumPy, Pandas, scikit-learn, PyTorch, TensorFlow, ambienti Jupyter, MLflow, statistica applicata, validation/CV, feature engineering. Nessun placeholder. Lunghezza: 80-150 parole.$$
  WHERE key = 'profile.data_science_ml';
