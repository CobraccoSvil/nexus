//! Schema JSON dei tool esposti all'agente (formato Anthropic).
//!
//! Estratto da  (refactor god-file). Costante pura, nessuna logica.

/// Schema JSON dei tool nel formato Anthropic (compatibile con OpenAI dopo conversione).
pub const AGENT_TOOLS_JSON: &str = r#"[
  {
    "name": "read_file",
    "description": "Usa questo tool per leggere il contenuto completo di un file. Ideale per analizzare codice sorgente, config, output di comandi. Per file grandi (>300 righe) restituisce solo le prime 300 righe. Non usare per file grandi interi: prima usa search_file_semantic per trovare le sezioni rilevanti, poi read_file_lines.",
    "input_schema": {
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "description": "Percorso del file relativo alla root del progetto (es. 'src/main.rs' o 'README.md')"
        }
      },
      "required": [
        "path"
      ]
    }
  },
  {
    "name": "read_file_lines",
    "description": "Usa questo tool per leggere un range specifico di righe dopo aver identificato le righe di interesse. Parametri: path, start_line, end_line (1-based, inclusi). Massimo 400 righe per chiamata. Non usare senza avere una sezione specifica da leggere: risparmi contesto leggendo solo ciò che serve.",
    "input_schema": {
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "description": "Percorso del file relativo alla root del progetto"
        },
        "start_line": {
          "type": "integer",
          "description": "Riga di inizio (1-based, inclusa). Es: 39 per iniziare dalla riga 39."
        },
        "end_line": {
          "type": "integer",
          "description": "Riga di fine (1-based, inclusa). Es: 80 per leggere fino alla riga 80. Massimo 400 righe per chiamata."
        }
      },
      "required": [
        "path",
        "start_line",
        "end_line"
      ]
    }
  },
  {
    "name": "write_file",
    "description": "Usa questo tool per creare nuovi file o riscrivere completamente un file esistente. Crea automaticamente directory intermedie. Non usare per modifiche puntuali su file esistenti: in quel caso usa edit_file che è più sicuro e preciso.",
    "input_schema": {
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "description": "Percorso del file relativo alla root del progetto"
        },
        "content": {
          "type": "string",
          "description": "Contenuto completo del file da scrivere"
        }
      },
      "required": [
        "path",
        "content"
      ]
    }
  },
  {
    "name": "list_files",
    "description": "Usa questo tool per esplorare la struttura del progetto, scoprire quali file esistono in una directory o verificare l'organizzazione. Non usare per cercare file specifici: usa search_in_files (exact match) o search_codebase_semantic (search semantico).",
    "input_schema": {
      "type": "object",
      "properties": {
        "directory": {
          "type": "string",
          "description": "Directory da listare (relativa alla root). Ometti o usa '' per la root del progetto."
        }
      }
    }
  },
  {
    "name": "search_in_files",
    "description": "Usa questo tool per trovare occorrenze esatte di simboli, funzioni, costanti, pattern regex nel codebase quando conosci il testo esatto (nome funzione, costante, import). Non usare per ricerche concettuali come 'gestione autenticazione': usa search_codebase_semantic per quelle.",
    "input_schema": {
      "type": "object",
      "properties": {
        "pattern": {
          "type": "string",
          "description": "Stringa o pattern regex da cercare"
        },
        "path": {
          "type": "string",
          "description": "Directory in cui cercare (relativa alla root). Ometti per cercare in tutto il progetto."
        }
      },
      "required": [
        "pattern"
      ]
    }
  },
  {
    "name": "git_status",
    "description": "Usa questo tool per verificare lo stato attuale del repository: file modificati, staged, non tracciati, branch corrente. Esegui sempre prima di un commit per controllare cosa sta per essere salvato. Non usare per visualizzare lo storico dei commit: usa git log per quello.",
    "input_schema": {
      "type": "object",
      "properties": {}
    }
  },
  {
    "name": "git_stage",
    "description": "Usa questo tool per preparare file per il commit aggiungendoli all'area di staging. Parametri: paths (lista di file o glob). Non usare per fare commit: questo tool solo prepara, devi usare git_commit dopo.",
    "input_schema": {
      "type": "object",
      "properties": {
        "paths": {
          "type": "array",
          "items": {
            "type": "string"
          },
          "description": "Lista di percorsi file da aggiungere allo staging (relativi alla root)"
        }
      },
      "required": [
        "paths"
      ]
    }
  },
  {
    "name": "git_commit",
    "description": "Usa questo tool per creare un commit con i file in staging. Parametri: message (messaggio di commit). Esegui git_stage prima se i file non sono ancora staged. Non usare per salvare senza message: il messaggio è obbligatorio per tracciare i cambiamenti.",
    "input_schema": {
      "type": "object",
      "properties": {
        "message": {
          "type": "string",
          "description": "Messaggio di commit"
        }
      },
      "required": [
        "message"
      ]
    }
  },
  {
    "name": "git_push",
    "description": "Usa questo tool DOPO git_commit per inviare i commit locali al repository remoto e triggerare pipeline CI/CD. Non usare senza prima aver creato il commit con git_commit o senza aver verificato con git_status che non ci siano conflitti.",
    "input_schema": {
      "type": "object",
      "properties": {}
    }
  },
  {
    "name": "git_pull",
    "description": "Usa questo tool per aggiornare il branch locale con le modifiche dal remote (usa --rebase per evitare merge). Esegui prima di iniziare nuove modifiche per evitare conflitti. Non usare in mezzo a modifiche locali non salvate: salva prima con git_commit.",
    "input_schema": {
      "type": "object",
      "properties": {}
    }
  },
  {
    "name": "git_remote_add",
    "description": "Usa questo tool per configurare un remote git (es. origin) puntando a un URL https://github.com/.../repo.git. Tool atomico Nexus che evita run_command shell. Idempotente: se il remote esiste gia, viene rimosso e ricreato col nuovo URL. Validazione: URL deve iniziare con https://, git@ o ssh:// (file:// rifiutati). Necessario prima di git_push verso un repository appena creato.",
    "input_schema": {
      "type": "object",
      "properties": {
        "name": {
          "type": "string",
          "description": "Nome del remote (default 'origin'). Solo alfanumerico/dash/underscore."
        },
        "url": {
          "type": "string",
          "description": "URL del remote (https://, git@, ssh://). Obbligatorio."
        }
      },
      "required": ["url"]
    }
  },
  {
    "name": "request_port",
    "description": "Fix M51: alloca una porta TCP libera per il progetto dal bucket deterministico Nexus (20000-39999). Usa questo tool al posto di hardcodare 3002/5173 o di chiamare curl all'endpoint REST allocate-port. Idempotente: chiamate ripetute con stessa label ritornano la stessa porta. La porta viene registrata in nexus_port_allocations e propagata automaticamente in run_configurations.env.PORT al prossimo run completed (M40). Ritorna JSON {port, label, allocation_mode}.",
    "input_schema": {
      "type": "object",
      "properties": {
        "label": {
          "type": "string",
          "description": "Etichetta logica del servizio (es. 'backend-dev', 'frontend-dev', 'api', 'web'). Obbligatorio."
        }
      },
      "required": ["label"]
    }
  },
  {
    "name": "nexus_todo_write",
    "description": "Gestisce la TODO list del piano agente. Azioni: create (planner: nuovo plan), check (marca completato), update (cambia status), add (append). Ritorna {ok,action,affected,todo_ids?}.",
    "input_schema": {
      "type": "object",
      "properties": {
        "action": {
          "type": "string",
          "enum": ["create", "check", "add", "update"],
          "description": "Operazione: create=reset+ricrea piano, check=marca completati, add=appende todos, update=aggiorna status arbitrari."
        },
        "run_id": {
          "type": "string",
          "description": "UUID dell'agent_run corrente (passato dal brain via state.thread_id). Obbligatorio."
        },
        "todos": {
          "type": "array",
          "description": "Array di todo. Per create/add: content+status+priority+acceptance_criteria. Per check/update: id obbligatorio.",
          "items": {
            "type": "object",
            "properties": {
              "id": {"type": "string", "description": "UUID del todo (obbligatorio per check/update)"},
              "seq": {"type": "integer", "description": "Ordinamento, opzionale (auto per create/add)"},
              "content": {"type": "string", "description": "Descrizione atomica e verificabile del todo"},
              "status": {"type": "string", "enum": ["pending", "in_progress", "completed", "blocked", "skipped"]},
              "priority": {"type": "string", "enum": ["high", "normal", "low"], "description": "Default 'normal'"},
              "acceptance_criteria": {
                "type": "array",
                "description": "Array di check verificabili: [{type:'run_command'|'http'|'file_exists'|'regex_in_output'|'db_query', spec:{...}, expected:{...}}]"
              }
            }
          }
        },
        "planner_model": {
          "type": "string",
          "description": "Modello LLM usato dal planner (per audit, opzionale)."
        },
        "plan_acceptance_criteria": {
          "type": "array",
          "description": "Acceptance criteria globali del plan (opzionale, action=create)."
        }
      },
      "required": ["action", "run_id", "todos"]
    }
  },
  {
    "name": "dispatch_subagent",
    "description": "Delega un sotto-task a un sub-agent isolato con context window pulito e tool whitelist propria. Il sub-agent ritorna SOLO un summary compatto al main. SCEGLI IL `kind` GIUSTO PER L'AZIONE: usa kind implementativi (rust_implementer, python_implementer, frontend_implementer, db_architect, doc_writer, test_author) per task che richiedono SCRITTURA EFFETTIVA di codice/file/migrazioni. Usa kind generici (plan, explore, verify, review) solo per task analitici. NON usare 'explore' se devi creare/modificare file: usa 'implement' o il kind specifico per linguaggio/dominio.",
    "input_schema": {
      "type": "object",
      "properties": {
        "kind": {
          "type": "string",
          "enum": [
            "plan", "explore", "implement", "verify", "review",
            "rust_implementer", "python_implementer", "frontend_implementer",
            "db_architect", "doc_writer", "test_author"
          ],
          "description": "Tipo di sub-agent. SCEGLI implementativi (rust_implementer/python_implementer/frontend_implementer/db_architect/doc_writer/test_author) per creare/modificare file. SCEGLI explore solo per analisi senza scrittura. 'implement' e' il fallback generico se nessun specialista combacia."
        },
        "task": {
          "type": "string",
          "description": "Descrizione COMPLETA e AUTONOMA del sotto-task. Il sub-agent non vede la conversation del main: includi obiettivo, file da toccare, vincoli, criteri di completamento."
        },
        "context": {
          "type": "string",
          "description": "Contesto aggiuntivo opzionale: file rilevanti, vincoli, decisioni precedenti."
        },
        "expected_output_format": {
          "type": "string",
          "description": "Forma del summary atteso, es. 'lista file modificati', 'paragrafo 300 char con file:linea', 'json {passed, results}'."
        }
      },
      "required": ["kind", "task"]
    }
  },
  {
    "name": "dispatch_subagents",
    "description": "Come dispatch_subagent ma esegue PIU' sub-agent IN PARALLELO (a ondate). Usalo quando hai piu' task INDIPENDENTI da svolgere contemporaneamente (es. rami indipendenti di un piano). Per un singolo task usa dispatch_subagent. STESSE REGOLE sui kind: usa implementativi (*_implementer/db_architect/doc_writer/test_author) per scrivere codice, explore solo per analisi.",
    "input_schema": {
      "type": "object",
      "properties": {
        "tasks": {
          "type": "array",
          "description": "Task indipendenti (1-8) eseguiti in parallelo",
          "items": {
            "type": "object",
            "properties": {
              "kind": {
                "type": "string",
                "enum": [
                  "plan", "explore", "implement", "verify", "review",
                  "rust_implementer", "python_implementer", "frontend_implementer",
                  "db_architect", "doc_writer", "test_author"
                ],
                "description": "Tipo di sub-agent (vedi dispatch_subagent per la guida)"
              },
              "task": {"type": "string", "description": "Descrizione COMPLETA e AUTONOMA del task"},
              "context": {"type": "string", "description": "Contesto aggiuntivo opzionale"},
              "expected_output_format": {"type": "string", "description": "Forma del summary atteso (opzionale)"}
            },
            "required": ["kind", "task"]
          }
        },
        "max_parallel": {"type": "integer", "description": "Ampiezza ondata concorrente (default 2, max 4)"}
      },
      "required": ["tasks"]
    }
  },
  {
    "name": "nexus_subagent_poll",
    "description": "PR-3: poll dello stato di un sub-agent in background. Usa con il subagent_run_id ritornato da dispatch_subagent quando il kind ha is_background=true (il main riceve subito status=running, poi fa polling). Ritorna lo stato corrente + summary se completed.",
    "input_schema": {
      "type": "object",
      "properties": {
        "subagent_run_id": {
          "type": "string",
          "description": "UUID del sub-agent run (ritornato da dispatch_subagent)."
        }
      },
      "required": ["subagent_run_id"]
    }
  },
  {
    "name": "nexus_subagent_resume",
    "description": "PR-3: riprende un sub-agent paused o timeout-ato. Marca lo status come running e re-invia al brain per ri-esecuzione. Usa solo con sub-agent precedentemente pausati.",
    "input_schema": {
      "type": "object",
      "properties": {
        "subagent_run_id": {
          "type": "string",
          "description": "UUID del sub-agent run da riprendere."
        }
      },
      "required": ["subagent_run_id"]
    }
  },
  {
    "name": "run_service",
    "description": "Usa questo tool per avviare servizi/processi long-running (server, watcher, file monitor). L'output è catturato nel pannello Output IDE. Restituisce process_id per leggere output con read_service_output. Non usare per comandi sincroni veloci: usa run_command per quelli.",
    "input_schema": {
      "type": "object",
      "properties": {
        "command": {
          "type": "string",
          "description": "Comando da eseguire (es. 'dotnet run', 'npm run dev', 'cargo watch -x run')"
        },
        "working_dir": {
          "type": "string",
          "description": "Sottodirectory in cui eseguire il comando (relativa alla root del progetto). Ometti per usare la root."
        },
        "label": {
          "type": "string",
          "description": "Etichetta breve che descrive ESATTAMENTE quello che fa il comando, derivata dal package.json/Cargo.toml/pom.xml del progetto. NON inventare nomi (es. NON usare 'Backend .NET' se il progetto e' Node). Apparira' nel pannello Servizi. Riusa lo stesso label per riavviare un servizio gia' attivo invece di crearne un duplicato."
        }
      },
      "required": [
        "command"
      ]
    }
  },
  {
    "name": "read_service_output",
    "description": "Usa questo tool per leggere l'output di un servizio avviato con run_service. Verifica se il servizio è partito, rileva errori, controlla la porta. Parametri: process_id. Non usare senza prima aver lanciato il servizio con run_service.",
    "input_schema": {
      "type": "object",
      "properties": {
        "process_id": {
          "type": "string",
          "description": "ID del processo restituito da run_service"
        }
      },
      "required": [
        "process_id"
      ]
    }
  },
  {
    "name": "stop_service",
    "description": "Usa questo tool per fermare un servizio avviato con run_service. Invia SIGTERM e poi SIGKILL se necessario. Parametri: process_id. Non usare per comandi non lanciati con run_service.",
    "input_schema": {
      "type": "object",
      "properties": {
        "process_id": {
          "type": "string",
          "description": "ID del processo da fermare"
        }
      },
      "required": [
        "process_id"
      ]
    }
  },
  {
    "name": "service_restart",
    "description": "Riavvia un servizio per label: ferma tutti i processi con la stessa label, poi riavvia con lo stesso comando originale. Utile dopo modifiche al codice per applicare le modifiche senza dover ricordare il comando di avvio.",
    "input_schema": {
      "type": "object",
      "properties": {
        "label": {
          "type": "string",
          "description": "Label esatto del servizio da riavviare (deve coincidere con un servizio gia' avviato via run_service). Usa list_active_services per vedere i label disponibili."
        }
      },
      "required": [
        "label"
      ]
    }
  },
  {
    "name": "tail_service_logs",
    "description": "Legge l'output di un servizio con opzione follow. Senza follow_seconds, restituisce l'output attuale (ultime righe). Con follow_seconds, monitora l'output per N secondi catturando nuove righe in tempo reale (max 60s). Utile per debugging servizi in esecuzione.",
    "input_schema": {
      "type": "object",
      "properties": {
        "process_id": {
          "type": "string",
          "description": "ID del processo. Se omesso, usa l'ultimo processo del progetto."
        },
        "max_chars": {
          "type": "integer",
          "description": "Numero massimo di caratteri da restituire. Default: 8000"
        },
        "follow_seconds": {
          "type": "integer",
          "description": "Secondi di follow in tempo reale (0 = lettura singola, max 60). Default: 0"
        }
      }
    }
  },
  {
    "name": "list_active_services",
    "description": "Lista tutti i servizi/processi del progetto corrente con stato, PID, exit code, comando e timestamp di avvio. Mostra sia servizi attivi che recentemente fermati (ultimi 20).",
    "input_schema": {
      "type": "object",
      "properties": {}
    }
  },
  {
    "name": "fs_mkdir",
    "description": "Crea una directory con semantica -p (idempotente, crea genitori mancanti). Percorso relativo alla root del progetto.",
    "input_schema": {
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "description": "Percorso directory da creare, relativo alla root del progetto (es. 'src/services/auth')"
        }
      },
      "required": ["path"]
    }
  },
  {
    "name": "fs_copy",
    "description": "Copia un file o una directory (ricorsiva) dentro la root del progetto. Rifiuta sovrascritture senza flag overwrite:true.",
    "input_schema": {
      "type": "object",
      "properties": {
        "from": {
          "type": "string",
          "description": "Percorso sorgente relativo alla root"
        },
        "to": {
          "type": "string",
          "description": "Percorso destinazione relativo alla root"
        },
        "overwrite": {
          "type": "boolean",
          "description": "Se true, sovrascrive la destinazione se esiste. Default: false"
        }
      },
      "required": ["from", "to"]
    }
  },
  {
    "name": "fs_move",
    "description": "Sposta (rinomina) un file o directory. Atomico se sullo stesso filesystem. Rifiuta se la destinazione esiste.",
    "input_schema": {
      "type": "object",
      "properties": {
        "from": {
          "type": "string",
          "description": "Percorso sorgente relativo alla root"
        },
        "to": {
          "type": "string",
          "description": "Percorso destinazione relativo alla root"
        }
      },
      "required": ["from", "to"]
    }
  },
  {
    "name": "run_specific_test",
    "description": "Esegue un singolo test per nome invece della suite intera. Rileva il framework: cargo test (Rust), vitest/jest/pnpm (Node), pytest (Python), mix (Elixir), go test (Go). Molto piu' veloce di run full suite.",
    "input_schema": {
      "type": "object",
      "properties": {
        "test_name": {
          "type": "string",
          "description": "Nome o pattern del test da eseguire (es. 'test_auth_login', 'describe auth')"
        },
        "working_dir": {
          "type": "string",
          "description": "Sottodirectory in cui eseguire. Ometti per usare la root del progetto."
        },
        "timeout_secs": {
          "type": "integer",
          "description": "Timeout in secondi. Default: 120, max: 600"
        }
      },
      "required": ["test_name"]
    }
  },
  {
    "name": "run_lint_fix",
    "description": "Esegue il linter con fix automatico. Rileva: clippy --fix (Rust), eslint --fix (Node), ruff --fix (Python). Con check_only:true esegue solo il controllo senza modificare i file.",
    "input_schema": {
      "type": "object",
      "properties": {
        "check_only": {
          "type": "boolean",
          "description": "Se true, esegue solo il controllo senza applicare fix. Default: false"
        },
        "working_dir": {
          "type": "string",
          "description": "Sottodirectory. Ometti per la root del progetto."
        },
        "timeout_secs": {
          "type": "integer",
          "description": "Timeout in secondi. Default: 120, max: 300"
        }
      }
    }
  },
  {
    "name": "format_file",
    "description": "Formatta un singolo file con il formatter appropriato. Supporta: .rs (rustfmt), .ts/.js/.json/.css/.md (prettier), .py (black), .go (gofmt). Con check_only:true verifica senza modificare.",
    "input_schema": {
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "description": "Percorso del file da formattare, relativo alla root"
        },
        "check_only": {
          "type": "boolean",
          "description": "Se true, verifica senza modificare. Default: false"
        }
      },
      "required": ["path"]
    }
  },
  {
    "name": "delete_file",
    "description": "Usa questo tool per eliminare file/directory nel progetto. Parametri: path, recursive=true per directory non vuote. Usa con attenzione: l'operazione è irreversibile a meno che non sia in git. Non usare per file in staging area: usa git reset prima.",
    "input_schema": {
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "description": "Percorso relativo alla root del file o directory da eliminare"
        },
        "recursive": {
          "type": "boolean",
          "description": "Se true, elimina ricorsivamente (necessario per directory non vuote). Default: false"
        }
      },
      "required": [
        "path"
      ]
    }
  },
  {
    "name": "rename_file",
    "description": "Usa questo tool per rinominare o spostare file/directory mantenendo la storia git. Parametri: old_path, new_path. Non usare tramite file operations separate: questo tool è atomico e preserva la storia.",
    "input_schema": {
      "type": "object",
      "properties": {
        "from": {
          "type": "string",
          "description": "Percorso sorgente relativo alla root"
        },
        "to": {
          "type": "string",
          "description": "Percorso destinazione relativo alla root"
        }
      },
      "required": [
        "from",
        "to"
      ]
    }
  },
  {
    "name": "edit_file",
    "description": "Usa questo tool per modifiche chirurgiche su file esistenti sostituendo stringa esatta con un'altra. Il file deve contenere esattamente una occorrenza di old_string. Aggiungi contesto circostante se la stringa non è unica. Non usare per riscritture complete: usa write_file per quelle.",
    "input_schema": {
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "description": "Percorso del file relativo alla root"
        },
        "old_string": {
          "type": "string",
          "description": "Stringa esatta da sostituire (deve esistere esattamente una volta nel file)"
        },
        "new_string": {
          "type": "string",
          "description": "Stringa con cui sostituire old_string"
        }
      },
      "required": [
        "path",
        "old_string",
        "new_string"
      ]
    }
  },
  {
    "name": "run_command",
    "description": "Usa questo tool per eseguire comandi nella root del progetto (build, test, lint, format, etc.). Default sincrono con timeout 120s. Imposta background=true per server/watcher/processi interattivi. Non usare per lunghe operazioni: usa run_service per quelle.",
    "input_schema": {
      "type": "object",
      "properties": {
        "command": {
          "type": "string",
          "description": "Comando da eseguire (es. 'cargo build', 'npm test', 'python -m pytest', './my-server')"
        },
        "working_dir": {
          "type": "string",
          "description": "Sottodirectory in cui eseguire il comando (relativa alla root). Ometti per usare la root del progetto."
        },
        "background": {
          "type": "boolean",
          "description": "Se true, il comando viene avviato come servizio server-side in background. Usa per: server (dotnet run, npm run dev, flask run, ./my-app), watcher (cargo watch, tsc --watch), e qualsiasi processo che non termina da solo. Default: false."
        }
      },
      "required": [
        "command"
      ]
    }
  },
  {
    "name": "run_tests",
    "description": "Usa questo tool per eseguire test del progetto con timeout esteso (120s). Auto-rileva il comando test (package.json, Cargo.toml, pyproject.toml, etc.) o accetta comando esplicito. Massimo 7 esecuzioni per sessione. Non usare run_command per test: questo tool è specializzato.",
    "input_schema": {
      "type": "object",
      "properties": {
        "command": {
          "type": "string",
          "description": "Comando test esplicito (es. 'npm test', 'cargo test', 'pytest'). Se omesso, viene auto-rilevato dal progetto."
        },
        "working_dir": {
          "type": "string",
          "description": "Sottodirectory in cui eseguire (relativa alla root). Ometti per la root."
        },
        "timeout_secs": {
          "type": "integer",
          "description": "Timeout in secondi (default: 120, max: 300)."
        },
        "filter": {
          "type": "string",
          "description": "Filtro per eseguire solo test specifici (nome test, file, modulo). Viene aggiunto al comando del framework."
        }
      }
    }
  },
  {
    "name": "run_playwright_tests",
    "description": "Usa questo tool per eseguire i test Playwright end-to-end del progetto. A differenza di run_tests, questo tool legge automaticamente le porte assegnate da Nexus al progetto e imposta BASE_URL sul server corretto. Può avviare il dev server automaticamente se non è in esecuzione. Salva i risultati nel pannello Playwright dell'IDE.",
    "input_schema": {
      "type": "object",
      "properties": {
        "filter": {
          "type": "string",
          "description": "Filtro per eseguire solo alcuni test (es. 'auth' esegue tutti i file con 'auth' nel nome)"
        },
        "project": {
          "type": "string",
          "description": "Progetto Playwright (es. 'chromium', 'firefox', 'webkit'). Ometti per eseguire tutti i browser configurati."
        },
        "base_url": {
          "type": "string",
          "description": "URL base del server da testare (es. 'http://localhost:3000'). Se omesso, viene letto dalla porta allocata da Nexus per questo progetto."
        },
        "workers": {
          "type": "integer",
          "description": "Numero di worker paralleli (default: 1)"
        },
        "reporter": {
          "type": "string",
          "description": "Formato output: 'list' (default), 'line', 'dot'"
        },
        "timeout_secs": {
          "type": "integer",
          "description": "Timeout totale per l'intero run in secondi (default: 600, max: 900)"
        },
        "test_timeout_ms": {
          "type": "integer",
          "description": "Timeout per il singolo test in millisecondi (default: 10000 = 10s, max: 60000). Con backend non disponibile, 10s è sufficiente (connection refused < 1s). Aumentare a 30000 se i test richiedono caricamento lento o upload di file."
        },
        "auto_start_server": {
          "type": "boolean",
          "description": "Se true e il dev server non è raggiungibile, lo avvia automaticamente con run_service prima dei test (default: false)"
        },
        "config_path": {
          "type": "string",
          "description": "Directory relativa alla root del progetto contenente playwright.config.ts (es. 'app'). Se omesso, Nexus sceglie automaticamente la directory con piu' test spec tra radice e sottodirectory comuni."
        },
        "cleanup_stale_configs": {
          "type": "boolean",
          "description": "Se true (default), rimuove automaticamente config wrapper stale alla radice quando la suite reale e' in una sottodirectory (es. playwright.config.ts alla radice con 0 test mentre i veri test sono in app/e2e/)."
        }
      }
    }
  },
  {
    "name": "dispatcher_emit_event",
    "description": "Emette un evento custom sul dispatcher centrale del progetto. Usalo quando hai un'informazione semantica che non rientra negli eventi automatici (es. 'analisi statica completata', 'modello AI cambiato'). I pannelli sottoscritti al dispatcher la ricevono in tempo reale.",
    "input_schema": {
      "type": "object",
      "properties": {
        "kind": { "type": "string", "description": "Nome logico dell'evento (es. 'analysis_done', 'config_reloaded')" },
        "resource": { "type": "string", "description": "Categoria della risorsa (es. 'quality', 'deploy', 'custom')" },
        "payload": { "type": "object", "description": "Dati liberi associati all'evento (verranno serializzati come JSON)" }
      },
      "required": ["kind"]
    }
  },
  {
    "name": "dispatcher_post_notification",
    "description": "Invia una notifica (toast) all'utente nell'IDE. Usalo per comunicare eventi importanti che meritano l'attenzione immediata dell'utente. Rate-limited a 10/min per run.",
    "input_schema": {
      "type": "object",
      "properties": {
        "severity": { "type": "string", "enum": ["info", "success", "warning", "error"], "description": "Severita' del toast" },
        "message": { "type": "string", "description": "Testo del messaggio (in italiano)" },
        "panel": { "type": "string", "description": "Pannello opzionale da evidenziare (playwright|ports|problems|services|database|...)" },
        "ttl_ms": { "type": "integer", "description": "Durata visibilita' in ms (default frontend: 5000)" }
      },
      "required": ["severity", "message"]
    }
  },
  {
    "name": "dispatcher_set_flag",
    "description": "Imposta o aggiorna un flag globale del progetto, persistito in DB. I pannelli interessati ricevono FlagChanged e possono mostrare badge/banner. Le chiavi devono iniziare con uno dei prefissi consentiti: build_, test_, deploy_, custom_, feature_.",
    "input_schema": {
      "type": "object",
      "properties": {
        "key": { "type": "string", "description": "Nome del flag (es. 'build_in_progress', 'test_suite_running')" },
        "value": { "description": "Valore JSON (boolean, number, string o null per cancellare il flag)" }
      },
      "required": ["key"]
    }
  },
  {
    "name": "dispatcher_update_monitor",
    "description": "Aggiorna un widget monitor custom nel pannello Monitor. Usalo per esporre metriche real-time (progresso build, contatori, KPI). Valori in memoria, persistono solo finche' mcp-core e' vivo.",
    "input_schema": {
      "type": "object",
      "properties": {
        "monitor_id": { "type": "string", "description": "ID univoco del widget (es. 'build_progress', 'http_qps')" },
        "value": { "description": "Valore corrente (number, string o object)" },
        "label": { "type": "string", "description": "Etichetta human-readable opzionale" }
      },
      "required": ["monitor_id", "value"]
    }
  },
  {
    "name": "dispatcher_highlight_panel",
    "description": "Forza un flash animation su un pannello dell'IDE per attirare l'attenzione dell'utente (es. dopo aver completato una migrazione Database, evidenzia il pannello Database).",
    "input_schema": {
      "type": "object",
      "properties": {
        "panel": { "type": "string", "description": "Nome del pannello (playwright|ports|problems|services|database|monitor|files|git)" },
        "duration_ms": { "type": "integer", "description": "Durata del flash in ms (default 800, max 5000)" }
      },
      "required": ["panel"]
    }
  },
  {
    "name": "create_profile",
    "description": "Usa questo tool quando rilevi pattern di lavoro ricorrente o contesto tecnico specifico (sempre C#, sempre testing, sempre DevOps). Crea profilo dedicato che ottimizza risposte future. Parametri: name, description, instructions. Non usare per situazioni una tantum: i profili sono per pattern ricorrenti.",
    "input_schema": {
      "type": "object",
      "properties": {
        "name": {
          "type": "string",
          "description": "Nome breve del profilo (es. 'Sviluppatore C#', 'Code Reviewer', 'DevOps Engineer')"
        },
        "emoji": {
          "type": "string",
          "description": "Emoji rappresentativa del profilo (es. '🦀', '🔍', '⚙️'). Default: '🤖'"
        },
        "description": {
          "type": "string",
          "description": "Descrizione breve del profilo e del suo scopo"
        },
        "system_prompt": {
          "type": "string",
          "description": "Istruzioni specializzate per questo profilo. Devono descrivere expertise, stile di risposta, framework preferiti, best practice da seguire."
        },
        "default_provider": {
          "type": "string",
          "description": "Provider AI preferito per questo profilo ('anthropic', 'openai', 'google', 'auto'). Ometti per ereditare il globale."
        },
        "default_model": {
          "type": "string",
          "description": "Modello AI preferito per questo profilo. Ometti per ereditare il globale."
        },
        "default_automation": {
          "type": "string",
          "description": "Modalita' automazione preferita ('automatic', 'confirm', 'study'). Ometti per ereditare il globale."
        },
        "set_as_default": {
          "type": "boolean",
          "description": "Se true, imposta questo profilo come predefinito per l'utente."
        }
      },
      "required": [
        "name",
        "system_prompt"
      ]
    }
  },
  {
    "name": "update_profile",
    "description": "Usa questo tool per migliorare un profilo esistente aggiungendo expertise o affinando istruzioni. Parametri: profile_id, aggiornamenti. Non usare per creare nuovo profilo: usa create_profile per quello.",
    "input_schema": {
      "type": "object",
      "properties": {
        "profile_name": {
          "type": "string",
          "description": "Nome esatto del profilo da aggiornare (deve corrispondere esattamente)"
        },
        "system_prompt": {
          "type": "string",
          "description": "Nuovo system prompt aggiornato per il profilo"
        },
        "description": {
          "type": "string",
          "description": "Nuova descrizione del profilo"
        },
        "emoji": {
          "type": "string",
          "description": "Nuova emoji per il profilo"
        }
      },
      "required": [
        "profile_name"
      ]
    }
  },
  {
    "name": "set_sandbox_config",
    "description": "Configura l'ambiente sandbox Docker per questo progetto. Imposta limiti di risorse, modalità rete e variabili d'ambiente extra. I valori impostati persistono per tutte le esecuzioni future del progetto. Usa get_sandbox_config per vedere la configurazione corrente.",
    "input_schema": {
      "type": "object",
      "properties": {
        "memory_mb": {
          "type": "integer",
          "description": "Limite memoria container in MB. Es: 512, 1024, 2048, 4096. Default: 1024"
        },
        "cpus": {
          "type": "number",
          "description": "Limite CPU in core. Es: 0.5, 1.0, 2.0, 4.0. Default: 2.0"
        },
        "network_mode": {
          "type": "string",
          "enum": ["none", "bridge", "host"],
          "description": "Modalità rete Docker. none=isolamento totale (default sicuro), bridge=accesso internet (per npm install, curl), host=condivide rete host (per servizi che devono comunicare tra loro)"
        },
        "extra_env": {
          "type": "object",
          "description": "Variabili d'ambiente aggiuntive iniettate in ogni processo. Es: {\"NODE_ENV\": \"development\", \"PORT\": \"3000\"}"
        }
      }
    }
  },
  {
    "name": "get_sandbox_config",
    "description": "Legge la configurazione sandbox corrente del progetto (limiti risorse, rete, variabili d'ambiente). Usa set_sandbox_config per modificarla.",
    "input_schema": {
      "type": "object",
      "properties": {}
    }
  },
  {
    "name": "build_project_image",
    "description": "Builda l'immagine Docker del progetto dal suo Dockerfile. Necessario per far girare i servizi nel container isolato del progetto invece che sull'host. Il build può richiedere alcuni minuti. Una volta buildata, l'immagine viene usata automaticamente dai servizi avviati con run_service.",
    "input_schema": {
      "type": "object",
      "properties": {}
    }
  },
  {
    "name": "scan_code_quality",
    "description": "Usa questo tool per analizzare qualità del codice rilevando: complessità ciclomatica, funzioni lunghe, smells, TODO/FIXME, dead code, duplicati, vulnerabilità SQL, variabili non tipizzate, import inutilizzati, funzioni senza docs. Parametri: path (file o intero progetto). Non usare per testing funzionale: questo tool è statico analysis.",
    "input_schema": {
      "type": "object",
      "properties": {
        "file_path": {
          "type": "string",
          "description": "Path del file da analizzare relativo alla root del progetto. Se omesso, scansiona l'intero progetto e ritorna i top findings."
        },
        "severity_filter": {
          "type": "string",
          "enum": [
            "all",
            "high",
            "medium"
          ],
          "description": "Filtra per severità minima. Default: all"
        }
      }
    }
  },
  {
    "name": "search_codebase_semantic",
    "description": "Usa questo tool per cercare nel codebase usando similarità semantica con descrizioni in linguaggio naturale (es. 'card Dettagli richiesta', 'funzione calcola tasse', 'endpoint login'). Ideale per scoprire come è implementato qualcosa. Non usare per cercare nomi esatti: usa search_in_files per quelli.",
    "input_schema": {
      "type": "object",
      "properties": {
        "query": {
          "type": "string",
          "description": "Descrizione in linguaggio naturale di cosa stai cercando nel codebase"
        },
        "limit": {
          "type": "integer",
          "description": "Numero massimo di risultati (default: 8, max: 20)"
        }
      },
      "required": [
        "query"
      ]
    }
  },
  {
    "name": "search_file_semantic",
    "description": "Usa questo tool per trovare informazioni rilevanti dentro singoli file grandi (log, csv, config, output) usando TF-IDF semantico. Esempi: search_file_semantic('server.log', 'errori connessione'), search_file_semantic('config.yaml', 'database'). Non usare per file piccoli: usa read_file per quelli.",
    "input_schema": {
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "description": "Percorso del file da analizzare (relativo alla root del progetto o assoluto)"
        },
        "query": {
          "type": "string",
          "description": "Cosa stai cercando nel file, in linguaggio naturale"
        },
        "top_k": {
          "type": "integer",
          "description": "Numero massimo di sezioni rilevanti da restituire (default: 5, max: 10)"
        },
        "chunk_lines": {
          "type": "integer",
          "description": "Righe per chunk (default: 50). Usa valori più bassi per file strutturati, più alti per log."
        }
      },
      "required": [
        "path",
        "query"
      ]
    }
  },
  {
    "name": "recall_context",
    "description": "Cerca informazioni rilevanti nella conversazione corrente e nella memoria del progetto. Usa questo tool quando hai bisogno di recuperare dettagli discussi in precedenza, risultati di tool eseguiti prima che sono stati compressi, o contesto che non hai piu' nel tuo contesto attivo. Ideale dopo molte iterazioni quando il contesto e' stato troncato.",
    "input_schema": {
      "type": "object",
      "properties": {
        "query": {
          "type": "string",
          "description": "Descrizione in linguaggio naturale di cosa stai cercando (es. 'output del comando npm install', 'struttura del database', 'errore di autenticazione discusso prima')"
        },
        "source": {
          "type": "string",
          "enum": ["conversation", "project", "all"],
          "description": "Dove cercare: 'conversation' (turni conversazionali correnti), 'project' (contesto e documentazione del progetto), 'all' (entrambi). Default: 'all'"
        },
        "limit": {
          "type": "integer",
          "description": "Numero massimo di risultati (default: 5, max: 10)"
        }
      },
      "required": ["query"]
    }
  },
  {
    "name": "batch_analyze_code",
    "description": "Analizza in batch più file per documentazione, ottimizzazione o revisione del codice usando la Batch API Anthropic. Ideale per task non urgenti su molti file (documentazione automatica, analisi architetturale, review multi-file). La chiamata è asincrona: sottomette il batch e aspetta il completamento con polling. Usa questo tool per task che tollerano latenza di 1-5 minuti. NON usare per rispondere a domande interattive.",
    "input_schema": {
      "type": "object",
      "properties": {
        "files": {
          "type": "array",
          "description": "Lista di file da analizzare (massimo 20 file per batch)",
          "items": {
            "type": "object",
            "properties": {
              "path": { "type": "string", "description": "Percorso relativo del file" },
              "content": { "type": "string", "description": "Contenuto del file (lasciare vuoto per leggere automaticamente)" }
            },
            "required": ["path"]
          }
        },
        "task": {
          "type": "string",
          "enum": ["document", "optimize", "analyze"],
          "description": "Tipo di analisi: 'document' genera docstring/commenti, 'optimize' suggerisce ottimizzazioni, 'analyze' revisione architetturale e potenziali bug"
        }
      },
      "required": ["files", "task"]
    }
  },
  {
    "name": "nexus_mcp_tool_search",
    "description": "Cerca tra tutti i tool MCP disponibili (builtin Nexus + plugin esterni abilitati) usando ricerca semantica (Qdrant) o testuale (ILIKE fallback). I tool builtin (es. nexus_extract_figma_code, nexus_extract_pdf_text) sono restituiti con server_id=\"builtin\". Usa questo tool per scoprire quale tool invocare invece di ricevere tutte le definizioni: riduce drasticamente il payload token. Restituisce server_id, tool_name, description e input_schema.",
    "input_schema": {
      "type": "object",
      "properties": {
        "query": {
          "type": "string",
          "description": "Query in linguaggio naturale (es. 'esegui test cargo', 'leggi file', 'crea branch git')"
        },
        "limit": {
          "type": "integer",
          "description": "Numero massimo di risultati (default: 10, max: 50)"
        }
      },
      "required": ["query"]
    }
  },
  {
    "name": "nexus_mcp_tool_call",
    "description": "Invoca un tool MCP specifico usando server_id e tool_name ottenuti da nexus_mcp_tool_search. Per i tool builtin Nexus (es. nexus_extract_figma_code, nexus_extract_pdf_text, nexus_extract_docx_text, suggeriti da next_action_recommended di nexus_inspect_attachment) passa server_id=\"builtin\". Per i plugin MCP esterni passa l'UUID del server. Applica le policy di sicurezza del plugin. Non usare per tool builtin standard (read_file, git_*, ecc.) che sono già disponibili direttamente nel toolspec.",
    "input_schema": {
      "type": "object",
      "properties": {
        "server_id": {
          "type": "string",
          "description": "UUID del server MCP esterno, oppure la sentinella \"builtin\" per i tool interni Nexus (consigliato per i tool restituiti da next_action_recommended)"
        },
        "tool_name": {
          "type": "string",
          "description": "Nome originale del tool (es. 'list_issues', 'create_branch')"
        },
        "arguments": {
          "type": "object",
          "description": "Argomenti JSON per il tool secondo il suo input_schema"
        }
      },
      "required": ["server_id", "tool_name", "arguments"]
    }
  },
  {
    "name": "nexus_doc_generate",
    "description": "Genera un documento professionale .docx (Analisi Funzionale IEEE 830, Analisi Tecnica, Diagramma ER, Gestione Progetto, Release Notes). Costruisci le sezioni del documento e passale come content_json. Il documento viene registrato nel DB e appare nel pannello DOCUMENTI. DEVI usare questo tool per generare documenti — non usare write_file.",
    "input_schema": {
      "type": "object",
      "properties": {
        "doc_type": {
          "type": "string",
          "enum": ["functional_analysis", "technical_analysis", "er_diagram", "project_management", "release_notes"],
          "description": "Tipo di documento da generare"
        },
        "title": {
          "type": "string",
          "description": "Titolo del documento (opzionale, default da template)"
        },
        "content_json": {
          "type": "object",
          "description": "Contenuto strutturato: { sections: [{ title: string, content: string, subsections?: [...] }] }"
        }
      },
      "required": ["doc_type", "content_json"]
    }
  },
  {
    "name": "nexus_list_attachments",
    "description": "Lista gli allegati caricati dall'utente nella sessione chat corrente (o in un'altra sessione specificata). Restituisce per ciascun allegato: id, file_name, mime_type, size_bytes, kind, created_at. Usalo quando il blocco <allegati> nel prompt iniziale e' stato troncato per limite di dimensione (vedi <attachment_access>) e devi scoprire quali file ci sono. Dopo aver scelto un id, leggi il contenuto con nexus_read_attachment.",
    "input_schema": {
      "type": "object",
      "properties": {
        "session_id": {
          "type": "string",
          "description": "UUID della sessione chat. Ometti per usare la sessione corrente."
        }
      }
    }
  },
  {
    "name": "nexus_read_attachment",
    "description": "Legge il contenuto (o una porzione) di un allegato caricato dall'utente, identificato da attachment_id (ottenuto da nexus_list_attachments). Supporta offset+length per lettura streaming. Max 100KB per chiamata: per file piu' grandi chiama piu' volte con offset crescente. Encoding 'auto' decide testo o base64 in base al MIME; forza 'text' o 'base64' se necessario. Restituisce JSON con content, encoding, offset, length, total_size, truncated.",
    "input_schema": {
      "type": "object",
      "properties": {
        "attachment_id": {
          "type": "string",
          "description": "UUID dell'allegato (da nexus_list_attachments). Obbligatorio."
        },
        "encoding": {
          "type": "string",
          "enum": ["auto", "text", "base64"],
          "description": "Forma del contenuto restituito. Default 'auto' (text per mime testuali, altrimenti base64)."
        },
        "offset": {
          "type": "integer",
          "description": "Byte offset da cui iniziare la lettura (default 0)."
        },
        "length": {
          "type": "integer",
          "description": "Byte massimi da leggere (default 102400, hard cap 102400)."
        }
      },
      "required": ["attachment_id"]
    }
  },
  {
    "name": "nexus_inspect_attachment",
    "description": "Magic-byte detection del formato reale di un allegato (.make/.dat/.bin/.fig). Ritorna kind+mime+extraction_tools+next_action_recommended. Chiama SEMPRE prima di leggere binari opachi: poi usa esattamente il tool suggerito in next_action_recommended.",
    "input_schema": {
      "type": "object",
      "properties": {
        "attachment_id": {
          "type": "string",
          "description": "UUID dell'allegato (ottenuto da nexus_list_attachments)."
        }
      },
      "required": ["attachment_id"]
    }
  },
  {
    "name": "nexus_list_archive_entries",
    "description": "Lista TUTTE le entries di un archivio ZIP, TAR o TAR.GZ allegato. Rileva il formato automaticamente dai magic bytes. Restituisce nome, dimensione, dimensione compressa, flag is_dir per ogni entry. Nessun limite sul numero di entry: l'elenco e' sempre completo.",
    "input_schema": {
      "type": "object",
      "properties": {
        "attachment_id": {
          "type": "string",
          "description": "UUID dell'allegato."
        }
      },
      "required": ["attachment_id"]
    }
  },
  {
    "name": "nexus_read_archive_entry",
    "description": "Estrae e legge il contenuto INTEGRALE di una singola entry da un archivio (ZIP/TAR/TAR.GZ). Nessun cap sui byte letti: la entry e' restituita per intero. encoding 'auto' decide text/base64 in base ai byte effettivamente estratti.",
    "input_schema": {
      "type": "object",
      "properties": {
        "attachment_id": {"type": "string", "description": "UUID dell'allegato archivio."},
        "entry_path": {"type": "string", "description": "Percorso esatto della entry dentro l'archivio (es. 'word/document.xml', 'src/main.rs')."},
        "encoding": {"type": "string", "enum": ["auto", "text", "base64"], "description": "Forma del contenuto. Default 'auto'."}
      },
      "required": ["attachment_id", "entry_path"]
    }
  },
  {
    "name": "nexus_extract_pdf_text",
    "description": "Estrae il testo INTEGRALE da un allegato PDF, opzionalmente limitato a un range di pagine. Restituisce {total_pages, pages_extracted, text}. Nessun cap sul testo estratto: il contenuto delle pagine richieste e' completo. Se il PDF e' scansionato (immagini, niente testo) ritorna is_scanned_pdf=true e un hint per richiedere OCR.",
    "input_schema": {
      "type": "object",
      "properties": {
        "attachment_id": {"type": "string"},
        "page_start": {"type": "integer", "description": "Pagina di inizio 1-based (default 1)."},
        "page_end": {"type": "integer", "description": "Pagina di fine inclusa (default ultima pagina)."}
      },
      "required": ["attachment_id"]
    }
  },
  {
    "name": "nexus_extract_docx_text",
    "description": "Estrae il testo dei paragrafi da un allegato DOCX (Word). Parser interno: spacchetta lo ZIP, parsea word/document.xml. Restituisce {paragraphs_count, text} con paragrafi separati da doppia newline.",
    "input_schema": {
      "type": "object",
      "properties": {
        "attachment_id": {"type": "string"}
      },
      "required": ["attachment_id"]
    }
  },
  {
    "name": "nexus_extract_xlsx_data",
    "description": "Estrae TUTTI i dati tabellari da un allegato XLSX (Excel). Restituisce array di righe (ognuna array di celle stringa). Default primo sheet (sheet1). Nessun cap sul numero di righe: il foglio e' estratto per intero.",
    "input_schema": {
      "type": "object",
      "properties": {
        "attachment_id": {"type": "string"},
        "sheet_name": {"type": "string", "description": "Nome del foglio (es. 'sheet1', 'sheet2'). Default 'sheet1'."}
      },
      "required": ["attachment_id"]
    }
  },
  {
    "name": "nexus_extract_figma_structure",
    "description": "MVP estrazione file Figma (.fig). I file Figma sono archivi che contengono un payload binario proprietario canvas.fig. Questo tool estrae il payload, ne restituisce dimensione + le stringhe ASCII leggibili (utili per inferire nomi di layer/stili) + un hint per ottenere la struttura completa via Figma API o plugin 'Figma to Code'. Per ora NON ricostruisce frame/componenti.",
    "input_schema": {
      "type": "object",
      "properties": {
        "attachment_id": {"type": "string"}
      },
      "required": ["attachment_id"]
    }
  },
  {
    "name": "nexus_extract_figma_code",
    "description": "Estrae il code-snapshot React/TS/Tailwind da un .make Figma e scrive i file su disco (default figma_export/). Ritorna manifest con files_written, entrypoints, detected_dependencies. Usa quando inspector segnala fast_apply.",
    "input_schema": {
      "type": "object",
      "properties": {
        "attachment_id": {"type": "string", "description": "UUID dell'allegato .make."},
        "target_subdir": {"type": "string", "description": "Sottocartella relativa alla project_root dove scrivere i file estratti. Default 'figma_export'."}
      },
      "required": ["attachment_id"]
    }
  },
  {
    "name": "nexus_install_shadcn_components",
    "description": "Crea stub TSX dei componenti shadcn/ui piu' usati (button, input, label, card, alert, tabs, table, badge, separator, sonner, dialog, dropdown-menu, select, popover, textarea) senza richiedere 'npx shadcn add'. Risolve il problema dei loop di errore quando il modello tenta shadcn-ui (rebrand a shadcn), peer dep rotte, o cache npx corrotta. Gli stub usano Tailwind classes e bastano a far buildare un'app React+TS+Vite scaffolded da Figma. Per UI ricca, sostituiscili poi con shadcn ufficiale.",
    "input_schema": {
      "type": "object",
      "properties": {
        "components": {
          "type": "array",
          "items": {"type": "string"},
          "description": "Lista nomi componenti da creare (es. ['button','input','card']). Se omesso, installa il set base: button/input/label/card/alert/tabs/sonner."
        },
        "target_dir": {
          "type": "string",
          "description": "Path relativo alla project root dove creare gli stub. Default: 'src/components/ui'. Per progetti con struttura figma_export usa 'figma_export/src/app/components/ui'."
        },
        "overwrite": {
          "type": "boolean",
          "description": "Se true, sovrascrive file esistenti. Default false (skip se esiste)."
        }
      },
      "required": []
    }
  },
  {
    "name": "nexus_dev_server_diagnose",
    "description": "Auto-healing per loop iterativo dev server. Legge il log/output di un dev server (vite/next/cargo/python) e ritorna findings strutturati [{category, suggested_fix_action, confidence}] basati su pattern DB-driven (nexus_dev_diagnostics, mig 0232). Usalo DOPO 'npm start' che fallisce: invece di leggere 200 righe di log manualmente, ottieni la lista di fix concreti da applicare in ordine di confidence. Estensibile via INSERT in nexus_dev_diagnostics, no deploy. Output: ogni finding ha suggested_fix_action = {type: 'run_command'|'write_file'|'edit_file'|'invoke_tool', ...} pronto da eseguire.",
    "input_schema": {
      "type": "object",
      "properties": {
        "log_path": {
          "type": "string",
          "description": "Path file log da scansionare (es. '/tmp/bb-app.log'). Relativo a project root oppure assoluto. Letti ultimi 200KB."
        },
        "log": {
          "type": "string",
          "description": "ALTERNATIVA a log_path: stringa di log inline (es. da read_service_output)."
        },
        "port": {
          "type": "integer",
          "description": "Porta del dev server (per nota nel risultato, non usata per matching)."
        }
      },
      "required": []
    }
  },
  {
    "name": "nexus_verify_scaffold",
    "description": "Verifica completezza di un progetto scaffolded (Vite+React+TS) PRIMA del primo 'npm start'. Controlla che esistano index.html / vite.config.ts / src/main.tsx, che package.json abbia uno script dev/start, e che gli import in main.tsx puntino a file esistenti o pkg npm installati. Ritorna {ok, missing_files, inconsistent_imports, package_json_issues, suggested_fixes:[{type:'write_file'|'edit_file'|'run_command', ...}]}. Usalo DOPO nexus_extract_figma_code per evitare il primo 'npm start' fallito.",
    "input_schema": {
      "type": "object",
      "properties": {
        "target_dir": {
          "type": "string",
          "description": "Path relativo alla project root del progetto scaffolded. Default: '.' (root). Per Beauty-Book/figma_export usa 'figma_export'."
        }
      },
      "required": []
    }
  },
  {
    "name": "nexus_db_query",
    "description": "Esegue SQL sul DB applicativo del progetto (SELECT/INSERT/UPDATE/DELETE/DDL). SELECT ritorna {columns,rows,row_count}; mutazioni ritornano {rows_affected}. Usa params=$1,$2 con cast ($1::int) per tipi non testo. Sostituisce psql/run_command.",
    "input_schema": {
      "type": "object",
      "properties": {
        "sql": {"type": "string", "description": "Statement SQL. Una sola statement per chiamata."},
        "params": {"type": "array", "items": {}, "description": "Parametri posizionali per $1,$2,... Bindati come testo; usa cast nel SQL per tipi non-testo."},
        "max_rows": {"type": "integer", "description": "Max righe ritornate da una SELECT (default e cap: 1000)."}
      },
      "required": ["sql"]
    }
  },
  {
    "name": "nexus_db_tables",
    "description": "Lista le tabelle del DATABASE APPLICATIVO del progetto (schema public di default) con stima righe per tabella. Usalo per orientarti prima di scrivere query: scopri quali tabelle esistono. Se il DB e' vuoto (0 tabelle), creale con nexus_db_query CREATE TABLE.",
    "input_schema": {
      "type": "object",
      "properties": {
        "schema": {"type": "string", "description": "Schema da listare (default 'public')."}
      },
      "required": []
    }
  },
  {
    "name": "nexus_db_describe",
    "description": "Mostra colonne (nome, tipo, nullable, default), indici e vincoli di una tabella del DB applicativo del progetto. Usalo prima di una INSERT/UPDATE per conoscere le colonne esatte e i loro tipi.",
    "input_schema": {
      "type": "object",
      "properties": {
        "table": {"type": "string", "description": "Nome tabella."},
        "schema": {"type": "string", "description": "Schema (default 'public')."}
      },
      "required": ["table"]
    }
  },
  {
    "name": "nexus_describe_image_attachment",
    "description": "Descrive un'immagine allegata alla chat usando un modello vision. Restituisce description testuale e ocr_text (se l'immagine contiene testo leggibile). Usalo quando l'inspector ha rilevato kind=image_* e devi capire il contenuto visivo (mockup UI, screenshot, foto, diagrammi).",
    "input_schema": {
      "type": "object",
      "properties": {
        "attachment_id": {"type": "string"},
        "question": {"type": "string", "description": "Domanda opzionale al modello vision (es. 'estrai i testi UI', 'descrivi il layout')."}
      },
      "required": ["attachment_id"]
    }
  },
  {
    "name": "nexus_visual_compare",
    "description": "Confronta screenshot dell'app locale con design Figma via vision model. Ritorna similarity_score (0-100), differences[], screenshot_path. Usa per iterare layout/stile fino a soglia (default 85).",
    "input_schema": {
      "type": "object",
      "properties": {
        "url": {"type": "string", "description": "URL locale dell'app avviata da screenshottare (es. 'http://localhost:29348/' o una route specifica). Obbligatorio."},
        "reference": {"type": "string", "description": "attachment_id del design di riferimento: se e' un .make Figma viene usato il suo thumbnail.png, se e' un'immagine viene usata direttamente. Ometti per ottenere solo lo screenshot senza confronto."},
        "viewport": {"type": "object", "description": "Dimensioni viewport {width, height}. Default 1280x800 (configurabile in settings agent.visual_compare.viewport_*).", "properties": {"width": {"type": "integer"}, "height": {"type": "integer"}}},
        "wait_ms": {"type": "integer", "description": "Attesa (ms) dopo il load prima dello scatto. Default da settings (agent.visual_compare.wait_ms)."}
      },
      "required": ["url"]
    }
  },
  {
    "name": "knowledge_search",
    "description": "Cerca note rilevanti nella Knowledge Base del progetto corrente. Usalo quando devi verificare se una richiesta simile e' gia' stata affrontata, se ci sono decisioni precedenti, o se ci sono requirement/feature gia' documentati. Restituisce top-K note ordinate per rilevanza semantica.",
    "input_schema": {
      "type": "object",
      "properties": {
        "query": {
          "type": "string",
          "description": "Testo da cercare (es. 'autenticazione OAuth Google', 'fix bug timezone'). Max 2000 char."
        },
        "top_k": {
          "type": "integer",
          "description": "Numero massimo di hit (default 5, max 20)"
        },
        "min_score": {
          "type": "number",
          "description": "Soglia minima similarita' 0-1 (default 0.4)"
        }
      },
      "required": ["query"]
    }
  },
  {
    "name": "code_doc",
    "description": "Restituisce la documentazione (Code Wiki) di un file specifico: scopo, componenti, dipendenze e call-graph. Usalo PRIMA di modificare o estendere un file per sapere cosa fa gia', evitare di re-implementarlo e non reintrodurre errori. Diretto (per path), piu' preciso di knowledge_search quando conosci gia' il file.",
    "input_schema": {
      "type": "object",
      "properties": {
        "file_path": {
          "type": "string",
          "description": "Path del file (relativo alla root del progetto, es. 'src/auth/login.ts')."
        }
      },
      "required": ["file_path"]
    }
  },
  {
    "name": "knowledge_get_note",
    "description": "Recupera il body COMPLETO di una nota della KB. Usalo dopo knowledge_search quando lo snippet non basta e serve il testo completo della nota. Aggiorna access_count della nota.",
    "input_schema": {
      "type": "object",
      "properties": {
        "note_id": {
          "type": "string",
          "description": "UUID della nota (dal risultato di knowledge_search)"
        }
      },
      "required": ["note_id"]
    }
  },
  {
    "name": "knowledge_create_note",
    "description": "Crea una nuova nota FUNZIONALE/CONCETTUALE nella KB del progetto. Usalo per documentare decisioni di design, requirement, feature, user story, dominio. Non usarlo per le chat utente (gia' auto-create). La nota viene indicizzata in Qdrant per ricerche future.",
    "input_schema": {
      "type": "object",
      "properties": {
        "title": {
          "type": "string",
          "description": "Titolo breve (1-200 char)"
        },
        "body_md": {
          "type": "string",
          "description": "Contenuto Markdown della nota"
        },
        "intent": {
          "type": "string",
          "enum": ["feature", "requirement", "decision", "domain", "user_story", "architecture", "fix", "refactor", "docs", "other"],
          "description": "Categoria semantica della nota (default 'feature')"
        },
        "tags": {
          "type": "array",
          "items": { "type": "string" },
          "description": "Tag opzionali per facilitare ricerca"
        },
        "file_paths": {
          "type": "array",
          "items": { "type": "string" },
          "description": "Path file correlati (opzionale)"
        }
      },
      "required": ["title", "body_md"]
    }
  },
  {
    "name": "knowledge_get_links",
    "description": "Restituisce i link entranti e uscenti di una nota della KB (relazioni del grafo: followup/correction/refinement/duplicate/blocks/blocked_by/relates). Usalo per capire da cosa dipende una nota o cosa la referenzia. Le note off_topic sono escluse.",
    "input_schema": {
      "type": "object",
      "properties": {
        "note_id": {"type": "string", "description": "UUID della nota di cui leggere i link"}
      },
      "required": ["note_id"]
    }
  },
  {
    "name": "knowledge_get_subgraph",
    "description": "Estrae un sottografo della KB del progetto da un seed (testo 'query' che trova le note radice per similarita', oppure 'note_id' esplicito) espandendo i link fino a 'depth'. Filtra per 'rel_types' (per le sole dipendenze di esecuzione passa [\"blocks\",\"blocked_by\"]). Restituisce nodi e archi. E' la base per derivare l'ordine delle azioni dal grafo.",
    "input_schema": {
      "type": "object",
      "properties": {
        "query": {"type": "string", "description": "Testo seed: trova le note radice per similarita' semantica. Alternativo a note_id."},
        "note_id": {"type": "string", "description": "UUID di una nota radice. Alternativo a query."},
        "rel_types": {"type": "array", "items": {"type": "string", "enum": ["followup","correction","refinement","duplicate","blocks","blocked_by","relates"]}, "description": "Filtra le relazioni. Default: tutte. Per le dipendenze di esecuzione usa [\"blocks\",\"blocked_by\"]."},
        "depth": {"type": "integer", "description": "Profondita' espansione BFS (default 2, max 4)"},
        "max_nodes": {"type": "integer", "description": "Numero massimo di nodi (default 30, max 100)"}
      }
    }
  },
  {
    "name": "knowledge_create_link",
    "description": "Crea o aggiorna un link diretto tra due note della KB. Usa 'blocks'/'blocked_by' per dipendenze di esecuzione (A blocked_by B => B prima di A), 'duplicate' per richieste gia' elaborate, 'correction' per contraddizioni, 'refinement' per ampliamenti, 'relates' per contesto correlato. Idempotente sulla tripla (from,to,rel_type).",
    "input_schema": {
      "type": "object",
      "properties": {
        "from_note_id": {"type": "string", "description": "UUID nota sorgente"},
        "to_note_id": {"type": "string", "description": "UUID nota destinazione"},
        "rel_type": {"type": "string", "enum": ["followup","correction","refinement","duplicate","blocks","blocked_by","relates"], "description": "Tipo di relazione"},
        "confidence": {"type": "number", "description": "Confidenza 0-1 (default 1.0)"}
      },
      "required": ["from_note_id", "to_note_id", "rel_type"]
    }
  },
  {
    "name": "knowledge_set_relevance",
    "description": "Marca una nota come on/off-topic rispetto allo scopo del progetto. Una nota off_topic resta in KB ma viene esclusa dal grafo, dal RAG e dal coordinamento delle azioni. Usalo per togliere dal grafo le richieste non pertinenti al progetto.",
    "input_schema": {
      "type": "object",
      "properties": {
        "note_id": {"type": "string", "description": "UUID della nota"},
        "off_topic": {"type": "boolean", "description": "true = fuori tema (esclusa dal grafo), false = pertinente"},
        "relevance_score": {"type": "number", "description": "Punteggio di pertinenza 0-1 (opzionale)"}
      },
      "required": ["note_id", "off_topic"]
    }
  },
  {
    "name": "knowledge_import_graph",
    "description": "Importa un grafo esterno (architettura, dipendenze moduli, knowledge di dominio) nella KB del progetto. I nodi diventano note, gli archi diventano relazioni: le dipendenze diventano blocks/blocked_by e alimentano il coordinamento delle azioni (DAG). Usalo per integrare grafi prodotti da altri strumenti.",
    "input_schema": {
      "type": "object",
      "properties": {
        "format": {"type": "string", "enum": ["json", "mermaid", "dot"], "description": "Formato: json (node-link {nodes,edges}), mermaid (flowchart), dot (graphviz)"},
        "content": {"type": "string", "description": "Contenuto del grafo nel formato indicato"},
        "source_id": {"type": "string", "description": "Identificatore della sorgente (opzionale, per tracciare l'origine)"}
      },
      "required": ["format", "content"]
    }
  }
  ,
  {
    "name": "nexus_search_semantic",
    "description": "Cerca semanticamente nel contesto del progetto: allegati indicizzati, knowledge base, chat history passate, tool result cached. Usalo per recuperare informazioni rilevanti senza dover ri-leggere file interi. Restituisce chunk testuali ordinati per score di similarita' coseno.",
    "input_schema": {
      "type": "object",
      "properties": {
        "query": {"type": "string", "description": "Testo da cercare (es. 'cosa fa il bottone Send nel chat input?'). Max 2000 char."},
        "source_kinds": {"type": "array", "items": {"type": "string", "enum": ["attachment", "kb", "chat_history", "tool_result", "code"]}, "description": "Filtra per tipologia. Default: tutte tranne 'code'."},
        "top_k": {"type": "integer", "description": "Numero hit (default da settings agent.rag.top_k_default, max 100)."},
        "filter_attachment_id": {"type": "string", "description": "Restringe a un singolo attachment_id."},
        "filter_session_id": {"type": "string", "description": "Restringe a una session_id (rilevante per chat_history)."}
      },
      "required": ["query"]
    }
  }
]"#;
