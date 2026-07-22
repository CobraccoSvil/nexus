//! Punto unico dei timeout delle chiamate LLM (regola L + regola G).
//!
//! # Perche' esiste
//!
//! I timeout erano decisi in due crate senza che nessuno guardasse la relazione
//! fra loro, e la gerarchia risultante era INVERTITA:
//!
//! | cosa | valore | dove |
//! |---|---|---|
//! | run di una figura (N turni) | 300s | `orchestrator.subagent_default_timeout_s` |
//! | UNA chiamata gateway -> provider | 300s | `max(complete 120, stream 300)` |
//! | mcp-core -> gateway (con retry) | 435s | `120*3+45+30` |
//!
//! Il budget di UNA chiamata era `>=` al budget dell'INTERO run multi-turno che
//! la contiene: una singola chiamata appesa consumava il 100% della vita del
//! run, che moriva per `RunTimeout` con **zero iterazioni completate** (`it=0`).
//! Non era un difetto del modello: era aritmetica. Il sintomo veniva attribuito
//! di volta in volta al modello di turno (z-ai/glm-4.7-flash, poi deepseek, poi
//! google), perche' l'innesco (una chiamata lenta) e' casuale mentre la
//! conseguenza e' deterministica.
//!
//! In piu' `gateway.complete_timeout_seconds` (120) NON aveva alcun effetto: era
//! usata solo dentro un `max(complete, stream)` che la scartava sempre (300 >
//! 120), e nel gateway non esisteva NESSUN timeout logico per-richiesta. Una
//! setting viva nel DB, letta a ogni avvio, e inerte.
//!
//! # Il contratto
//!
//! Tutto deriva da due grandezze primarie lette dal DB (regola G):
//!   * `orchestrator.subagent_default_timeout_s` -> il run PIU' CORTO che
//!     contiene chiamate LLM (le figure del consiglio). E' il vincolo piu'
//!     stretto, quindi il riferimento conservativo per tutti.
//!   * `agent.llm.min_guaranteed_turns` -> quanti turni il run deve poter
//!     completare **anche nel caso peggiore** in cui ogni chiamata esaurisce il
//!     proprio budget.
//!
//! e ne discendono, con l'invariante `request_budget * min_turns <= run_timeout`
//! garantito PER COSTRUZIONE (vedi [`LlmTimeouts::derive`] e i test):
//!   * `request_budget` — deadline end-to-end di UNA `/v1/complete`, retry e
//!     chain inclusi. E' il numero che impedisce a una chiamata di mangiarsi il
//!     run.
//!   * `per_attempt` — cap su un singolo `provider.complete()`, cosi' un
//!     provider appeso non brucia il budget dell'intera chain.
//!   * `client_budget` — quanto mcp-core attende il gateway (budget + margine).
//!
//! `gateway.complete_timeout_seconds` torna EFFICACE come cap per-tentativo, ma
//! solo nella direzione che conta: puo' STRINGERE (`min`), mai sforare il
//! budget. Alzarla oltre `request_budget` non ha effetto — ed e' giusto cosi':
//! nessuna setting deve poter violare l'invariante.

use std::time::Duration;

use sqlx::PgPool;

/// Turni che un run deve poter completare nel caso peggiore. Seed: mig **0587**.
pub const DEFAULT_MIN_GUARANTEED_TURNS: u64 = 4;
/// Cap per-tentativo verso il provider (completion non-streaming). Seed: mig **0586**.
pub const DEFAULT_COMPLETE_TIMEOUT_SECS: u64 = 120;
/// Timeout dello streaming SSE verso il provider. Seed: mig **0586**.
pub const DEFAULT_STREAM_TIMEOUT_SECS: u64 = 300;
/// Timeout del run di un subagente. Seed: `orchestrator.subagent_default_timeout_s`.
pub const DEFAULT_RUN_TIMEOUT_SECS: u64 = 300;
/// Margine di rete/serializzazione sopra `request_budget` per il client
/// mcp-core -> gateway: il client non deve mollare PRIMA che il gateway abbia
/// avuto modo di rispondere entro il proprio budget (altrimenti il gateway
/// lavora per un chiamante che non c'e' piu').
pub const CLIENT_BUDGET_MARGIN_SECS: u64 = 15;
/// Sotto i 2 turni garantiti il concetto stesso di run multi-turno non esiste.
const MIN_TURNS_FLOOR: u64 = 2;

/// Chiave DB: turni minimi garantiti per un run.
pub const KEY_MIN_GUARANTEED_TURNS: &str = "agent.llm.min_guaranteed_turns";
/// Chiave DB: cap per-tentativo completion.
pub const KEY_COMPLETE_TIMEOUT: &str = "gateway.complete_timeout_seconds";
/// Chiave DB: timeout streaming.
pub const KEY_STREAM_TIMEOUT: &str = "gateway.stream_timeout_seconds";
/// Chiave DB: timeout del run di un subagente (il run piu' corto).
pub const KEY_RUN_TIMEOUT: &str = "orchestrator.subagent_default_timeout_s";

/// I timeout LLM derivati, coerenti fra loro per costruzione.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LlmTimeouts {
    /// Budget dell'intero run multi-turno (riferimento: il piu' corto).
    pub run_timeout: Duration,
    /// Deadline end-to-end di UNA `/v1/complete` (retry + chain inclusi).
    pub request_budget: Duration,
    /// Cap su un singolo `provider.complete()`.
    pub per_attempt: Duration,
    /// Attesa di mcp-core verso il gateway.
    pub client_budget: Duration,
    /// Timeout dello streaming SSE.
    pub stream_timeout: Duration,
    /// Turni minimi garantiti usati nella derivazione.
    pub min_guaranteed_turns: u64,
}

impl LlmTimeouts {
    /// Derivazione PURA (niente IO): l'invariante si testa senza DB.
    ///
    /// Garantisce `request_budget * min_turns <= run_timeout`: e' il vincolo che
    /// impedisce a una singola chiamata di consumare l'intero run.
    pub fn derive(
        run_timeout_secs: u64,
        complete_secs: u64,
        stream_secs: u64,
        min_turns: u64,
    ) -> Self {
        let run = run_timeout_secs.max(1);
        let turns = min_turns.max(MIN_TURNS_FLOOR);
        // Divisione intera: arrotonda per DIFETTO, quindi l'invariante
        // budget*turns <= run resta vero anche quando run non e' divisibile.
        let budget = (run / turns).max(1);
        // Il cap per-tentativo puo' solo STRINGERE il budget, mai sforarlo.
        let per_attempt = complete_secs.max(1).min(budget);
        Self {
            run_timeout: Duration::from_secs(run),
            request_budget: Duration::from_secs(budget),
            per_attempt: Duration::from_secs(per_attempt),
            client_budget: Duration::from_secs(budget.saturating_add(CLIENT_BUDGET_MARGIN_SECS)),
            stream_timeout: Duration::from_secs(stream_secs.max(1)),
            min_guaranteed_turns: turns,
        }
    }

    /// Timeout del client reqwest CONDIVISO fra completion e streaming: deve
    /// coprire il caso piu' lungo (lo streaming), perche' e' un tetto di
    /// trasporto, non il budget applicativo. Le completion sono limitate dalla
    /// deadline logica (`request_budget`/`per_attempt`), non da questo valore.
    pub fn client_http_timeout(&self) -> Duration {
        self.per_attempt.max(self.stream_timeout)
    }

    /// Valori di default (nessun DB disponibile): stessa derivazione, stessi
    /// invarianti. Niente numeri magici sparsi nei costruttori.
    pub fn defaults() -> Self {
        Self::derive(
            DEFAULT_RUN_TIMEOUT_SECS,
            DEFAULT_COMPLETE_TIMEOUT_SECS,
            DEFAULT_STREAM_TIMEOUT_SECS,
            DEFAULT_MIN_GUARANTEED_TURNS,
        )
    }

    /// Risolve dal DB (regola G: unica fonte). Ogni chiave mancante o non
    /// parsabile ricade sul proprio default, poi la derivazione riallinea il
    /// tutto: nessuna combinazione di settings puo' violare l'invariante.
    pub async fn resolve(db: &PgPool) -> Self {
        Self::resolve_for_run(db, None).await
    }

    /// Come [`resolve`], ma per un run di durata NOTA.
    ///
    /// L'invariante `request_budget * min_turns <= run_timeout` vale solo
    /// rispetto al run su cui e' stata calcolata. `resolve` usa il default
    /// globale (`orchestrator.subagent_default_timeout_s`, 300s), ma le figure
    /// hanno il PROPRIO `nexus_subagent_definitions.timeout_s`: `review` ne ha
    /// 240, `implement` 600. Con `min_turns = 4` il budget derivato dal globale
    /// e' 75s, quindi a un `review` venivano promessi 4 turni da 75s = 300s
    /// dentro un run che ne dura 240: l'invariante era verificata contro un run
    /// che nessuna figura possiede davvero, e la figura veniva uccisa dal
    /// cronometro credendo di avere ancora turni a disposizione.
    ///
    /// Passando qui la durata reale, i turni garantiti tornano a essere una
    /// promessa mantenibile (240/4 = 60s per turno). NON allunga nulla: stringe
    /// il budget della singola chiamata quando il run e' piu' corto del default.
    pub async fn resolve_for_run(db: &PgPool, run_timeout_secs: Option<u64>) -> Self {
        let run = match run_secs_utile(run_timeout_secs) {
            Some(reale) => reale,
            None => setting_u64(db, KEY_RUN_TIMEOUT, DEFAULT_RUN_TIMEOUT_SECS).await,
        };
        let complete = setting_u64(db, KEY_COMPLETE_TIMEOUT, DEFAULT_COMPLETE_TIMEOUT_SECS).await;
        let stream = setting_u64(db, KEY_STREAM_TIMEOUT, DEFAULT_STREAM_TIMEOUT_SECS).await;
        let turns = setting_u64(db, KEY_MIN_GUARANTEED_TURNS, DEFAULT_MIN_GUARANTEED_TURNS).await;
        Self::derive(run, complete, stream, turns)
    }
}

async fn setting_u64(db: &PgPool, key: &str, default: u64) -> u64 {
    crate::get_setting(db, key)
        .await
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

/// La durata di run da usare, quando e' NOTA. `None` significa "non la so,
/// chiedila al DB": e' la sola porta d'ingresso del default globale.
///
/// Estratta perche' la SCELTA della sorgente e' il punto in cui il difetto
/// vive, e dentro `resolve_for_run` sarebbe verificabile solo con un DB —
/// cioe' mai (regola O).
fn run_secs_utile(run_timeout_secs: Option<u64>) -> Option<u64> {
    run_timeout_secs.filter(|&s| s > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// L'INVARIANTE. Questo test e' la ragione per cui il modulo esiste: se
    /// qualcuno reintroduce un budget >= al run, qui diventa rosso.
    #[test]
    fn una_chiamata_non_puo_mangiarsi_il_run() {
        for run in [60_u64, 120, 300, 301, 599, 900] {
            for turns in [0_u64, 1, 2, 3, 4, 7, 10] {
                for complete in [1_u64, 30, 120, 500] {
                    let t = LlmTimeouts::derive(run, complete, 300, turns);
                    let budget = t.request_budget.as_secs();
                    let effective_turns = t.min_guaranteed_turns;
                    assert!(
                        budget * effective_turns <= t.run_timeout.as_secs(),
                        "budget {budget}s x {effective_turns} turni sfora il run \
                         {run}s (complete={complete})"
                    );
                    assert!(
                        t.per_attempt <= t.request_budget,
                        "il cap per-tentativo non puo' superare il budget della richiesta"
                    );
                }
            }
        }
    }

    /// La regressione storica, in numeri: coi valori LIVE del DB il budget di
    /// una chiamata era 300s contro un run di 300s (it=0 garantito).
    #[test]
    fn i_valori_storici_non_producono_piu_budget_pari_al_run() {
        let t = LlmTimeouts::derive(300, 120, 300, DEFAULT_MIN_GUARANTEED_TURNS);
        assert_eq!(t.request_budget, Duration::from_secs(75));
        assert_eq!(t.per_attempt, Duration::from_secs(75));
        assert_eq!(t.client_budget, Duration::from_secs(90));
        // Il punto: il client non attende piu' del run che lo contiene (era 435 > 300).
        assert!(t.client_budget < t.run_timeout);
    }

    /// `complete_timeout_seconds` deve poter STRINGERE (prima era inerte: il
    /// `max(120, 300)` la scartava sempre).
    #[test]
    fn il_cap_per_tentativo_e_efficace_solo_in_restrizione() {
        let stretto = LlmTimeouts::derive(300, 30, 300, 4);
        assert_eq!(stretto.per_attempt, Duration::from_secs(30), "deve stringere");
        let largo = LlmTimeouts::derive(300, 1000, 300, 4);
        assert_eq!(
            largo.per_attempt,
            largo.request_budget,
            "non puo' sforare il budget"
        );
    }

    /// Il client HTTP e' condiviso: deve coprire lo streaming, che e' piu' lungo
    /// del cap per-tentativo delle completion.
    #[test]
    fn il_client_condiviso_copre_lo_streaming() {
        let t = LlmTimeouts::derive(300, 120, 300, 4);
        assert_eq!(t.client_http_timeout(), Duration::from_secs(300));
    }

    /// Il difetto che `resolve_for_run` chiude: l'invariante veniva verificata
    /// contro un run che la figura non possiede.
    ///
    /// `orchestrator.subagent_default_timeout_s` vale 300, ma la figura `review`
    /// ha `timeout_s = 240` in `nexus_subagent_definitions`. Derivando dal
    /// default, a un review venivano promessi 4 turni da 75s = 300s dentro un
    /// cronometro che scade a 240: il quarto turno non esisteva, e la figura
    /// veniva uccisa mentre credeva di avere ancora budget. Con la durata reale
    /// i turni tornano a essere una promessa mantenibile.
    #[test]
    fn il_budget_deve_nascere_dal_run_reale_non_dal_default_globale() {
        let review_reale = 240_u64;

        let dal_default = LlmTimeouts::derive(
            DEFAULT_RUN_TIMEOUT_SECS,
            120,
            300,
            DEFAULT_MIN_GUARANTEED_TURNS,
        );
        assert!(
            dal_default.request_budget.as_secs() * dal_default.min_guaranteed_turns > review_reale,
            "premessa del difetto: il budget derivato dal default sfora il run \
             vero della figura review"
        );

        let dal_reale =
            LlmTimeouts::derive(review_reale, 120, 300, DEFAULT_MIN_GUARANTEED_TURNS);
        assert_eq!(dal_reale.request_budget, Duration::from_secs(60));
        assert!(
            dal_reale.request_budget.as_secs() * dal_reale.min_guaranteed_turns <= review_reale,
            "coi 240s reali i turni garantiti devono starci dentro"
        );
        assert!(
            dal_reale.client_budget < Duration::from_secs(review_reale),
            "nemmeno l'attesa del client puo' superare il run della figura"
        );
    }

    /// La durata nota vince sul default; solo l'assenza (o uno zero, che nel DB
    /// significa "non impostato") lascia parlare il setting globale.
    #[test]
    fn la_durata_nota_vince_sul_default_globale() {
        assert_eq!(run_secs_utile(Some(240)), Some(240));
        assert_eq!(
            run_secs_utile(Some(0)),
            None,
            "timeout_s = 0 e' 'non impostato', non 'run istantaneo'"
        );
        assert_eq!(run_secs_utile(None), None);
    }

    /// Un run piu' LUNGO del default non deve essere stretto dal default: la
    /// figura `implement` ha `timeout_s = 600` e i suoi turni valgono 150s.
    #[test]
    fn un_run_piu_lungo_del_default_ottiene_il_suo_budget() {
        let t = LlmTimeouts::derive(600, 1000, 300, DEFAULT_MIN_GUARANTEED_TURNS);
        assert_eq!(t.request_budget, Duration::from_secs(150));
    }

    #[test]
    fn turni_sotto_il_pavimento_sono_clampati() {
        assert_eq!(LlmTimeouts::derive(300, 120, 300, 0).min_guaranteed_turns, 2);
        assert_eq!(LlmTimeouts::derive(300, 120, 300, 1).min_guaranteed_turns, 2);
    }
}
