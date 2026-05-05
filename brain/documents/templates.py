"""Default document structures following IEEE 830 / ISO 29148 standards."""
from __future__ import annotations

# Each template defines the section skeleton for a doc_type.
# The agent fills in the content; these are the expected section numbers/titles.

TEMPLATES: dict[str, dict] = {
    "functional_analysis": {
        "title_default": "Analisi Funzionale",
        "standard": "ieee830",
        "sections": [
            {
                "number": "1",
                "title": "Introduzione",
                "subsections": [
                    {"number": "1.1", "title": "Scopo"},
                    {"number": "1.2", "title": "Ambito del Prodotto"},
                    {"number": "1.3", "title": "Definizioni, Acronimi e Abbreviazioni"},
                    {"number": "1.4", "title": "Riferimenti"},
                    {"number": "1.5", "title": "Panoramica del Documento"},
                ],
            },
            {
                "number": "2",
                "title": "Descrizione Generale",
                "subsections": [
                    {"number": "2.1", "title": "Prospettiva del Prodotto"},
                    {"number": "2.2", "title": "Funzionalità del Prodotto"},
                    {"number": "2.3", "title": "Classi di Utenti e Caratteristiche"},
                    {"number": "2.4", "title": "Ambiente Operativo"},
                    {"number": "2.5", "title": "Vincoli di Progettazione e Implementazione"},
                    {"number": "2.6", "title": "Assunzioni e Dipendenze"},
                ],
            },
            {
                "number": "3",
                "title": "Requisiti Funzionali",
                "subsections": [],
            },
            {
                "number": "4",
                "title": "Requisiti Non Funzionali",
                "subsections": [
                    {"number": "4.1", "title": "Requisiti di Performance"},
                    {"number": "4.2", "title": "Requisiti di Sicurezza"},
                    {"number": "4.3", "title": "Requisiti di Usabilità"},
                    {"number": "4.4", "title": "Requisiti di Affidabilità"},
                    {"number": "4.5", "title": "Requisiti di Manutenibilità"},
                ],
            },
            {
                "number": "5",
                "title": "Interfacce Esterne",
                "subsections": [
                    {"number": "5.1", "title": "Interfacce Utente"},
                    {"number": "5.2", "title": "Interfacce Software"},
                    {"number": "5.3", "title": "Interfacce Hardware"},
                    {"number": "5.4", "title": "Interfacce di Comunicazione"},
                ],
            },
            {
                "number": "6",
                "title": "Matrice di Tracciabilità",
                "subsections": [],
            },
        ],
    },
    "technical_analysis": {
        "title_default": "Analisi Tecnica",
        "standard": "iso29148",
        "sections": [
            {
                "number": "1",
                "title": "Panoramica Architettura",
                "subsections": [
                    {"number": "1.1", "title": "Stack Tecnologico"},
                    {"number": "1.2", "title": "Diagramma dei Componenti"},
                    {"number": "1.3", "title": "Deployment Architecture"},
                ],
            },
            {
                "number": "2",
                "title": "Struttura del Codebase",
                "subsections": [
                    {"number": "2.1", "title": "Organizzazione Moduli"},
                    {"number": "2.2", "title": "Dipendenze Principali"},
                    {"number": "2.3", "title": "Pattern Architetturali"},
                ],
            },
            {
                "number": "3",
                "title": "Database Schema",
                "subsections": [
                    {"number": "3.1", "title": "Tabelle e Relazioni"},
                    {"number": "3.2", "title": "Indici e Performance"},
                    {"number": "3.3", "title": "Migrazioni"},
                ],
            },
            {
                "number": "4",
                "title": "API Reference",
                "subsections": [
                    {"number": "4.1", "title": "Endpoint REST"},
                    {"number": "4.2", "title": "Autenticazione e Autorizzazione"},
                    {"number": "4.3", "title": "Formati Request/Response"},
                    {"number": "4.4", "title": "Servizi gRPC"},
                ],
            },
            {
                "number": "5",
                "title": "Integrazioni Esterne",
                "subsections": [
                    {"number": "5.1", "title": "Servizi AI (Provider LLM)"},
                    {"number": "5.2", "title": "Vector Database (Qdrant)"},
                    {"number": "5.3", "title": "Cache (Redis)"},
                ],
            },
            {
                "number": "6",
                "title": "Sicurezza",
                "subsections": [
                    {"number": "6.1", "title": "Autenticazione"},
                    {"number": "6.2", "title": "Autorizzazione e Ruoli"},
                    {"number": "6.3", "title": "Crittografia e Gestione Segreti"},
                ],
            },
            {
                "number": "7",
                "title": "DevOps e Infrastruttura",
                "subsections": [
                    {"number": "7.1", "title": "CI/CD Pipeline"},
                    {"number": "7.2", "title": "Monitoring e Logging"},
                    {"number": "7.3", "title": "Backup e Disaster Recovery"},
                ],
            },
        ],
    },
    "er_diagram": {
        "title_default": "Diagramma Entity-Relationship",
        "standard": "minimal",
        "sections": [
            {
                "number": "1",
                "title": "Panoramica delle Entità",
                "subsections": [],
            },
            {
                "number": "2",
                "title": "Diagramma ER",
                "subsections": [],
            },
            {
                "number": "3",
                "title": "Dettaglio Tabelle",
                "subsections": [],
            },
            {
                "number": "4",
                "title": "Indici e Vincoli",
                "subsections": [],
            },
        ],
    },
    "project_management": {
        "title_default": "Documento di Gestione Progetto",
        "standard": "minimal",
        "sections": [
            {
                "number": "1",
                "title": "Panoramica del Progetto",
                "subsections": [
                    {"number": "1.1", "title": "Obiettivi"},
                    {"number": "1.2", "title": "Ambito"},
                    {"number": "1.3", "title": "Stakeholder"},
                ],
            },
            {
                "number": "2",
                "title": "Piano di Lavoro",
                "subsections": [
                    {"number": "2.1", "title": "Milestone"},
                    {"number": "2.2", "title": "Timeline"},
                    {"number": "2.3", "title": "Work Breakdown Structure"},
                ],
            },
            {
                "number": "3",
                "title": "Gestione dei Rischi",
                "subsections": [
                    {"number": "3.1", "title": "Identificazione Rischi"},
                    {"number": "3.2", "title": "Matrice di Rischio"},
                    {"number": "3.3", "title": "Piano di Mitigazione"},
                ],
            },
            {
                "number": "4",
                "title": "Risorse e Budget",
                "subsections": [
                    {"number": "4.1", "title": "Team"},
                    {"number": "4.2", "title": "Infrastruttura"},
                    {"number": "4.3", "title": "Stima Costi"},
                ],
            },
            {
                "number": "5",
                "title": "Qualità e Testing",
                "subsections": [
                    {"number": "5.1", "title": "Strategia di Test"},
                    {"number": "5.2", "title": "Criteri di Accettazione"},
                ],
            },
        ],
    },
    "release_notes": {
        "title_default": "Note di Rilascio",
        "standard": "minimal",
        "sections": [
            {
                "number": "1",
                "title": "Panoramica della Versione",
                "subsections": [],
            },
            {
                "number": "2",
                "title": "Nuove Funzionalità",
                "subsections": [],
            },
            {
                "number": "3",
                "title": "Miglioramenti",
                "subsections": [],
            },
            {
                "number": "4",
                "title": "Bug Fix",
                "subsections": [],
            },
            {
                "number": "5",
                "title": "Breaking Changes",
                "subsections": [],
            },
            {
                "number": "6",
                "title": "Problemi Noti",
                "subsections": [],
            },
            {
                "number": "7",
                "title": "Istruzioni di Aggiornamento",
                "subsections": [],
            },
        ],
    },
}


def get_template(doc_type: str) -> dict:
    """Return the template for a doc_type, or a minimal fallback."""
    return TEMPLATES.get(doc_type, {
        "title_default": "Documento",
        "standard": "minimal",
        "sections": [],
    })
