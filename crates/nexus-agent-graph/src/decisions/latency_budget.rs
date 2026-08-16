//! `latency_budget`: il criterio PURO con cui la latenza OSSERVATA di una
//! coppia (provider, model) entra nella selezione, quando il chiamante
//! DICHIARA un budget.
//!
//! # Il difetto che chiude
//!
//! La selezione non sapeva quanto e' lento un fornitore, e chi lo sapeva
//! (lo storico dei probe, `ai_model_health_history.latency_ms`) non aveva
//! lettori sulla strada della scelta: il gate duale convocava validatori con
//! p95 osservato sopra il proprio timeout per validatore, e ogni convocazione
//! bruciava un'astensione `timeout` per costruzione (misurato il 13/08/2026:
//! kimi con p95 22-26s contro il cap per-attempt di 72s regge, ma con un
//! timeout amministrativo piu' stretto la convocazione e' persa in partenza).
//! Il rimedio NON e' alzare il timeout per inseguire il lento (la toppa che
//! la regola H vieta per nome): e' che la selezione SEGUA la configurazione —
//! chi dichiara quanto puo' aspettare non riceve chi, ai fatti, non arriva in
//! tempo.
//!
//! # La politica: esclusione con ricaduta dichiarata
//!
//! - un candidato con percentile osservato OLTRE il budget e' escluso dal
//!   pool ([`LatencyFit::Exceeds`]);
//! - l'IGNOTO non esclude (regola Q): nessuna osservazione, o campioni sotto
//!   la soglia, e' [`LatencyFit::Unknown`] — «non ho misurato» non e' «e'
//!   lento», e un criterio che escludesse al buio fermerebbe ogni modello
//!   appena entrato a catalogo;
//! - se il filtro SVUOTA il pool si serve il pool INTERO, dichiarandolo
//!   ([`EsitoBudgetLatenza::RicadutaPoolPieno`] + [`SEGNALE_RICADUTA`] nel
//!   rationale): un budget e' una preferenza informata, mai un fail-closed —
//!   meglio un giudice lento di nessun giudice, e chi legge l'esito VEDE che
//!   la ricaduta e' avvenuta invece di dedurla dal comportamento.
//!
//! I FATTI arrivano dall'I/O di mcp-core (`latency_telemetry`:
//! `percentile_cont` sui probe sani in finestra, config `routing.latency.*`,
//! mig 0725); qui vive solo il criterio, golden-abile in isolamento — stessa
//! divisione di [`super::governance`], che della latenza fa una penalita' di
//! riordino: questo modulo risponde a un'altra domanda («sta DENTRO un budget
//! dichiarato?»), non a «chi e' preferibile a parita' di ammissibilita'?».

/// La latenza osservata di UNA coppia (provider, model): il percentile
/// configurato (`routing.latency.percentile`) sui probe sani in finestra, e
/// quanti campioni lo sostengono. I campi sono la MISURA, non il verdetto: il
/// verdetto lo produce [`latency_fit`], che conosce anche la soglia minima di
/// campioni.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LatencyObservation {
    /// Il percentile osservato, in millisecondi.
    pub p_ms: i64,
    /// Quanti probe sostengono la misura dentro la finestra.
    pub samples: i64,
}

/// Il verdetto del criterio su UN candidato (regola Q: l'ignoto e' una
/// variante dichiarata, mai un bool a due valori per una domanda che ne ha
/// tre).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatencyFit {
    /// Il percentile osservato sta dentro il budget: convocabile.
    Fits,
    /// Il percentile osservato ECCEDE il budget: escluso dal pool. Porta i
    /// due numeri del confronto, cosi' chi legge il log rifa' il conto invece
    /// di dedurlo (regola M).
    Exceeds { p_ms: i64, budget_ms: i64 },
    /// Nessuna osservazione, o campioni sotto soglia: non si decide al buio.
    /// NON esclude.
    Unknown,
}

/// L'esito dell'applicazione del budget a un POOL di candidati.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EsitoBudgetLatenza {
    /// Il filtro ha tenuto almeno un candidato (`esclusi` puo' essere 0:
    /// budget dichiarato, nessuno oltre).
    Filtrato { esclusi: usize },
    /// TUTTI i candidati osservati eccedono il budget: si serve il pool
    /// INTERO, e il segnale viaggia nel rationale ([`SEGNALE_RICADUTA`]).
    RicadutaPoolPieno { oltre_budget: usize },
}

/// Il suffisso strutturato che la ricaduta appende al `rationale` della
/// scelta. UN solo letterale (regola N): chi lo legge nei log o nei test lo
/// importa da qui, mai una seconda stringa.
pub const SEGNALE_RICADUTA: &str = "latency=overbudget_fallback";

impl EsitoBudgetLatenza {
    /// Il segnale da appendere al rationale: solo la ricaduta parla. Il testo
    /// si compone DAI campi (regola Q), mai il contrario.
    pub fn segnale(&self) -> Option<&'static str> {
        match self {
            EsitoBudgetLatenza::RicadutaPoolPieno { .. } => Some(SEGNALE_RICADUTA),
            EsitoBudgetLatenza::Filtrato { .. } => None,
        }
    }
}

/// L'esito del filtro su un pool: QUALI indici sopravvivono (nell'ordine di
/// ingresso) e che cosa e' successo. Gli indici e non le righe: il criterio
/// non conosce — e non deve conoscere — la forma delle righe del chiamante.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FiltroLatenza {
    /// Indici del pool d'ingresso da servire, ordine preservato.
    pub keep: Vec<usize>,
    pub esito: EsitoBudgetLatenza,
}

/// Il verdetto su UN candidato. `min_samples` e' la soglia sotto cui una
/// misura non e' una misura (pochi probe = rumore): sotto soglia il verdetto
/// e' `Unknown`, non un `Fits` di comodo ne' un `Exceeds` al buio.
///
/// Il confine e' INCLUSIVO sul budget (`p_ms == budget_ms` -> `Fits`): il
/// budget dice «entro quanto», e chi arriva esattamente al limite arriva.
pub fn latency_fit(
    obs: Option<&LatencyObservation>,
    budget_ms: i64,
    min_samples: i64,
) -> LatencyFit {
    let Some(o) = obs else {
        return LatencyFit::Unknown;
    };
    if o.samples < min_samples {
        return LatencyFit::Unknown;
    }
    if o.p_ms > budget_ms {
        LatencyFit::Exceeds {
            p_ms: o.p_ms,
            budget_ms,
        }
    } else {
        LatencyFit::Fits
    }
}

/// Applica il budget a un POOL: esclude gli `Exceeds`, tiene `Fits` e
/// `Unknown` (regola Q), e se il filtro svuota il pool RICADE sul pool intero
/// dichiarandolo. `osservazioni` e' parallelo al pool (una voce per
/// candidato, `None` = mai osservato).
pub fn filtra_per_budget(
    osservazioni: &[Option<LatencyObservation>],
    budget_ms: i64,
    min_samples: i64,
) -> FiltroLatenza {
    let mut keep = Vec::with_capacity(osservazioni.len());
    let mut oltre = 0usize;
    for (i, obs) in osservazioni.iter().enumerate() {
        match latency_fit(obs.as_ref(), budget_ms, min_samples) {
            LatencyFit::Exceeds { .. } => oltre += 1,
            LatencyFit::Fits | LatencyFit::Unknown => keep.push(i),
        }
    }
    if keep.is_empty() && !osservazioni.is_empty() {
        // Pool svuotato: si serve il pool INTERO, mai fail-closed. Un budget
        // che lasciasse la selezione senza modelli trasformerebbe una
        // preferenza in un guasto — l'esatto contrario del suo scopo.
        return FiltroLatenza {
            keep: (0..osservazioni.len()).collect(),
            esito: EsitoBudgetLatenza::RicadutaPoolPieno {
                oltre_budget: oltre,
            },
        };
    }
    FiltroLatenza {
        keep,
        esito: EsitoBudgetLatenza::Filtrato { esclusi: oltre },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(p_ms: i64, samples: i64) -> Option<LatencyObservation> {
        Some(LatencyObservation { p_ms, samples })
    }

    /// Test 2 del design (regola Q): l'ignoto NON esclude — ne' l'assenza di
    /// osservazione, ne' i campioni sotto soglia, anche se la poca storia che
    /// c'e' racconta un modello lento.
    ///
    /// MUTAZIONE: se `latency_fit` trattasse l'ignoto come `Exceeds` (o i
    /// campioni scarsi come misura valida), una delle due asserzioni
    /// rosseggia.
    #[test]
    fn latenza_ignota_non_esclude() {
        assert_eq!(latency_fit(None, 10_000, 5), LatencyFit::Unknown);
        // 2 campioni a 30s con soglia 5: non e' una misura, e' rumore.
        assert_eq!(
            latency_fit(obs(30_000, 2).as_ref(), 10_000, 5),
            LatencyFit::Unknown
        );
    }

    /// Il confronto e' inclusivo sul budget e i due numeri viaggiano nel
    /// verdetto (regola M: chi legge rifa' il conto).
    ///
    /// MUTAZIONE: `>` che diventa `>=` fa rosseggiare il caso al limite;
    /// un `Exceeds` senza i numeri non compila (i campi sono obbligatori).
    #[test]
    fn il_confine_del_budget_e_inclusivo() {
        assert_eq!(latency_fit(obs(10_000, 5).as_ref(), 10_000, 5), LatencyFit::Fits);
        assert_eq!(
            latency_fit(obs(10_001, 5).as_ref(), 10_000, 5),
            LatencyFit::Exceeds {
                p_ms: 10_001,
                budget_ms: 10_000
            }
        );
        assert_eq!(latency_fit(obs(2_000, 5).as_ref(), 10_000, 5), LatencyFit::Fits);
    }

    /// Il filtro esclude i soli `Exceeds`, preserva l'ordine e tiene
    /// l'ignoto.
    ///
    /// MUTAZIONE: se il filtro escludesse anche gli `Unknown` (il criterio
    /// che decide al buio), l'indice 2 sparisce e il test rosseggia.
    #[test]
    fn il_filtro_esclude_i_soli_oltre_budget() {
        let oss = vec![
            obs(30_000, 10), // 0: oltre -> escluso
            obs(2_000, 10),  // 1: dentro -> resta
            None,            // 2: ignoto -> resta (regola Q)
            obs(9_000, 2),   // 3: campioni sotto soglia -> ignoto -> resta
        ];
        let f = filtra_per_budget(&oss, 10_000, 5);
        assert_eq!(f.keep, vec![1, 2, 3]);
        assert_eq!(f.esito, EsitoBudgetLatenza::Filtrato { esclusi: 1 });
        assert_eq!(f.esito.segnale(), None, "il filtro riuscito non segnala nulla");
    }

    /// Test 3 del design (parte pura): tutti oltre budget -> pool INTERO
    /// servito, con l'esito che lo dichiara e il segnale canonico per il
    /// rationale.
    ///
    /// MUTAZIONE: fail-closed (keep vuoto sulla ricaduta) -> il test
    /// rosseggia su `keep`; segnale assente -> rosseggia sull'ultima
    /// asserzione.
    #[test]
    fn pool_svuotato_ricade_dichiarando() {
        let oss = vec![obs(30_000, 10), obs(20_000, 10)];
        let f = filtra_per_budget(&oss, 5_000, 5);
        assert_eq!(f.keep, vec![0, 1], "la ricaduta serve il pool INTERO, mai vuoto");
        assert_eq!(
            f.esito,
            EsitoBudgetLatenza::RicadutaPoolPieno { oltre_budget: 2 }
        );
        assert_eq!(f.esito.segnale(), Some(SEGNALE_RICADUTA));
    }

    /// Pool vuoto in ingresso: nessuna ricaduta da inventare (non c'e' nulla
    /// da servire), esito `Filtrato { 0 }`.
    #[test]
    fn pool_vuoto_resta_vuoto() {
        let f = filtra_per_budget(&[], 5_000, 5);
        assert!(f.keep.is_empty());
        assert_eq!(f.esito, EsitoBudgetLatenza::Filtrato { esclusi: 0 });
    }
}
