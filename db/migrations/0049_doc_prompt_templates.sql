-- Prompt templates for document generation guidance

-- Update category check to include 'docs'
ALTER TABLE nexus_prompt_templates DROP CONSTRAINT IF EXISTS nexus_prompt_templates_category_check;
ALTER TABLE nexus_prompt_templates ADD CONSTRAINT nexus_prompt_templates_category_check
  CHECK (category IN ('system', 'quality', 'automation', 'chat', 'docs', 'profile'));

INSERT INTO nexus_prompt_templates (key, category, title, content, updated_by) VALUES
('docs.functional_analysis', 'docs', 'Template Analisi Funzionale IEEE 830',
$$Genera un documento di Analisi Funzionale seguendo lo standard IEEE 830 / ISO 29148.

STRUTTURA OBBLIGATORIA del content_json:
{
  "version": "1.0.0",
  "sections": [
    {"number": "1", "title": "Introduzione", "content": "...", "subsections": [
      {"number": "1.1", "title": "Scopo", "content": "Descrivi lo scopo del sistema"},
      {"number": "1.2", "title": "Ambito del Prodotto", "content": "..."},
      {"number": "1.3", "title": "Definizioni, Acronimi e Abbreviazioni", "content": "..."},
      {"number": "1.4", "title": "Riferimenti", "content": "..."}
    ]},
    {"number": "2", "title": "Descrizione Generale", "subsections": [
      {"number": "2.1", "title": "Prospettiva del Prodotto", "content": "..."},
      {"number": "2.2", "title": "Funzionalità del Prodotto", "content": "..."},
      {"number": "2.3", "title": "Classi di Utenti", "content": "..."},
      {"number": "2.4", "title": "Ambiente Operativo", "content": "..."},
      {"number": "2.5", "title": "Vincoli", "content": "..."},
      {"number": "2.6", "title": "Assunzioni e Dipendenze", "content": "..."}
    ]},
    {"number": "3", "title": "Requisiti Funzionali", "content": "Elenca ogni requisito come RF-001, RF-002 con priorità, input, output, precondizioni"},
    {"number": "4", "title": "Requisiti Non Funzionali", "subsections": [
      {"number": "4.1", "title": "Performance", "content": "..."},
      {"number": "4.2", "title": "Sicurezza", "content": "..."},
      {"number": "4.3", "title": "Usabilità", "content": "..."}
    ]},
    {"number": "5", "title": "Interfacce Esterne", "subsections": [
      {"number": "5.1", "title": "Interfacce Utente", "content": "..."},
      {"number": "5.2", "title": "Interfacce Software", "content": "..."},
      {"number": "5.3", "title": "Interfacce API", "content": "..."}
    ]},
    {"number": "6", "title": "Matrice di Tracciabilità", "content": "..."}
  ]
}

ISTRUZIONI:
- Analizza il codebase del progetto per estrarre i requisiti reali
- Ogni requisito deve avere: ID, descrizione, priorità (alta/media/bassa), input, output
- Sii specifico e concreto, non generico
- Usa il tool nexus_doc_generate con doc_type="functional_analysis"$$,
'system')
ON CONFLICT (key) DO NOTHING;

INSERT INTO nexus_prompt_templates (key, category, title, content, updated_by) VALUES
('docs.technical_analysis', 'docs', 'Template Analisi Tecnica',
$$Genera un documento di Analisi Tecnica del progetto.

STRUTTURA del content_json:
- Sezione 1: Panoramica Architettura (stack, deployment, componenti)
- Sezione 2: Struttura Codebase (moduli, dipendenze, pattern)
- Sezione 3: Database Schema (tabelle, relazioni, indici)
- Sezione 4: API Reference (endpoint, request/response)
- Sezione 5: Integrazioni Esterne
- Sezione 6: Sicurezza
- Sezione 7: DevOps e Infrastruttura

ISTRUZIONI:
- Analizza i file Cargo.toml, package.json per le dipendenze
- Leggi le migration SQL per lo schema DB
- Elenca tutti gli endpoint API da main.rs
- Documenta l'architettura reale, non ipotetica
- Usa il tool nexus_doc_generate con doc_type="technical_analysis"$$,
'system')
ON CONFLICT (key) DO NOTHING;

INSERT INTO nexus_prompt_templates (key, category, title, content, updated_by) VALUES
('docs.er_diagram', 'docs', 'Template Diagramma ER',
$$Genera un documento con il Diagramma Entity-Relationship del database.

STRUTTURA del content_json:
- Sezione 1: Panoramica delle Entità (lista tabelle con descrizione)
- Sezione 2: Diagramma ER (usa sintassi Mermaid erDiagram)
- Sezione 3: Dettaglio Tabelle (colonne, tipi, vincoli FK per ogni tabella)
- Sezione 4: Indici e Vincoli (indici chiave per performance)

ISTRUZIONI:
- Leggi le migration SQL in db/migrations/ per estrarre lo schema reale
- Genera il diagramma ER in sintassi Mermaid
- Per ogni tabella, documenta: colonne, tipi, nullable, FK, indici
- Usa il tool nexus_doc_generate con doc_type="er_diagram"$$,
'system')
ON CONFLICT (key) DO NOTHING;

INSERT INTO nexus_prompt_templates (key, category, title, content, updated_by) VALUES
('docs.project_management', 'docs', 'Template Gestione Progetto',
$$Genera un documento di Gestione Progetto.

STRUTTURA del content_json:
- Sezione 1: Panoramica (obiettivi, ambito, stakeholder)
- Sezione 2: Piano di Lavoro (milestone, timeline, WBS)
- Sezione 3: Gestione Rischi (identificazione, matrice, mitigazione)
- Sezione 4: Risorse e Budget (team, infrastruttura, costi)
- Sezione 5: Qualità e Testing (strategia test, criteri accettazione)

ISTRUZIONI:
- Analizza la struttura del progetto per inferire milestone realistiche
- Identifica rischi tecnici basandoti sulle dipendenze e complessità
- Usa il tool nexus_doc_generate con doc_type="project_management"$$,
'system')
ON CONFLICT (key) DO NOTHING;

INSERT INTO nexus_prompt_templates (key, category, title, content, updated_by) VALUES
('docs.release_notes', 'docs', 'Template Release Notes',
$$Genera un documento di Release Notes / Note di Rilascio.

STRUTTURA del content_json:
- Sezione 1: Panoramica della Versione
- Sezione 2: Nuove Funzionalità
- Sezione 3: Miglioramenti
- Sezione 4: Bug Fix
- Sezione 5: Breaking Changes
- Sezione 6: Problemi Noti
- Sezione 7: Istruzioni di Aggiornamento

ISTRUZIONI:
- Usa git log per estrarre i commit recenti
- Raggruppa per categoria (feat, fix, refactor, docs)
- Segui il formato Conventional Commits per la classificazione
- Indica la versione SemVer appropriata
- Usa il tool nexus_doc_generate con doc_type="release_notes"$$,
'system')
ON CONFLICT (key) DO NOTHING;
