//! Il processo operativo standard nel system prompt: punto unico (regola L) di
//! COME il processo di implementazione entra nel contesto di chi lavora sui
//! progetti gestiti — e di CHI non deve riceverlo.
//!
//! ## Perche' un compositore e non un append alle chiavi (mig 0674)
//!
//! Il testo vive in UNA riga di `nexus_prompt_templates`
//! (`system.implementation_process`). Appenderlo alle 13 chiavi dei prompt
//! avrebbe raggiunto solo le figure di OGGI: una figura creata domani dal
//! FigureWizard (che scrive `subagent.<kind>.base` a runtime) non passa da
//! nessuna migrazione e nascerebbe senza processo — la stessa lezione di
//! `prompt_memories` (in mcp-core), dove il consumo su un ramo solo lascio' l'altro
//! percorso scoperto senza che nessuno se ne accorgesse. L'innesto vive quindi
//! DENTRO i tre compositori di system prompt: la chat
//! (`chat_messages::handlers::compose_chat_system_context`), il run agentico
//! (`chat_messages::agent_run::compose_agent_system_text`, nel gruppo STABILE)
//! e il sub-run (`agent_tools::subagent_native::resolve_system_text`).
//!
//! ## Chi NON lo riceve
//!
//! Le figure advisory («chi si convoca per un PARERE non mette le mani nel
//! codice», commit 303e1437): un processo di implementazione nel system di chi
//! non implementa e' rumore con autorita'. Il discriminante e' il punto unico
//! [`nexus_types::figure_advisory::is_advisory_kind`] sulla `tool_whitelist`
//! della figura — mai un elenco di nomi. Le figure di VERDETTO (review) e di
//! sola analisi non sono advisory per quel criterio e il blocco arriva anche a
//! loro: e' il TESTO a degradare per mandato (tre perimetri dichiarati in
//! testa), non un secondo discriminante a codice.
//!
//! ## Costo sul prefisso riusabile: nessuno
//!
//! Il blocco e' STABILE (cambia solo su edit admin del template) e va appeso
//! PRIMA di qualunque blocco che introduca `CONFINE_DI_TURNO`: nel run
//! agentico sta nel gruppo `stabili` di `componi_system_di_run`, mai in coda.
//! I test di posizione nei compositori lo asseriscono.

use nexus_types::figure_advisory;
use sqlx::PgPool;

/// Tag di apertura del blocco: la FORMA vive solo qui (guard
/// `processo-operativo` in check-single-source.sh). Il testo vero e' nel DB.
pub const TAG_APERTURA: &str = "<processo_implementazione>";

/// Tag di chiusura: e' QUESTO che decide l'idempotenza. Una MENZIONE del
/// blocco in un template cita l'apertura, mai la coppia completa — decidere
/// sull'apertura renderebbe una menzione indistinguibile dal blocco vero e il
/// blocco non entrerebbe (trappola trovata dalla review avversaria del 04/08:
/// il rimando inciso in agent.coder.base conteneva il tag letterale).
pub const TAG_CHIUSURA: &str = "</processo_implementazione>";

/// Chiave del template (regola G: il testo vive nel DB, qui solo il nome).
const CHIAVE_TEMPLATE: &str = "system.implementation_process";

/// Vocabolario dei tool mutativi: lo stesso che governa il gate HITL.
const CHIAVE_MUTATORI: &str = "agent.tools.result_cache_mutators";

/// Il blocco dal DB. `None` se assente, disattivo o vuoto: una configurazione
/// assente deve VEDERSI (warn), mai essere supplita da un letterale di ripiego
/// che nessuno ha scelto (regola G).
pub async fn section(db: &PgPool) -> Option<String> {
    let template: Option<String> = sqlx::query_scalar::<_, String>(
        "SELECT content FROM nexus_prompt_templates \
          WHERE key = $1 AND is_active = true",
    )
    .bind(CHIAVE_TEMPLATE)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    match template {
        Some(t) if !t.trim().is_empty() => Some(t.trim().to_string()),
        _ => {
            tracing::warn!(
                chiave = CHIAVE_TEMPLATE,
                "template del processo operativo assente o disattivo: il blocco non entra"
            );
            None
        }
    }
}

/// Appende il blocco processo a un system prompt (idempotente sul tag).
///
/// Per la chat e per ogni testo gia' composto che ripassa di qui (resend): un
/// system gia' dotato del blocco resta invariato, mai due processi nello
/// stesso prompt.
pub async fn con_processo(db: &PgPool, system: String) -> String {
    if system.contains(TAG_CHIUSURA) {
        return system;
    }
    match section(db).await {
        Some(blocco) => {
            let mut testo = system;
            testo.push_str("\n\n");
            testo.push_str(&blocco);
            testo.push('\n');
            testo
        }
        None => system,
    }
}

/// La variante per il sub-run: decide DENTRO se la figura e' advisory.
///
/// Il vocabolario dei mutatori arriva dal DB qui (e non dal chiamante) perche'
/// questo E' il punto di decisione, e i due consumatori della domanda — la
/// creazione dal wizard e la convocazione del Consiglio — la pongono gia'
/// ciascuno al punto unico col proprio vocabolario.
///
/// Vocabolario vuoto (DB muto, chiave assente): le advisory restano
/// riconoscibili dal solo canale `advisory_verdict` (e' meta' del criterio di
/// `is_advisory_kind` che non dipende dal vocabolario); cio' che si perde e'
/// la capacita' di smascherare una "scrittrice che parla" (verdetto E
/// mutatori), che senza vocabolario passerebbe per advisory e resterebbe
/// senza processo. Il warn dichiara questa perdita; il fail-open resta verso
/// il processo per tutte le NON-advisory.
pub async fn con_processo_figura(
    db: &PgPool,
    system: String,
    tool_whitelist: &[String],
) -> String {
    let mutatori = nexus_auth::get_csv_setting(db, CHIAVE_MUTATORI).await;
    if mutatori.is_empty() {
        tracing::warn!(
            chiave = CHIAVE_MUTATORI,
            "vocabolario mutatori vuoto: una figura con advisory_verdict E tool di scrittura \
             passerebbe per advisory e resterebbe senza processo"
        );
    }
    if figure_advisory::is_advisory_kind(tool_whitelist, &mutatori) {
        return system;
    }
    con_processo(db, system).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYSTEM_DI_FIGURA: &str = "<role>Sei la figura implement.</role>";

    /// Il blocco entra e il testo viene dal DB migrato (regola O: il template lo
    /// semina la migrazione 0674 del set META, non una fixture).
    ///
    /// MUTAZIONE: svuotare il template nella 0674 (o togliere l'INSERT) fa
    /// tornare `None` da `section` e la prima asserzione cade.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_blocco_entra_e_viene_dal_db(pool: PgPool) {
        let finale = con_processo(&pool, SYSTEM_DI_FIGURA.to_string()).await;
        assert!(finale.contains(TAG_APERTURA), "{finale}");
        assert!(finale.contains("</processo_implementazione>"), "{finale}");
        // Ancore del contenuto seminato: criteri eseguibili e chiusura onesta.
        assert!(finale.contains("CRITERI DI ACCETTAZIONE"), "{finale}");
        assert!(finale.contains("task_complete"), "{finale}");
        // Il system di partenza resta intatto: si aggiunge, non si riscrive.
        assert!(finale.contains(SYSTEM_DI_FIGURA), "{finale}");
    }

    /// Due passaggi dal compositore = un blocco solo (il caso resend).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_blocco_non_si_raddoppia(pool: PgPool) {
        let una = con_processo(&pool, SYSTEM_DI_FIGURA.to_string()).await;
        let due = con_processo(&pool, una.clone()).await;
        assert_eq!(una, due);
        assert_eq!(due.matches(TAG_APERTURA).count(), 1);
    }

    /// Template disattivato dall'admin = blocco assente, system invariato:
    /// la configurazione assente si vede, non si supplisce (regola G).
    ///
    /// MUTAZIONE: aggiungere un fallback letterale in `section` fa comparire il
    /// blocco anche a template spento e l'asserzione di uguaglianza cade.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn template_disattivo_nessun_blocco(pool: PgPool) {
        sqlx::query("UPDATE nexus_prompt_templates SET is_active = false WHERE key = $1")
            .bind(CHIAVE_TEMPLATE)
            .execute(&pool)
            .await
            .expect("update is_active");
        let finale = con_processo(&pool, SYSTEM_DI_FIGURA.to_string()).await;
        assert_eq!(finale, SYSTEM_DI_FIGURA);
    }

    /// Una figura advisory (whitelist con `advisory_verdict` e zero mutatori,
    /// giudicata col vocabolario REALE del DB migrato — mig 0394, non una
    /// fixture: regola O) NON riceve il processo; una scrittrice si'.
    ///
    /// MUTAZIONE: togliere il ramo `is_advisory_kind` da `con_processo_figura`
    /// fa comparire il blocco anche all'advisory e la prima asserzione cade —
    /// e' la mutazione-specchio dell'incidente prenotazioni-sala (chi da'
    /// pareri trattato come chi scrive).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn figura_advisory_non_riceve_il_processo(pool: PgPool) {
        let advisory: Vec<String> = ["advisory_verdict", "read_file", "list_files"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let finale =
            con_processo_figura(&pool, SYSTEM_DI_FIGURA.to_string(), &advisory).await;
        assert_eq!(finale, SYSTEM_DI_FIGURA, "l'advisory ha ricevuto il processo");

        let writer: Vec<String> = ["write_file", "edit_file", "run_command", "task_complete"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let finale = con_processo_figura(&pool, SYSTEM_DI_FIGURA.to_string(), &writer).await;
        assert!(finale.contains(TAG_APERTURA), "{finale}");
    }

    /// Vocabolario mutatori vuoto: il processo si innesta comunque sulle
    /// scrittrici (fail-open VERSO il processo, documentato nel modulo).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn vocabolario_mutatori_vuoto_innesta_comunque(pool: PgPool) {
        sqlx::query("DELETE FROM settings WHERE key = $1")
            .bind(CHIAVE_MUTATORI)
            .execute(&pool)
            .await
            .expect("delete vocabolario");
        let writer: Vec<String> =
            ["write_file", "run_command"].iter().map(|s| s.to_string()).collect();
        let finale = con_processo_figura(&pool, SYSTEM_DI_FIGURA.to_string(), &writer).await;
        assert!(finale.contains(TAG_APERTURA), "{finale}");
    }
}
