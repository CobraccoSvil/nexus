//! Definizioni dei tool disponibili all'agente e funzioni di esecuzione.
//!
//! I tool sono sicuri: nessuna esecuzione di shell arbitraria.
//! Tutte le operazioni file sono vincolate alla root del progetto.
//!
//! Modulo splittato per dominio:
//! - `files`   — operazioni su filesystem (read/write/edit/delete/list/search)
//! - `git`     — comandi Git
//! - `service` — gestione processi long-running e build immagine progetto
//! - `sandbox` — configurazione sandbox del progetto
//! - `command` — esecuzione comandi shell e test runner

use std::{path::{Path, PathBuf}, sync::Arc};

use mcp_quality;
use mcp_db;

use serde_json::Value;
use sqlx::{PgPool, Row};
use tokio::process::Command;
use uuid::Uuid;

use crate::{projects::{resolve_relative_path, run_git_command}, vector_memory};

pub(crate) mod files;
pub(crate) mod git;
pub(crate) mod service;
pub(crate) mod sandbox;
pub(crate) mod command;
pub(crate) mod testing;

// Re-export per uso interno crate (tool_run_tests è chiamato da agent_loop, in teoria).
pub(crate) use command::tool_run_tests;

/// Numero massimo di righe restituite da read_file prima di troncare.
pub(super) const READ_FILE_MAX_LINES: usize = 300;
/// Numero massimo di righe leggibili con read_file_lines in una singola chiamata.
pub(super) const READ_FILE_LINES_MAX: usize = 400;

/// File e pattern che l'agente non può mai modificare, indipendentemente dai permessi.
/// Proteggono secrets, configurazioni ambiente e il binario in produzione.
pub(super) const PROTECTED_PATTERNS: &[&str] = &[
    ".env",
    ".env.local",
    ".env.production",
    ".env.staging",
    ".env.development",
    "nexus.env",          // env specifico di Nexus
    "secrets",            // qualsiasi file con "secrets" nel nome
    "credentials",
    "id_rsa",
    "id_ed25519",
    ".pem",
    ".key",
    "Cargo.lock",         // non modificare il lockfile manualmente
    "pnpm-lock.yaml",
];

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
    "name": "dispatch_subtask",
    "description": "Usa questo tool per delegare sottotask a agenti paralleli quando decomponibile in lavoro indipendente (es. refactoring moduli separati, update test+docs insieme). Richiede modalità agenti paralleli abilitata. Non usare per task sequenziali che dipendono l'uno dall'altro.",
    "input_schema": {
      "type": "object",
      "properties": {
        "task": {
          "type": "string",
          "description": "Descrizione completa e autonoma del sotto-task da eseguire (l'agente figlio non ha accesso alla conversazione corrente)"
        },
        "context": {
          "type": "string",
          "description": "Contesto aggiuntivo utile all'agente figlio: file coinvolti, vincoli, dipendenze da rispettare"
        }
      },
      "required": [
        "task"
      ]
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
          "description": "Etichetta breve per identificare il servizio (es. 'Backend .NET', 'Frontend React'). Apparira' nel pannello Output."
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
        }
      }
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
    "description": "Cerca tra tutti i tool MCP disponibili (builtin + plugin abilitati) usando ricerca semantica (Qdrant) o testuale (ILIKE fallback). Usa questo tool per scoprire quale tool invocare invece di ricevere tutte le definizioni: riduce drasticamente il payload token. Restituisce server_id, tool_name, description e input_schema.",
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
    "description": "Invoca un tool MCP specifico usando server_id e tool_name ottenuti da nexus_mcp_tool_search. Applica le policy di sicurezza del plugin. Non usare per tool builtin standard (read_file, git_*, ecc.) che sono già disponibili direttamente.",
    "input_schema": {
      "type": "object",
      "properties": {
        "server_id": {
          "type": "string",
          "description": "UUID del server MCP (ottenuto da nexus_mcp_tool_search)"
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
  }
]"#;

/// Contesto necessario all'esecuzione dei tool.
#[derive(Debug, Clone)]
pub struct AgentToolContext {
    /// Root assoluta del progetto (path-traversal-safe).
    pub root_path: PathBuf,
    pub user_id: Uuid,
    pub is_git_repo: bool,
    pub can_write: bool,
    pub project_id: Uuid,
    pub session_id: Option<Uuid>,
    pub db: Arc<PgPool>,
    /// ID del run padre (per agenti figlio lanciati da dispatch_subtask).
    pub parent_run_id: Option<Uuid>,
    /// Canali agente (DashMap) per registrare i run figlio.
    pub agent_channels: crate::AgentChannels,
    /// Client Neural Core per i run figlio.
    pub neural: crate::orchestrator::NeuralCoreClient,
    /// Automation mode corrente (ereditata dai run figlio).
    pub automation_mode: crate::orchestrator::AutomationMode,
    /// Terminali IDE connessi per utente/progetto.
    pub terminal_consumers: crate::TerminalConsumers,
    /// Pattern long-running caricati dal DB (pre-fetched all'inizio del run).
    pub long_running_patterns: Vec<String>,
    /// Cache template prompt (condivisa con AppState).
    pub template_cache: crate::prompt_templates::TemplateCache,
    /// Ruolo utente corrente ("admin" | "editor" | "viewer") — usato dai tool nexus_builtin.
    pub user_role: String,
    /// Stato atomico dipendenze (Qdrant, embedder). Se down, i tool vettoriali
    /// ritornano subito un messaggio informativo invece di aspettare il timeout.
    pub dependency_status: crate::task_watchdog::DependencyStatusRef,
}

/// Ritorna true se il tool modifica lo stato del filesystem o del repository.
pub fn is_mutating_tool(name: &str) -> bool {
    matches!(
        name,
        "write_file" | "edit_file" | "delete_file" | "rename_file"
            | "git_stage" | "git_commit" | "git_push" | "git_pull"
    )
    // run_in_terminal è intenzionalmente NON mutante: il comando appare nel terminale
    // ma l'agente non ha visibilità dell'output, quindi non blocca la conferma.
}

#[allow(dead_code)]
pub(super) fn shell_escape(s: &str) -> String {
    // Wraps in single quotes, escaping existing single quotes
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Controlla se il comando corrisponde a uno dei pattern long-running caricati dal DB.
/// Ogni pattern è una sequenza di token (es. "npm run dev") che viene cercata
/// come sottosequenza contigua nei token del comando.
pub(super) fn looks_like_long_running_command(command: &str, patterns: &[String]) -> bool {
    let lower = command.to_lowercase();
    let normalized = lower
        .replace("&&", " ")
        .replace("||", " ")
        .replace(';', " ")
        .replace('|', " ")
        .replace('(', " ")
        .replace(')', " ");
    let tokens: Vec<&str> = normalized.split_whitespace().collect();

    for pattern in patterns {
        let pat_tokens: Vec<&str> = pattern.split_whitespace().collect();
        if pat_tokens.is_empty() {
            continue;
        }
        // Pattern singolo token: match anche come primo token (es. "vite", "nodemon", "uvicorn")
        if pat_tokens.len() == 1 {
            if tokens.contains(&pat_tokens[0].to_lowercase().as_str())
                || tokens.first().copied() == Some(pat_tokens[0])
            {
                return true;
            }
            // Match case-insensitive su tutti i token
            let pat_lower = pat_tokens[0].to_lowercase();
            if tokens.iter().any(|t| *t == pat_lower.as_str()) {
                return true;
            }
        } else {
            // Multi-token: match come sottosequenza contigua
            let pat_lower: Vec<String> = pat_tokens.iter().map(|t| t.to_lowercase()).collect();
            let pat_refs: Vec<&str> = pat_lower.iter().map(|s| s.as_str()).collect();
            if tokens.len() >= pat_refs.len()
                && tokens.windows(pat_refs.len()).any(|w| w == pat_refs.as_slice())
            {
                return true;
            }
        }
    }
    false
}

/// Estrae una mappa strutturale del file: funzioni, classi, componenti con numero di riga.
/// Supporta Rust, TypeScript/JavaScript, Python, C#, Go.
/// Usa corrispondenza su prefisso di parola chiave — nessuna regex, O(n) per riga.
pub(super) fn extract_file_structure(content: &str) -> Vec<(usize, String)> {
    let mut entries: Vec<(usize, String)> = Vec::new();

    for (line_idx, raw_line) in content.lines().enumerate() {
        let line_num = line_idx + 1;
        let line = raw_line.trim();

        // Salta righe vuote e commenti
        if line.is_empty() || line.starts_with("//") || line.starts_with("/*") || line.starts_with('#') {
            continue;
        }

        // Helper: estrai nome identificatore dopo una keyword
        let ident_after = |s: &str, kw: &str| -> Option<String> {
            let rest = s.strip_prefix(kw)?.trim_start();
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if name.is_empty() { None } else { Some(name) }
        };

        // Normalizza spazi multipli per matching keyword composte
        let normalized = line.split_whitespace().collect::<Vec<_>>().join(" ");

        // TypeScript/JavaScript — export function, async function, function
        if let Some(name) = ["export async function ", "export function ", "async function ", "function "]
            .iter().find_map(|kw| ident_after(&normalized, kw))
        {
            entries.push((line_num, format!("fn {name}")));
            continue;
        }

        // TypeScript/JavaScript — export const X = (...) => / = async (
        if normalized.starts_with("export const ") || normalized.starts_with("const ") {
            // Solo se è assegnazione a funzione/arrow
            if normalized.contains("= (") || normalized.contains("= async (") || normalized.contains(": React.") || normalized.contains("FC =") {
                if let Some(name) = ident_after(&normalized, "export const ")
                    .or_else(|| ident_after(&normalized, "const "))
                {
                    entries.push((line_num, format!("const {name}")));
                    continue;
                }
            }
        }

        // class (TS/JS/Python/C#)
        if let Some(name) = ["export default class ", "export class ", "class "]
            .iter().find_map(|kw| ident_after(&normalized, kw))
        {
            entries.push((line_num, format!("class {name}")));
            continue;
        }

        // Rust — pub async fn, pub fn, async fn, fn
        if let Some(name) = ["pub async fn ", "pub fn ", "async fn ", "fn "]
            .iter().find_map(|kw| ident_after(&normalized, kw))
        {
            entries.push((line_num, format!("fn {name}")));
            continue;
        }

        // Rust — impl, struct, enum
        if let Some(name) = ident_after(&normalized, "impl ") {
            entries.push((line_num, format!("impl {name}")));
            continue;
        }
        if let Some(name) = ["pub struct ", "struct "].iter().find_map(|kw| ident_after(&normalized, kw)) {
            entries.push((line_num, format!("struct {name}")));
            continue;
        }
        if let Some(name) = ["pub enum ", "enum "].iter().find_map(|kw| ident_after(&normalized, kw)) {
            entries.push((line_num, format!("enum {name}")));
            continue;
        }

        // Python — def, async def
        if let Some(name) = ["async def ", "def "].iter().find_map(|kw| ident_after(&normalized, kw)) {
            entries.push((line_num, format!("def {name}")));
            continue;
        }

        // C# — public/private/protected method or class
        if normalized.starts_with("public ") || normalized.starts_with("private ") || normalized.starts_with("protected ") {
            if normalized.contains(" class ") || normalized.contains(" interface ") || normalized.contains(" enum ") {
                let short: String = normalized.chars().take(60).collect();
                entries.push((line_num, format!("class {short}")));
                continue;
            }
            // method: ha parentesi aperta e non è una property semplice
            if normalized.contains('(') && !normalized.ends_with(';') {
                let short: String = normalized.chars().take(60).collect();
                entries.push((line_num, format!("method {short}")));
                continue;
            }
        }
    }

    entries
}

/// Ritorna true se il path è protetto e non deve essere modificato dall'agente.
pub(super) fn is_protected_path(path_str: &str) -> Option<&'static str> {
    let lower = path_str.to_lowercase();
    // Controlla nome file esatto o pattern nel path
    for pattern in PROTECTED_PATTERNS {
        let pat_lower = pattern.to_lowercase();
        // Match esatto del nome file o estensione
        if lower.ends_with(&pat_lower)
            || lower.contains(&format!("/{}", pat_lower))
            || lower.contains(&format!("\\{}", pat_lower))
            || lower == pat_lower
        {
            return Some(pattern);
        }
    }
    None
}

pub(super) fn format_process_output(info: &crate::agent_processes::ProcessOutput) -> String {
    let mut msg = format!(
        "Processo: {} (pid: {}, status: {}",
        info.command,
        info.pid.map(|p| p.to_string()).unwrap_or_else(|| "?".into()),
        info.status,
    );
    if let Some(code) = info.exit_code {
        msg.push_str(&format!(", exit_code: {}", code));
    }
    msg.push_str(")\n");
    if !info.stdout.is_empty() {
        msg.push_str(&format!("\nSTDOUT:\n{}", info.stdout));
    }
    if !info.stderr.is_empty() {
        msg.push_str(&format!("\nSTDERR:\n{}", info.stderr));
    }
    if info.stdout.is_empty() && info.stderr.is_empty() {
        msg.push_str("\n(Nessun output disponibile)");
    }
    msg
}

/// Classifica l'errore di un comando shell e restituisce un suggerimento diagnostico.
pub(super) fn classify_command_error(exit_code: i32, stderr: &str, stdout: &str) -> &'static str {
    let err = stderr.to_lowercase();
    let out = stdout.to_lowercase();
    let combined = format!("{err} {out}");
    if exit_code == 127 || combined.contains("command not found") || combined.contains("not found") {
        return "comando non trovato — verifica il nome esatto o installa il pacchetto mancante con run_command(\"sudo apt-get install -y <pacchetto>\")";
    }
    if combined.contains("permission denied") || combined.contains("operation not permitted") {
        return "permesso negato — prova ad aggiungere `sudo` oppure verifica i permessi del file con run_command(\"ls -la <percorso>\")";
    }
    if combined.contains("no such file") || combined.contains("cannot find") || combined.contains("no existe") {
        return "file o directory non trovata — verifica il percorso con list_files o run_command(\"ls <directory>\")";
    }
    if combined.contains("already installed") || combined.contains("is already") {
        return "già installato o già presente — il problema è probabilmente altrove, non ripetere l'installazione";
    }
    if combined.contains("syntax error") || combined.contains("unexpected token") {
        return "errore di sintassi nel comando — correggi la sintassi prima di riprovare";
    }
    if combined.contains("connection refused") || combined.contains("network unreachable") {
        return "connessione rifiutata o rete non raggiungibile — verifica che il servizio target sia attivo";
    }
    if exit_code == 1 && stderr.trim().is_empty() && stdout.trim().is_empty() {
        return "exit code 1 senza output — per grep/find significa 'nessuna corrispondenza': prova un pattern diverso";
    }
    "errore generico — leggi stderr per la causa specifica, poi usa un approccio alternativo o un comando diverso"
}

/// Esegue un tool per conto dell'agente.
/// Ritorna sempre una stringa: il risultato in caso di successo, o un messaggio d'errore.
pub async fn execute_agent_tool(ctx: &AgentToolContext, name: &str, input: &Value) -> String {
    match name {
        "read_file" => files::tool_read_file(ctx, input).await,
        "read_file_lines" => files::tool_read_file_lines(ctx, input).await,
        "write_file" => files::tool_write_file(ctx, input).await,
        "list_files" => files::tool_list_files(ctx, input).await,
        "search_in_files" => files::tool_search_in_files(ctx, input).await,
        "git_status" => git::tool_git_status(ctx).await,
        "git_stage" => git::tool_git_stage(ctx, input).await,
        "git_commit" => git::tool_git_commit(ctx, input).await,
        "git_push" => git::tool_git_push(ctx).await,
        "git_pull" => git::tool_git_pull(ctx).await,
        "run_in_terminal" => service::tool_run_service(ctx, input, "task").await,
        "run_service" => service::tool_run_service(ctx, input, "service").await,
        "read_terminal_output" => service::tool_read_service_output(ctx, input).await,
        "read_service_output" => service::tool_read_service_output(ctx, input).await,
        "stop_service" => service::tool_stop_service(ctx, input).await,
        "delete_file" => files::tool_delete_file(ctx, input).await,
        "rename_file" => files::tool_rename_file(ctx, input).await,
        "edit_file" => files::tool_edit_file(ctx, input).await,
        "run_command" => command::tool_run_command(ctx, input).await,
        "dispatch_subtask" => tool_dispatch_subtask(ctx.clone(), input.clone()).await,
        "create_profile" => tool_create_profile(ctx, input).await,
        "update_profile" => tool_update_profile(ctx, input).await,
        "set_sandbox_config" => sandbox::tool_set_sandbox_config(ctx, input).await,
        "get_sandbox_config" => sandbox::tool_get_sandbox_config(ctx).await,
        "build_project_image" => service::tool_build_project_image(ctx).await,
        "scan_code_quality" => tool_scan_code_quality(ctx, input).await,
        "search_codebase_semantic" => {
            let query = input.get("query").and_then(Value::as_str).unwrap_or("").to_string();
            let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(8).min(20) as usize;
            tool_search_codebase_semantic(ctx, &query, limit).await
        }
        "search_file_semantic" => {
            let path = input.get("path").and_then(Value::as_str).unwrap_or("").to_string();
            let query = input.get("query").and_then(Value::as_str).unwrap_or("").to_string();
            let top_k = input.get("top_k").and_then(Value::as_u64).unwrap_or(5).min(10) as usize;
            let chunk_lines = input.get("chunk_lines").and_then(Value::as_u64).unwrap_or(50).max(10).min(200) as usize;
            tool_search_file_semantic(ctx, &path, &query, top_k, chunk_lines).await
        }
        "recall_context" => {
            let query = input.get("query").and_then(Value::as_str).unwrap_or("").to_string();
            let source = input.get("source").and_then(Value::as_str).unwrap_or("all").to_string();
            let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(5).min(10) as usize;
            tool_recall_context(ctx, &query, &source, limit).await
        }
        "run_playwright_tests" => testing::tool_run_playwright_tests(ctx, input).await,
        "batch_analyze_code" => tool_batch_analyze_code(ctx, input).await,
        // ── Nexus Builtin tool (prefisso nexus_*) ──────────────────────────
        // Dispatch verso nexus_builtin::execute_with_neural per usare
        // la ricerca semantica quando neural è disponibile (Qdrant).
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
        other => {
            // Suggerisci il tool corretto in base al nome errato chiamato
            let hint = match other {
                "mcp" | "execute" | "shell" | "bash" | "cmd" | "exec" | "terminal" | "command" =>
                    " Per eseguire comandi shell usa il tool `run_command` con il parametro `command`. Es: run_command({\"command\": \"which dotnet\"}).",
                "install" | "apt" | "brew" | "pip" | "npm_install" | "cargo_install" =>
                    " Per installare pacchetti usa `run_command` con il comando appropriato. Es: run_command({\"command\": \"sudo apt-get install -y <pacchetto>\"}).",
                "read" | "open" | "cat" | "file_read" =>
                    " Per leggere file usa `read_file` (parametro: path) oppure `read_file_lines` (parametri: path, start_line, end_line).",
                "write" | "save" | "file_write" =>
                    " Per scrivere file usa `write_file` (parametri: path, content) oppure `edit_file` per modifiche parziali.",
                "search" | "grep" | "find" =>
                    " Per cercare testo usa `search_in_files` (parametri: query, path). Per cercare file usa `list_files`.",
                "git" | "git_cmd" | "git_command" =>
                    " Per operazioni Git usa i tool dedicati: `git_status`, `git_stage`, `git_commit`, `git_push`, `git_pull`.",
                _ =>
                    " Controlla la lista dei tool disponibili. Se hai bisogno di eseguire comandi shell usa `run_command`.",
            };
            format!("❌ Tool '{other}' non esiste.{hint}")
        }
    }
}

// ── Tool non specifici di un dominio: profili utente, qualità, ricerca semantica, batch ──

// TODO(fase 4b): portare subtask dispatch sul brain
async fn tool_dispatch_subtask(_ctx: AgentToolContext, _input: Value) -> String {
    // Fase 4 refactor Nexus: AgentLoop locale eliminato. Il dispatch di
    // sotto-task agente deve passare dal brain LangGraph (non ancora wired).
    "[dispatch_subtask] subtask dispatch non disponibile in modalità brain orchestrator".to_string()
}

// ── Profili utente ──────────────────────────────────────────────────────────

async fn tool_create_profile(ctx: &AgentToolContext, input: &Value) -> String {
    let name = match input.get("name").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return "[Errore: parametro 'name' obbligatorio]".to_string(),
    };
    let system_prompt = match input.get("system_prompt").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return "[Errore: parametro 'system_prompt' obbligatorio]".to_string(),
    };
    let emoji = input.get("emoji").and_then(Value::as_str).unwrap_or("🤖").trim().to_string();
    let description: Option<String> = input.get("description").and_then(Value::as_str)
        .map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let default_provider: Option<String> = input.get("default_provider").and_then(Value::as_str)
        .map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let default_model: Option<String> = input.get("default_model").and_then(Value::as_str)
        .map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let default_automation: Option<String> = input.get("default_automation").and_then(Value::as_str)
        .map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let set_as_default = input.get("set_as_default").and_then(Value::as_bool).unwrap_or(false);

    // Controlla se esiste già un profilo con lo stesso nome per l'utente
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM user_profiles WHERE user_id = $1 AND name = $2"
    )
    .bind(ctx.user_id)
    .bind(&name)
    .fetch_optional(&*ctx.db)
    .await
    .unwrap_or(None);

    if existing.is_some() {
        return format!("[Profilo '{}' già esistente. Usa update_profile per modificarlo.]", name);
    }

    let profile_id = Uuid::new_v4();

    // Se set_as_default, azzera is_default sugli altri
    if set_as_default {
        let _ = sqlx::query(
            "UPDATE user_profiles SET is_default = FALSE WHERE user_id = $1"
        )
        .bind(ctx.user_id)
        .execute(&*ctx.db)
        .await;
    }

    let res = sqlx::query(
        "INSERT INTO user_profiles (id, user_id, name, avatar_emoji, description, system_prompt, \
         default_provider, default_model, default_automation, is_default, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW(), NOW())"
    )
    .bind(profile_id)
    .bind(ctx.user_id)
    .bind(&name)
    .bind(&emoji)
    .bind(&description)
    .bind(&system_prompt)
    .bind(&default_provider)
    .bind(&default_model)
    .bind(&default_automation)
    .bind(set_as_default)
    .execute(&*ctx.db)
    .await;

    match res {
        Ok(_) => format!(
            "Profilo '{}' {} creato con successo (ID: {}). L'utente lo troverà nel selettore profili accanto alla chat.",
            name, emoji,
            profile_id
        ),
        Err(e) => format!("[Errore creazione profilo: {}]", e),
    }
}

async fn tool_update_profile(ctx: &AgentToolContext, input: &Value) -> String {
    let profile_name = match input.get("profile_name").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return "[Errore: parametro 'profile_name' obbligatorio]".to_string(),
    };

    // Trova il profilo per nome e user_id
    let row: Option<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, system_prompt, avatar_emoji FROM user_profiles WHERE user_id = $1 AND name = $2"
    )
    .bind(ctx.user_id)
    .bind(&profile_name)
    .fetch_optional(&*ctx.db)
    .await
    .unwrap_or(None);

    let (profile_id, current_prompt, current_emoji) = match row {
        Some(r) => r,
        None => return format!("[Profilo '{}' non trovato. Usa create_profile per crearlo.]", profile_name),
    };

    let system_prompt = input.get("system_prompt").and_then(Value::as_str)
        .map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
        .unwrap_or(current_prompt);
    let emoji = input.get("emoji").and_then(Value::as_str)
        .map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
        .unwrap_or(current_emoji);
    let description: Option<String> = input.get("description").and_then(Value::as_str)
        .map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

    let res = sqlx::query(
        "UPDATE user_profiles SET system_prompt = $1, avatar_emoji = $2, description = COALESCE($3, description), updated_at = NOW() \
         WHERE id = $4"
    )
    .bind(&system_prompt)
    .bind(&emoji)
    .bind(&description)
    .bind(profile_id)
    .execute(&*ctx.db)
    .await;

    match res {
        Ok(_) => format!("Profilo '{}' aggiornato con successo.", profile_name),
        Err(e) => format!("[Errore aggiornamento profilo: {}]", e),
    }
}

async fn tool_scan_code_quality(ctx: &AgentToolContext, input: &Value) -> String {
    let file_path = input.get("file_path").and_then(Value::as_str);
    let severity_filter = input.get("severity_filter").and_then(Value::as_str).unwrap_or("all");

    if let Some(rel_path) = file_path {
        // Single file analysis
        let full_path = ctx.root_path.join(rel_path);
        let content = match tokio::fs::read_to_string(&full_path).await {
            Ok(c) => c,
            Err(e) => return format!("Errore lettura file: {}", e),
        };

        if rel_path.ends_with(".sql") {
            let db_report = mcp_db::analyze_query(&content);
            let findings: Vec<String> = db_report.findings.iter()
                .map(|f| format!("[{}][{}] {} -- {}", f.severity.to_uppercase(), f.category, f.title, f.detail))
                .collect();
            if findings.is_empty() {
                return format!("Nessun problema trovato in `{}`", rel_path);
            }
            return format!("Analisi SQL `{}`:\n{}", rel_path, findings.join("\n"));
        }

        let report = mcp_quality::analyze_source(rel_path, &content);

        let filtered: Vec<_> = report.findings.iter()
            .filter(|f| match severity_filter {
                "high" => f.severity == "high",
                "medium" => f.severity == "high" || f.severity == "medium",
                _ => true,
            })
            .collect();

        if filtered.is_empty() {
            return format!("Nessun problema trovato in `{}` (filtro: {})", rel_path, severity_filter);
        }

        let lines: Vec<String> = filtered.iter().map(|f| {
            let loc = f.line.map(|l| format!(":{}", l)).unwrap_or_default();
            format!("[{}][{}] {}{} -- {}", f.severity.to_uppercase(), f.category, rel_path, loc, f.title)
        }).collect();

        format!("Analisi `{}`:\n{}\n\nMetriche: {} righe totali, complessità max: {}, lunghezza media funzioni: {:.0}",
            rel_path, lines.join("\n"),
            report.metrics.total_lines, report.metrics.max_complexity, report.metrics.avg_function_length)
    } else {
        // Full project scan: read from DB if available
        let rows = sqlx::query(
            "SELECT file_path, category, severity, title, line_number \
             FROM project_quality_findings WHERE project_id = $1 AND fixed_at IS NULL \
             ORDER BY CASE severity WHEN 'high' THEN 1 WHEN 'medium' THEN 2 ELSE 3 END \
             LIMIT 30"
        )
        .bind(ctx.project_id)
        .fetch_all(&*ctx.db)
        .await;

        match rows {
            Ok(rows) if !rows.is_empty() => {
                let lines: Vec<String> = rows.iter().map(|r| {
                    let fp: String = r.try_get("file_path").unwrap_or_default();
                    let cat: String = r.try_get("category").unwrap_or_default();
                    let sev: String = r.try_get("severity").unwrap_or_default();
                    let title: String = r.try_get("title").unwrap_or_default();
                    let line: Option<i32> = r.try_get("line_number").ok().flatten();
                    let loc = line.map(|l| format!(":{}", l)).unwrap_or_default();
                    format!("[{}][{}] {}{} -- {}", sev.to_uppercase(), cat, fp, loc, title)
                }).collect();
                format!("Top findings del progetto (da ultimo scan):\n{}\n\nUsa scan_code_quality(file_path) per analizzare un file specifico.", lines.join("\n"))
            }
            _ => {
                "Nessun dato di qualità disponibile. Esegui prima una scansione completa dal pannello Ottimizzazione, oppure specifica un file_path per analizzare un file singolo.".to_string()
            }
        }
    }
}

async fn tool_search_codebase_semantic(
    ctx: &AgentToolContext,
    query: &str,
    limit: usize,
) -> String {
    if query.is_empty() {
        return "Errore: query vuota".to_string();
    }
    // Guard: se Qdrant o embedder sono down, ritorna subito
    {
        use std::sync::atomic::Ordering;
        let qdrant_ok = ctx.dependency_status.qdrant.load(Ordering::Relaxed);
        let embedder_ok = ctx.dependency_status.embedder.load(Ordering::Relaxed);
        if !qdrant_ok || !embedder_ok {
            return format!(
                "Ricerca semantica non disponibile (qdrant={}, embedder={}). \
                 Usa 'grep' o 'find_files' per cercare nel codice.",
                if qdrant_ok { "ok" } else { "down" },
                if embedder_ok { "ok" } else { "down" },
            );
        }
    }
    // Embed la query
    let embedding = match ctx.neural.embed_text("", query).await {
        Ok(v) => v,
        Err(e) => return format!("Errore embedding: {e}"),
    };
    // Cerca in Qdrant
    let hits = match vector_memory::search_code_index(&ctx.db, &embedding, ctx.project_id, limit).await {
        Ok(h) => h,
        Err(e) => return format!("Errore ricerca: {e}"),
    };
    if hits.is_empty() {
        return "Nessun risultato trovato. Il codebase potrebbe non essere ancora indicizzato — prova ad analizzare il progetto prima.".to_string();
    }
    // Formatta risultati
    let results: Vec<String> = hits.iter().enumerate().map(|(i, hit)| {
        let file = hit.payload.get("file_path").and_then(Value::as_str).unwrap_or("?");
        let chunk = hit.payload.get("chunk_index").and_then(Value::as_u64).unwrap_or(0);
        let labels = hit.payload.get("ui_labels")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        let score = (hit.score * 100.0).round() as u64;
        let mut parts = vec![format!("{}. {} (score: {}%)", i + 1, file, score)];
        if !labels.is_empty() {
            parts.push(format!("   Label UI: {labels}"));
        }
        if chunk > 0 {
            parts.push(format!("   Chunk: {chunk}"));
        }
        parts.join("\n")
    }).collect();
    format!("Risultati per '{query}':\n\n{}", results.join("\n\n"))
}

async fn tool_recall_context(
    ctx: &AgentToolContext,
    query: &str,
    source: &str,
    limit: usize,
) -> String {
    if query.is_empty() {
        return "Errore: query vuota".to_string();
    }
    // Guard: se Qdrant o embedder sono down, ritorna subito
    {
        use std::sync::atomic::Ordering;
        let qdrant_ok = ctx.dependency_status.qdrant.load(Ordering::Relaxed);
        let embedder_ok = ctx.dependency_status.embedder.load(Ordering::Relaxed);
        if !qdrant_ok || !embedder_ok {
            return "Recall context non disponibile: dipendenze vettoriali temporaneamente offline.".to_string();
        }
    }
    let embedding = match ctx.neural.embed_text("", query).await {
        Ok(v) => v,
        Err(e) => return format!("Errore embedding: {e}"),
    };

    let search_conversation = source == "conversation" || source == "all";
    let search_project = source == "project" || source == "all";
    let mut sections: Vec<String> = Vec::new();

    if search_conversation {
        if let Some(sid) = ctx.session_id {
            match vector_memory::search_conversation_context(
                &ctx.db, &embedding, sid, limit as u64, 0.55,
            ).await {
                Ok(hits) if !hits.is_empty() => {
                    let mut conv_results: Vec<String> = Vec::new();
                    for (i, hit) in hits.iter().enumerate() {
                        let role = hit.payload.get("role").and_then(Value::as_str).unwrap_or("?");
                        let preview = hit.payload.get("text_preview")
                            .and_then(Value::as_str)
                            .or_else(|| hit.payload.get("content").and_then(Value::as_str))
                            .unwrap_or("");
                        let score = (hit.score * 100.0).round() as u64;
                        conv_results.push(format!(
                            "{}. [{}] (pertinenza: {}%)\n{}",
                            i + 1, role, score,
                            if preview.len() > 1500 { &preview[..1500] } else { preview }
                        ));
                    }
                    sections.push(format!(
                        "--- Contesto conversazionale ---\n{}",
                        conv_results.join("\n\n")
                    ));
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("recall_context: errore ricerca conversazione: {e}");
                }
            }
        }
    }

    if search_project {
        match vector_memory::search_project_context_points(
            &ctx.db, &embedding, ctx.project_id, limit as u64, 0.60,
        ).await {
            Ok(hits) if !hits.is_empty() => {
                let mut proj_results: Vec<String> = Vec::new();
                for (i, hit) in hits.iter().enumerate() {
                    let title = hit.payload.get("section_title")
                        .and_then(Value::as_str).unwrap_or("Contesto progetto");
                    let preview = hit.payload.get("text_preview")
                        .and_then(Value::as_str).unwrap_or("");
                    let score = (hit.score * 100.0).round() as u64;
                    proj_results.push(format!(
                        "{}. {} (pertinenza: {}%)\n{}",
                        i + 1, title, score,
                        if preview.len() > 1500 { &preview[..1500] } else { preview }
                    ));
                }
                sections.push(format!(
                    "--- Contesto progetto ---\n{}",
                    proj_results.join("\n\n")
                ));
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("recall_context: errore ricerca progetto: {e}");
            }
        }
    }

    if sections.is_empty() {
        return format!(
            "Nessun contesto rilevante trovato per '{}'. La conversazione potrebbe non essere ancora indicizzata o la query potrebbe essere troppo specifica.",
            query
        );
    }

    format!("Contesto recuperato per '{query}':\n\n{}", sections.join("\n\n"))
}

/// Ricerca semantica TF-IDF in-process all'interno di un singolo file.
/// Divide il file in chunk sovrapposti, scorea ogni chunk vs query e
/// restituisce le sezioni più rilevanti con i numeri di riga.
async fn tool_search_file_semantic(
    ctx: &AgentToolContext,
    path_str: &str,
    query: &str,
    top_k: usize,
    chunk_lines: usize,
) -> String {
    if query.is_empty() {
        return "[Errore: parametro 'query' mancante]".to_string();
    }
    if path_str.is_empty() {
        return "[Errore: parametro 'path' mancante]".to_string();
    }

    // Risolvi il percorso (supporta assoluti e relativi alla root progetto)
    let target = if std::path::Path::new(path_str).is_absolute() {
        std::path::PathBuf::from(path_str)
    } else {
        match resolve_relative_path(&ctx.root_path, path_str) {
            Ok(p) => p,
            Err(e) => return format!("[Errore percorso: {}]", e.1["error"].as_str().unwrap_or("path error")),
        }
    };

    let content = match tokio::fs::read_to_string(&target).await {
        Ok(c) => c,
        Err(e) => return format!("[Errore lettura '{}': {}]", path_str, e),
    };

    let all_lines: Vec<&str> = content.lines().collect();
    let total_lines = all_lines.len();

    if total_lines == 0 {
        return format!("Il file '{}' è vuoto.", path_str);
    }

    // Tokenizza la query: lowercase, split su non-alfanumerici, filtra token brevi
    let query_tokens: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.len() >= 2)
        .map(str::to_string)
        .collect();

    if query_tokens.is_empty() {
        return "[Errore: query non contiene termini di ricerca validi]".to_string();
    }

    // Overlap: 20% del chunk_lines per non perdere contesto ai bordi
    let overlap = (chunk_lines / 5).max(5);
    let step = chunk_lines.saturating_sub(overlap).max(1);

    // Costruisci chunk con scoring TF-IDF semplificato
    struct ScoredChunk {
        start_line: usize, // 1-based
        end_line: usize,   // 1-based
        score: f32,
        text: String,
    }

    let mut chunks: Vec<ScoredChunk> = Vec::new();
    let mut chunk_start = 0usize;

    while chunk_start < total_lines {
        let chunk_end = (chunk_start + chunk_lines).min(total_lines);
        let chunk_text = all_lines[chunk_start..chunk_end].join("\n");
        let chunk_lower = chunk_text.to_lowercase();

        // Score = somma pesata delle occorrenze dei token della query
        // Penalty per chunk troppo corti (pochi token di testo effettivo)
        let word_count = chunk_lower
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|t| !t.is_empty())
            .count()
            .max(1) as f32;

        let mut raw_score = 0.0f32;
        for token in &query_tokens {
            // Conta occorrenze del token nel chunk
            let count = chunk_lower.matches(token.as_str()).count() as f32;
            if count > 0.0 {
                // TF puro, log-normalizzato per ridurre l'influenza di token ripetuti
                raw_score += (1.0 + count.ln()) * (total_lines as f32 / (chunks.len() + 1).max(1) as f32).ln().max(1.0);
            }
        }

        // Normalizza per densità (token utili per riga)
        let density_bonus = (word_count / (chunk_end - chunk_start) as f32).min(2.0);
        let score = raw_score * density_bonus;

        chunks.push(ScoredChunk {
            start_line: chunk_start + 1,
            end_line: chunk_end,
            score,
            text: chunk_text,
        });

        chunk_start += step;
        if chunk_start >= total_lines {
            break;
        }
    }

    if chunks.is_empty() {
        return format!("File '{}' ({} righe): nessun chunk prodotto.", path_str, total_lines);
    }

    // Ordina per score decrescente
    chunks.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    // Deduplica: salta chunk il cui range sovrappone un chunk già selezionato
    let mut selected: Vec<&ScoredChunk> = Vec::new();
    'outer: for chunk in &chunks {
        for sel in &selected {
            let overlap_start = chunk.start_line.max(sel.start_line);
            let overlap_end = chunk.end_line.min(sel.end_line);
            if overlap_start <= overlap_end {
                let overlap_len = overlap_end - overlap_start + 1;
                let min_len = (chunk.end_line - chunk.start_line + 1).min(sel.end_line - sel.start_line + 1);
                if overlap_len * 2 > min_len {
                    continue 'outer; // sovrappone troppo: salta
                }
            }
        }
        selected.push(chunk);
        if selected.len() >= top_k {
            break;
        }
    }

    // Ri-ordina i selezionati per numero di riga (ordine naturale del file)
    selected.sort_by_key(|c| c.start_line);

    let header = format!(
        "File: {} ({} righe totali) — {} sezioni rilevanti per '{}'\n",
        path_str, total_lines, selected.len(), query
    );

    let sections: Vec<String> = selected
        .iter()
        .map(|c| {
            format!(
                "── Righe {}-{} (score: {:.0}) ──\n{}",
                c.start_line, c.end_line, c.score, c.text
            )
        })
        .collect();

    format!("{}\n{}", header, sections.join("\n\n"))
}

async fn tool_batch_analyze_code(ctx: &AgentToolContext, input: &Value) -> String {
    let task = input.get("task").and_then(Value::as_str).unwrap_or("analyze");
    let files_arr = match input.get("files").and_then(Value::as_array) {
        Some(a) => a.clone(),
        None => return "[batch_analyze_code] Campo 'files' mancante o non è un array".to_string(),
    };
    if files_arr.is_empty() {
        return "[batch_analyze_code] Nessun file specificato".to_string();
    }
    if files_arr.len() > 20 {
        return "[batch_analyze_code] Massimo 20 file per batch".to_string();
    }

    let system_prompt = match task {
        "document" => "Sei un esperto di documentazione tecnica. Analizza il codice e genera commenti/docstring chiari e concisi in italiano. Concentrati sul WHY, non sul WHAT.",
        "optimize" => "Sei un esperto di ottimizzazione del codice. Identifica problemi di performance, complessità eccessiva, codice duplicato e suggerisci refactoring concreti.",
        _ => "Sei un esperto di revisione del codice. Identifica bug potenziali, problemi di sicurezza, violazioni di pattern architetturali e punti di miglioramento.",
    };

    // Leggi il contenuto dei file non forniti
    let mut requests: Vec<serde_json::Value> = Vec::new();
    for (i, file_obj) in files_arr.iter().enumerate() {
        let path_str = match file_obj.get("path").and_then(Value::as_str) {
            Some(p) => p.to_string(),
            None => continue,
        };
        let content = if let Some(c) = file_obj.get("content").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            c.to_string()
        } else {
            // Leggi il file dalla root del progetto
            let abs_path = ctx.root_path.join(&path_str);
            match tokio::fs::read_to_string(&abs_path).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("batch_analyze_code: impossibile leggere {}: {}", path_str, e);
                    format!("[Errore lettura file: {e}]")
                }
            }
        };
        requests.push(serde_json::json!({
            "custom_id": format!("file-{}", i),
            "system": system_prompt,
            "prompt": format!("File: {}\n\n```\n{}\n```\n\nEsegui il task '{}' su questo file.", path_str, &content[..content.len().min(32000)], task),
        }));
    }

    if requests.is_empty() {
        return "[batch_analyze_code] Nessun file valido trovato".to_string();
    }

    let brain_http_url = std::env::var("BRAIN_HTTP_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());
    let client = reqwest::Client::new();

    // Sottomette il batch
    let submit_resp = match client
        .post(format!("{brain_http_url}/batch-analyze/submit"))
        .json(&serde_json::json!({ "requests": requests }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return format!("[batch_analyze_code] Errore sottomissione batch: {e}"),
    };
    let batch_id = match submit_resp.json::<serde_json::Value>().await {
        Ok(v) => match v.get("batch_id").and_then(Value::as_str) {
            Some(id) => id.to_string(),
            None => return format!("[batch_analyze_code] Risposta batch non valida: {v}"),
        },
        Err(e) => return format!("[batch_analyze_code] Errore parsing risposta submit: {e}"),
    };

    // Poll con backoff esponenziale (max 10 minuti)
    let mut wait_secs = 2u64;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(600);
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(wait_secs)).await;
        wait_secs = (wait_secs * 2).min(60);

        let status_resp = match client
            .get(format!("{brain_http_url}/batch-analyze/{batch_id}/status"))
            .send()
            .await
            .and_then(|r| Ok(r))
        {
            Ok(r) => r,
            Err(e) => return format!("[batch_analyze_code] Errore polling status: {e}"),
        };
        let status_json = match status_resp.json::<serde_json::Value>().await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let processing_status = status_json.get("status").and_then(Value::as_str).unwrap_or("");
        if processing_status == "ended" {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            return format!("[batch_analyze_code] Timeout: il batch {} non ha terminato in 10 minuti", batch_id);
        }
    }

    // Recupera i risultati
    let results_resp = match client
        .get(format!("{brain_http_url}/batch-analyze/{batch_id}/results"))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return format!("[batch_analyze_code] Errore recupero risultati: {e}"),
    };
    let results: Vec<serde_json::Value> = match results_resp.json().await {
        Ok(v) => v,
        Err(e) => return format!("[batch_analyze_code] Errore parsing risultati: {e}"),
    };

    // Formatta output
    let mut output_parts: Vec<String> = Vec::new();
    for (i, file_obj) in files_arr.iter().enumerate() {
        let path_str = file_obj.get("path").and_then(Value::as_str).unwrap_or("?");
        let custom_id = format!("file-{i}");
        if let Some(result) = results.iter().find(|r| r.get("custom_id").and_then(Value::as_str) == Some(&custom_id)) {
            if let Some(content) = result.get("content").and_then(Value::as_str) {
                output_parts.push(format!("### {path_str}\n\n{content}"));
            } else if let Some(err) = result.get("error").and_then(Value::as_str) {
                output_parts.push(format!("### {path_str}\n\n[Errore: {err}]"));
            }
        }
    }

    if output_parts.is_empty() {
        format!("[batch_analyze_code] Nessun risultato per il batch {batch_id}")
    } else {
        format!("## Analisi batch ({task}) — {} file\n\n{}", output_parts.len(), output_parts.join("\n\n---\n\n"))
    }
}
