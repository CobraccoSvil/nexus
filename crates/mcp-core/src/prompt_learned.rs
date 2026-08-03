//! Istruzioni apprese nel prompt: punto unico (regola L) di QUALI entrano e
//! COME si rendono.
//!
//! Sono le regole durature che il distillatore ricava dall'esperienza operativa
//! del progetto e materializza in `nexus_learned_instructions` (DB META) con
//! `status='active'`. Il pannello admin le mostra e permette di correggerle a
//! mano.
//!
//! ROOT CAUSE che ha reso necessario questo modulo: il distillatore girava, il
//! pannello le mostrava, il template `system.learned_instructions_block`
//! esisteva in DB col suo placeholder `{{rules}}` — e NESSUN compositore di
//! prompt leggeva la tabella. Misurato il 03/08/2026 sul DB vivo: 68 istruzioni
//! `active` e 3 `proposed`, e ogni lettura della tabella in tutto il codebase
//! stava dentro `learned_instructions.rs` stesso (il distillatore che le scrive
//! e le rotte admin che le mostrano). Il ciclo di apprendimento era completo
//! tranne che nell'unico punto in cui serviva: il modello non le ha mai lette.
//!
//! Non era un difetto astratto. Fra le 68 attive al momento della misura:
//! «Evita URL hardcoded come localhost/127.0.0.1 nei file di configurazione o
//! test» e «Non scegliere manualmente le porte nei file .env: richiedile
//! tramite il sistema di allocazione» — cioe' esattamente i due difetti che
//! l'app generata la sera prima aveva riprodotto entrambi.
//!
//! ## Perche' un punto unico, e perche' STABILE
//!
//! I compositori sono due (`Orchestrator::compose_prompt` per il turno singolo,
//! `compose_agent_system_text` per il run agentico) ed e' la stessa lezione di
//! [`crate::prompt_memories`]: un consumo scritto in un ramo solo non entra
//! nell'altro, e in modalita' Conferma/Automatico l'handler dispatcha al run
//! agentico prima ancora di arrivare al primo.
//!
//! Il blocco appartiene alla parte STABILE del system, a differenza delle
//! memorie: quelle sono richiamate per pertinenza semantica alla domanda del
//! turno (cambiano a ogni messaggio), queste sono le regole del PROGETTO e
//! cambiano solo quando il distillatore gira o l'utente le corregge. Metterle
//! dietro il confine di turno le farebbe uscire dal prefisso riusabile senza
//! alcun motivo, e l'ordine e' deterministico apposta perche' due run dello
//! stesso progetto producano gli stessi byte (vedi [`ORDINE`]).

use sqlx::PgPool;
use uuid::Uuid;

/// Quante regole al massimo entrano nel prompt.
///
/// Un tetto serve: la tabella cresce a ogni distillazione e un blocco che si
/// allunga senza limite finirebbe per pesare piu' di cio' che insegna. Il
/// criterio del taglio e' la confidenza, cioe' quante volte l'esperienza ha
/// confermato la regola.
const MAX_REGOLE: i64 = 30;

/// Ordinamento CANONICO delle regole nel blocco.
///
/// Deterministico per necessita', non per gusto: il blocco sta nella parte
/// stabile del system, e due run dello stesso progetto devono produrre gli
/// STESSI byte o il prefisso non viene riusato dal fornitore (vedi
/// `nexus_types::system_prompt`). `confidence DESC` sceglie QUALI regole
/// entrano, `id` spezza i pari — senza il secondo criterio due righe con la
/// stessa confidenza potrebbero uscire in ordine diverso a ogni query, e il
/// prefisso cambierebbe senza che nulla sia cambiato davvero.
const ORDINE: &str = "ORDER BY confidence DESC, id";

/// Chiave del template che rende il blocco. Il testo vive nel DB (regola G):
/// qui c'e' solo il nome, e il placeholder che il template dichiara.
const CHIAVE_TEMPLATE: &str = "system.learned_instructions_block";

/// Placeholder che il template espone per l'elenco delle regole.
const SEGNAPOSTO_REGOLE: &str = "{{rules}}";

/// Le istruzioni apprese ATTIVE di un progetto, pronte per il prompt.
pub(crate) struct LearnedInstructions {
    regole: Vec<String>,
}

impl LearnedInstructions {
    /// Carica le regole attive del progetto.
    ///
    /// Non fallisce mai verso l'alto: un turno senza istruzioni apprese e' un
    /// turno valido, un turno rotto perche' una SELECT non e' andata no. Il DB
    /// e' quello META (la tabella e' li', condivisa fra i progetti e filtrata
    /// per `project_id`).
    pub(crate) async fn load(db: &PgPool, project_id: Uuid) -> Self {
        let sql = format!(
            "SELECT rule_text FROM nexus_learned_instructions \
              WHERE project_id = $1 AND status = 'active' \
                AND rule_text IS NOT NULL AND btrim(rule_text) <> '' \
              {ORDINE} LIMIT $2"
        );
        let regole: Vec<String> = match sqlx::query_scalar(&sql)
            .bind(project_id)
            .bind(MAX_REGOLE)
            .fetch_all(db)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    project_id = %project_id,
                    error = %e,
                    "istruzioni apprese non leggibili: il prompt prosegue senza"
                );
                Vec::new()
            }
        };
        Self { regole }
    }

    /// Il blocco da innestare nel system, gia' reso col template del DB.
    ///
    /// `None` quando non c'e' nulla da dire: un blocco vuoto o col solo
    /// involucro sarebbe rumore, e su un system prompt e' rumore con autorita'.
    ///
    /// Il testo lo porta il template (regola G): se manca dal DB il blocco NON
    /// si compone da un letterale di ripiego — una configurazione assente deve
    /// vedersi, non essere supplita in silenzio con parole che nessuno ha
    /// scelto.
    pub(crate) async fn section(&self, db: &PgPool) -> Option<String> {
        if self.regole.is_empty() {
            return None;
        }
        let template: Option<String> = sqlx::query_scalar::<_, String>(
            "SELECT content FROM nexus_prompt_templates \
              WHERE key = $1 AND is_active = true",
        )
        .bind(CHIAVE_TEMPLATE)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .filter(|s: &String| !s.trim().is_empty());
        let template = match template {
            Some(t) => t,
            None => {
                tracing::warn!(
                    chiave = CHIAVE_TEMPLATE,
                    regole = self.regole.len(),
                    "template del blocco istruzioni apprese assente o disattivo: il blocco non entra"
                );
                return None;
            }
        };
        let elenco = self
            .regole
            .iter()
            .map(|r| format!("- {}", r.trim()))
            .collect::<Vec<_>>()
            .join("\n");
        Some(template.replace(SEGNAPOSTO_REGOLE, &elenco).trim().to_string())
    }

    pub(crate) fn len(&self) -> usize {
        self.regole.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::seed_project_meta;

    async fn inserisci(pool: &PgPool, project_id: Uuid, testo: &str, conf: f64, status: &str) {
        sqlx::query(
            "INSERT INTO nexus_learned_instructions \
               (id, project_id, category, rule_text, status, confidence, content_hash) \
             VALUES ($1, $2, 'tooling', $3, $4, $5, $6)",
        )
        .bind(Uuid::new_v4())
        .bind(project_id)
        .bind(testo)
        .bind(status)
        .bind(conf)
        // Deterministico e unico per testo: la colonna e' NOT NULL e serve
        // solo a distinguere le righe, non a essere un hash crittografico.
        .bind(format!("prova-{:x}", testo.bytes().map(u64::from).sum::<u64>()))
        .execute(pool)
        .await
        .expect("regola");
    }

    /// LA prova: solo le ATTIVE entrano, ordinate per confidenza, e il testo
    /// esce dentro l'involucro del template del DB.
    ///
    /// MUTAZIONE: togliere `AND status = 'active'` dalla query fa entrare la
    /// regola `proposed` — cioe' una proposta non ancora approvata dichiarata
    /// al modello come regola del progetto.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn entrano_solo_le_attive_in_ordine_di_confidenza(pool: PgPool) {
        let progetto = seed_project_meta(&pool).await;
        inserisci(&pool, progetto, "regola meno sicura", 0.5, "active").await;
        inserisci(&pool, progetto, "regola piu' sicura", 0.9, "active").await;
        inserisci(&pool, progetto, "solo proposta", 0.99, "proposed").await;

        let apprese = LearnedInstructions::load(&pool, progetto).await;
        assert_eq!(apprese.len(), 2, "la proposta non deve entrare");

        let blocco = apprese.section(&pool).await.expect("blocco reso");
        let pos_sicura = blocco.find("regola piu' sicura").expect("presente");
        let pos_meno = blocco.find("regola meno sicura").expect("presente");
        assert!(
            pos_sicura < pos_meno,
            "la regola con confidenza maggiore viene prima: {blocco}"
        );
        assert!(
            !blocco.contains("solo proposta"),
            "una proposta non e' una regola del progetto: {blocco}"
        );
        assert!(
            !blocco.contains(SEGNAPOSTO_REGOLE),
            "il placeholder deve essere sostituito: {blocco}"
        );
    }

    /// Un progetto senza regole non produce nessun blocco: un involucro vuoto
    /// nel system e' rumore con autorita'.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn senza_regole_nessun_blocco(pool: PgPool) {
        let progetto = seed_project_meta(&pool).await;
        let apprese = LearnedInstructions::load(&pool, progetto).await;
        assert_eq!(apprese.len(), 0);
        assert!(apprese.section(&pool).await.is_none());
    }

    /// Le regole di un ALTRO progetto non entrano: la tabella e' condivisa e il
    /// filtro per progetto e' l'unica cosa che tiene separati i mondi
    /// (CLAUDE.md sez. E).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn le_regole_di_un_altro_progetto_non_entrano(pool: PgPool) {
        let mio = seed_project_meta(&pool).await;
        let altrui = seed_project_meta(&pool).await;
        inserisci(&pool, altrui, "regola del vicino", 0.9, "active").await;

        let apprese = LearnedInstructions::load(&pool, mio).await;
        assert_eq!(apprese.len(), 0, "nessuna regola altrui");
        assert!(apprese.section(&pool).await.is_none());
    }
}
