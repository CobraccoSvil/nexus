//! Audit delle capability dichiarate: **chi le legge, se sono coperte, e con
//! quale prova si accerterebbero**.
//!
//! Due punti unici (regola L) che rispondono a due domande vicine e distinte:
//!   - [`copertura`] — «i modelli instradabili di questo fornitore hanno una
//!     riga di capability?». Verdetto condiviso fra il pannello (mcp-core) e il
//!     censimento a riga di comando (xtask).
//!   - [`selezionabilita`] — «i modelli di questo fornitore possono essere
//!     SCELTI, e se nessuno puo', qualcuno lo sta ancora misurando?». Stessi
//!     fatti di catalogo, terza domanda: un fornitore sano e dichiarato puo'
//!     essere fuori dal routing perche' la sua qualificazione non converge.
//!   - [`vocabolario`] — «di questa colonna, chi la legge, di chi e' la
//!     proprieta', con quale prova si accerta?».
//!
//! # Perche' un crate, e non due moduli di mcp-core
//!
//! mcp-core e' bin-only: uno strumento di misura non puo' dipenderne. Senza
//! questo crate un censimento dovrebbe RICOPIARE il criterio della copertura, e
//! due copie della stessa domanda che divergono in silenzio sono esattamente il
//! difetto che la regola O descrive — gia' accaduto qui il 2026-07-17, quando
//! uno script diagnostico ricopio' la query del claim leggendo la suite dalla
//! tabella sbagliata e riporto' «0 candidati» mentre erano 29. Stessa forma,
//! stesso rimedio di `nexus-model-eligibility`.
//!
//! # Il disegno della verifica automatica, e perche' oggi si ferma qui
//!
//! La domanda «da dove dovrebbero venire le capability, e come si accerta che
//! siano VERE» non ha una risposta sola, perche' le colonne non sono la stessa
//! cosa. Il vocabolario le separa, e le quattro strade si applicano a insiemi
//! diversi.
//!
//! **(a) Derivare dall'ESERCIZIO.** Non e' da progettare: **esiste gia', e
//! copre una colonna su 32.** `tool_capability.rs` e' il punto unico delle
//! scritture di `supports_tool_use`, con tutto cio' che una scrittura automatica
//! deve avere — due scrittori (il tool-probe e il tracking dei run reali), una
//! SOGLIA (`consecutive_tool_failures`, mai una singola osservazione), un lock
//! umano (`capability_locked`, mig 0590, distinto dalla provenienza
//! `capability_source` proprio perche' una riga dichiarata a mano non diventi
//! infalsificabile per sempre), un REASON scritto accanto al valore, e un
//! ripristino simmetrico. E' il precedente da imitare, ed e' della stessa
//! famiglia di `apply_tier`/`TierSource`.
//!   *Costo:* nessuno nuovo dove il segnale esiste gia'.
//!   *Cosa puo' sbagliare:* attribuire al modello un guasto che era del
//!   trasporto. Se ne accorge perche' il ripristino e' simmetrico: il primo
//!   successo riabilita, quindi un degrado sbagliato dura al massimo un giro.
//!   *Dove NON si applica:* vedi sotto, ed e' la parte che conta.
//!
//! **(b) Interrogare il fornitore** (liste modelli, endpoint di capability).
//!   *Costo:* basso. *Cosa puo' sbagliare:* i cataloghi dichiarano cio' che il
//!   fornitore VUOLE dire, e su Kimi la doc dichiara `tool_choice: "required"`
//!   supportato dal solo `k3` — che e' risultato vero (mig 0690, 22 astensioni
//!   su 22 per HTTP 400 sugli altri), ma e' un caso, non una garanzia. Resta
//!   una fonte di PRIMO popolamento, mai di verifica: non c'e' modo di
//!   accorgersi che il catalogo del fornitore mente se non provando.
//!
//! **(c) Batteria di qualificazione dedicata.** Esiste
//! (`mcp-core/src/model_qualification.rs`) e ha gia' morso: il 15/07/2026
//! squalificava modelli SANI perche' `evaluate_attempt` leggeva
//! `turn["result"]` mentre il produttore scrive `turn["content"]` —
//! `content_chars=0` per COSTRUZIONE, e i sette test preesistenti restavano
//! verdi perche' inventavano il turno con la stessa chiave sbagliata del codice.
//!   *Costo:* alto, e in chiamate al fornitore. *Cosa puo' sbagliare:* misurare
//!   la propria richiesta invece del modello (li' `max_tokens: 64` non bastava a
//!   un modello con thinking per scrivere «ok»). *Come si accorge:* solo se il
//!   test di contratto parte dal PRODUTTORE reale. Non e' una strada vietata, e'
//!   una strada che pretende quella disciplina.
//!
//! **(d) Lasciare le migrazioni e pretendere che la riga ci sia.** E' cio' che
//! e' implementato, in due punti: la copertura a runtime (gia' sul wire del
//! pannello) e il censimento ripetibile `cargo xtask capability-census --gate`.
//!   *Costo:* nullo. *Limite dichiarato:* pretende la RIGA, non la sua verita'.
//!
//! ## Il fatto che oggi blocca (a) sulla colonna che piu' lo meriterebbe
//!
//! `tool_choice_style` e' l'unica meccanica insieme LETTA e FALSIFICABILE, e la
//! prova che sia falsificabile esiste: la mig 0694 ha corretto
//! `mistral/magistral-small-latest` da `openai_auto` a `openai_required` sui
//! fatti — 10 verdetti espressi sotto forcing — mentre su `kimi/kimi-k2.6` il
//! catalogo aveva ragione (22 astensioni su 22, HTTP 400 «tool_choice required
//! is incompatible with thinking enabled»).
//!
//! Quella deduzione **oggi non e' piu' ripetibile**, e non per mancanza di dati:
//! l'osservazione persistita registra l'ESITO e non lo STIMOLO. Il payload di
//! `nexus_agent_meta_steps(kind='step_validation')` porta per ogni giudice
//! `provider`, `model`, `verdict`, `abstain_cause` — e nessun campo che dica se
//! `force_tool_choice` fosse acceso. Finche' il gate forzava SEMPRE, lo stimolo
//! era deducibile da una costante; da quando `forzatura_ammessa` lo rende
//! condizionato allo stile dichiarato, un verdetto espresso da una coppia
//! `openai_auto` non prova nulla — non e' stata forzata — e la stessa query che
//! ha diagnosticato il difetto produrrebbe ora dati non interpretabili. **Il fix
//! che ha chiuso il difetto ha tolto l'interpretabilita' della prova che lo
//! aveva diagnosticato**, e nessuno se ne sarebbe accorto perche' la query
//! continua a restituire righe.
//!
//! Ne segue il PRIMO passo di (a) su questa colonna, e non e' un probe: e'
//! aggiungere il campo dello stimolo accanto all'esito (regola Q — l'osservazione
//! dichiara in un CAMPO le condizioni in cui e' stata fatta). Solo dopo N giri
//! con lo stimolo registrato la scrittura automatica ha un segnale su cui
//! poggiare, e va scritta con la forma di `tool_capability`: soglia, lock,
//! reason, ripristino.
//!
//! Il tool-probe NON puo' fare da scorciatoia: `build_tool_probe_request`
//! (`model_health_probe.rs:1270`) forza la tool call **via messaggio**, e il
//! commento accanto lo dice — lo schema `generate_agent_turn` non ha un campo
//! `tool_choice`. Quel probe puo' provare `supports_tool_use` e infatti e'
//! esattamente cio' che scrive; non puo' provare `tool_choice_style`, perche'
//! non manda mai il parametro di cui si dubita. La coerenza e' notevole e va
//! preservata: **il probe scrive cio' che puo' provare, e nient'altro.**
//!
//! ## E le altre 20 colonne
//!
//! Per la maggior parte la domanda «come si accerta che sia vera» e' prematura:
//! nessuno le legge (vedi [`vocabolario`]), quindi un valore falso non produce
//! sintomi e un ciclo di verifica spenderebbe chiamate per correggere un dato
//! che non cambia il comportamento di nulla. Il caso limite e'
//! `supports_prompt_cache`: MISURATO il 10/08/2026, e' `false` per nove coppie
//! che nel ledger hanno letture di cache — `mistral/mistral-small-latest` ne ha
//! 2.461.120 su 152 chiamate. E' una dichiarazione **falsa**, ed e' innocua solo
//! perche' morta; il codice che la ignora lo dichiara gia'
//! (`nexus-gateway/src/providers/generic.rs:35-40`). Per quelle colonne il
//! rimedio non e' automatizzarne la scrittura: e' collegarle o rimuoverle.
//!
//! Un terzo gruppo non e' accertabile per costruzione, perche' non descrive il
//! fornitore: `history_keep_recent_messages`, `soft_failure_*`,
//! `tool_result_max_*`, i timeout. Sono NOSTRA politica in una tabella che si
//! chiama capability; il posto di una politica e' `settings` (regola G), e
//! nessun esperimento puo' dire se il suo valore sia «vero».

pub mod copertura;
pub mod selezionabilita;
pub mod vocabolario;

pub use copertura::{
    carica_fatti_catalogo, classifica_dichiarazione, DeclarationCoverage, ModelFact,
    SQL_FATTI_CATALOGO,
};
pub use selezionabilita::{
    classifica_selezionabilita, ProviderSelectability, PREFISSO_ROUND_NON_MISURANTE,
};
pub use vocabolario::{
    colonna, senza_lettore, Accertamento, ColonnaCapability, Lettura, Proprieta, COLONNE,
};

#[cfg(test)]
mod tests {
    use super::vocabolario::{colonna, COLONNE};

    /// IL GUARD del vocabolario (regola O). Le colonne non si elencano a
    /// memoria: si confrontano con quelle che le migrazioni VERE producono. Una
    /// colonna aggiunta domani rende rosso questo test finche' qualcuno non ne
    /// dichiara chi la legge e come si accerta — che e' il passo che oggi manca
    /// all'onboarding di una capability, e che nessun guard testuale puo'
    /// pretendere perche' la vista la costruisce il DB, non il sorgente.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn vocabolario_copre_la_vista_reale(pool: sqlx::PgPool) {
        let reali: Vec<String> = sqlx::query_scalar(
            "SELECT column_name::text FROM information_schema.columns \
              WHERE table_name = 'v_model_capabilities' ORDER BY ordinal_position",
        )
        .fetch_all(&pool)
        .await
        .expect("colonne della vista");

        assert!(
            !reali.is_empty(),
            "la vista non esiste nello schema migrato: senza questa premessa il \
             confronto sarebbe verde per assenza"
        );

        let mancanti: Vec<&String> = reali.iter().filter(|n| colonna(n).is_none()).collect();
        assert!(
            mancanti.is_empty(),
            "colonne della vista non dichiarate nel vocabolario: {mancanti:?}. \
             Chi aggiunge una capability ne dichiara qui chi la legge, di chi e' \
             la proprieta' e con quale prova si accerta."
        );

        let inventate: Vec<&str> = COLONNE
            .iter()
            .map(|c| c.nome)
            .filter(|n| !reali.iter().any(|r| r == n))
            .collect();
        assert!(
            inventate.is_empty(),
            "colonne dichiarate che la vista non ha: {inventate:?}. Un vocabolario \
             che nomina cio' che non esiste descrive uno schema immaginario."
        );

        assert_eq!(
            reali.len(),
            COLONNE.len(),
            "il vocabolario deve coprire la vista ESATTAMENTE, senza duplicati"
        );
    }
}
