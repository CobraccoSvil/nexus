//! `capienza_tpm`: il criterio PURO con cui il budget di TOKEN AL MINUTO
//! DICHIARATO da un fornitore entra nella selezione, quando il chiamante
//! DICHIARA quanto e' grossa la richiesta che sta per mandare.
//!
//! # Il difetto che chiude, misurato il 17/08/2026 in esercizio
//!
//! Run reale dalla UI, contesto ~180.000 token. La selezione ha scelto
//! `groq/openai/gpt-oss-20b` e ha preso HTTP 429: «Rate limit reached ...
//! service tier 'on_demand' on tokens per minute (TPM): Limit 8000, Used
//! 5503». Il dato che avrebbe evitato il tentativo era GIA' IN CASA, scritto
//! dal sensore della mig 0718 un minuto prima:
//!
//! ```text
//! groq | openai/gpt-oss-20b | tokens_limit 8000 | tokens_remaining 120
//!      | tokens_reset_at 14:59:18 | observed_at 14:58:19
//! ```
//!
//! Sono DUE fatti distinti, e il secondo e' peggiore del primo:
//!
//! 1. **residuo istantaneo insufficiente** (120 token su 8000): temporaneo,
//!    passa da solo al reset;
//! 2. **limite STRUTTURALE**: groq dichiara 8000 TPM e quel run porta 180.000
//!    token di contesto. Per i turni grossi quella coppia non e' una scelta
//!    valida MAI — non per una congestione, per costruzione.
//!
//! Il sensore della mig 0718 nacque dichiarando «solo telemetria, nessuna
//! decisione automatica»: la scelta era giusta allora (prima si osserva, poi
//! si decide), ma l'osservazione adesso c'e' ed e' misurata.
//!
//! # La forma: gemello di [`super::latency_budget`]
//!
//! La' il budget di TEMPO dichiarato dal chiamante, qui il budget di TOKEN AL
//! MINUTO dichiarato dal fornitore. Stessa divisione (qui il criterio PURO, i
//! FATTI stanno nell'I/O di mcp-core `tpm_telemetry`), stessa disciplina
//! sull'ignoto, stessa ricaduta dichiarata a pool svuotato.
//!
//! # L'asimmetria e' voluta
//!
//! - **[`VerdettoCapienza::OltreIlLimite`] ESCLUDE**: mandare la' quella
//!   richiesta e' un 429 CERTO, e nessun'attesa lo cambia.
//! - **[`VerdettoCapienza::ResiduoInsufficiente`] RETROCEDE** in coda invece
//!   di escludere: fra qualche decina di secondi quel candidato torna valido,
//!   e se e' l'unico rimasto e' meglio provarlo che non avere nessuno.
//! - **[`VerdettoCapienza::Ignota`] non tocca nulla** (regola Q): «non ho
//!   guardato» non e' «non ci sta». Deepseek e openrouter non mandano affatto
//!   quegli header, e non devono essere penalizzati per questo.
//!
//! Il verso dell'errore e' quello di provare una volta di troppo, mai di
//! escludere al buio.
//!
//! # Due scostamenti dal design, entrambi verso l'ignoto DICHIARATO
//!
//! Il design elencava tre soli motivi di [`MotivoIgnota`] e un `reset_fra_s`
//! scalare. Sui dati reali del 17/08 servono quattro motivi e un residuo
//! opzionale:
//!
//! - `tokens_limit` dichiarato ma `tokens_remaining` no: il caso strutturale
//!   resta giudicabile (serve il solo limite), il residuo no. Rispondere
//!   `Capiente` sarebbe affermare cio' che non si e' guardato, quindi
//!   [`MotivoIgnota::ResiduoNonDichiarato`].
//! - `tokens_reset_at` assente: MISURATO, mistral manda limite e residuo ma
//!   MAI l'istante di reset. «Il residuo non basta adesso» resta vero e la
//!   retrocessione vale lo stesso; «fra quanto torna valido» non lo sappiamo,
//!   e un numero inventato li' e' esattamente il tipo di bugia silenziosa che
//!   la regola Q vieta. Da qui `reset_fra_s: Option<i64>`.

use chrono::{DateTime, Utc};

/// Cio' che il sensore ha osservato per UNA coppia (fornitore, modello):
/// l'ultima riga di `nexus_rate_limit_observations` (mig 0718). I campi sono
/// la MISURA cosi' come il wire l'ha dichiarata, non il verdetto: il verdetto
/// lo produce [`capienza`], che conosce anche la soglia di freschezza.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OsservazioneTpm {
    /// Il tetto di token al minuto dichiarato dal fornitore. `None` = il
    /// fornitore non manda quell'header (deepseek, openrouter, perplexity).
    pub tokens_limit: Option<i64>,
    /// Quanti token restavano nel bucket all'istante dell'osservazione.
    pub tokens_remaining: Option<i64>,
    /// Quando il bucket si rigenera. `None` = non dichiarato (mistral).
    pub tokens_reset_at: Option<DateTime<Utc>>,
    /// Quando l'osservazione e' stata presa: e' cio' che la rende (o no) una
    /// descrizione del minuto CORRENTE.
    pub observed_at: DateTime<Utc>,
}

/// Perche' non si sa dire nulla della capienza. Quattro cause con quattro
/// rimedi diversi (regola Q: l'ignoto e' una variante dichiarata, mai un
/// `Capiente` di comodo che affermerebbe cio' che non si e' guardato).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotivoIgnota {
    /// Nessuna riga per questa coppia: il sensore non l'ha mai vista.
    MaiOsservata,
    /// C'e' una riga, ma vecchia: un residuo di mezz'ora fa non descrive il
    /// minuto corrente. Porta l'eta' in secondi, cosi' chi legge rifa' il
    /// conto invece di dedurlo (regola M).
    OsservazioneScaduta { eta_s: i64 },
    /// Il fornitore non dichiara un tetto di token (o ne dichiara uno non
    /// positivo, che non e' un tetto): senza limite non c'e' capienza da
    /// misurare.
    LimiteNonDichiarato,
    /// Il tetto c'e' e la richiesta ci sta dentro, ma il residuo istantaneo
    /// non e' dichiarato: la domanda strutturale ha risposta, quella sulla
    /// congestione no.
    ResiduoNonDichiarato,
}

/// Il verdetto del criterio su UN candidato.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerdettoCapienza {
    /// La richiesta ci sta: dentro il limite totale e dentro il residuo (o il
    /// residuo si e' gia' rigenerato).
    Capiente,
    /// La richiesta supera il LIMITE del fornitore: non ci stara' mai, nemmeno
    /// a bucket pieno. E' un fatto STRUTTURALE della coppia, non una
    /// congestione. Porta i due numeri del confronto (regola M).
    OltreIlLimite { richiesta: i64, limite: i64 },
    /// Il residuo di adesso non basta: e' una congestione, non
    /// un'incompatibilita'. `reset_fra_s` e' `None` quando il fornitore non
    /// dichiara l'istante di reset (MISURATO su mistral).
    ResiduoInsufficiente {
        richiesta: i64,
        residuo: i64,
        reset_fra_s: Option<i64>,
    },
    /// Non si sa: NON esclude e NON retrocede.
    Ignota { motivo: MotivoIgnota },
}

/// L'esito dell'applicazione del criterio a un POOL di candidati.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EsitoCapienza {
    /// Il criterio ha lavorato lasciando almeno un candidato in testa:
    /// `esclusi` fuori dal pool (oltre il limite), `retrocessi` in coda
    /// (residuo insufficiente). Entrambi possono essere 0.
    Applicato { esclusi: usize, retrocessi: usize },
    /// TUTTI i candidati sono oltre il limite: si serve il pool INTERO,
    /// dichiarandolo ([`SEGNALE_RICADUTA`]).
    ///
    /// Il design taceva su questo caso e la scelta merita il suo perche': un
    /// criterio che lascia la selezione SENZA modelli trasforma una
    /// preferenza informata in un guasto — la stessa ragione per cui il
    /// gemello della latenza non e' mai fail-closed. E qui i due esiti non si
    /// equivalgono: il 429 e' un fallimento VELOCE, gia' gestito a valle
    /// (failover cross-provider + portata del cooldown), mentre «nessun
    /// modello» ferma il run. La ricaduta si DICHIARA nel rationale invece di
    /// lasciarla dedurre dal comportamento.
    RicadutaPoolPieno { oltre_limite: usize },
}

/// Il suffisso strutturato che l'esclusione appende al `rationale` della
/// scelta. UN solo letterale (regola N): chi lo legge nei log o nei test lo
/// importa da qui, mai una seconda stringa.
pub const SEGNALE_OLTRE_LIMITE: &str = "tpm=oltre_limite";
/// Il suffisso della retrocessione.
pub const SEGNALE_RESIDUO_SCARSO: &str = "tpm=residuo_scarso";
/// Il suffisso della ricaduta a pool svuotato.
pub const SEGNALE_RICADUTA: &str = "tpm=oltre_limite_ricaduta";

impl EsitoCapienza {
    /// I segnali da appendere al rationale, nell'ordine. Il testo si compone
    /// DAI campi (regola Q), mai il contrario: un esito che non ha ne'
    /// escluso ne' retrocesso non dice nulla.
    ///
    /// I segnali descrivono la SELEZIONE, non il candidato scelto: dicono che
    /// in questo giro qualcuno e' stato scartato o retrocesso per capienza —
    /// che e' cio' che serve a chi rilegge una scelta e si chiede perche' non
    /// sia uscito il fornitore che si aspettava.
    pub fn segnali(&self) -> Vec<&'static str> {
        match self {
            EsitoCapienza::RicadutaPoolPieno { .. } => vec![SEGNALE_RICADUTA],
            EsitoCapienza::Applicato {
                esclusi,
                retrocessi,
            } => {
                let mut v = Vec::new();
                if *esclusi > 0 {
                    v.push(SEGNALE_OLTRE_LIMITE);
                }
                if *retrocessi > 0 {
                    v.push(SEGNALE_RESIDUO_SCARSO);
                }
                v
            }
        }
    }
}

/// L'esito sul pool: QUALI indici servire e in quale ORDINE, piu' che cosa e'
/// successo. Gli indici e non le righe: il criterio non conosce — e non deve
/// conoscere — la forma delle righe del chiamante.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrdineCapienza {
    /// Indici del pool d'ingresso da servire: prima gli ammessi nell'ordine
    /// d'ingresso, poi i retrocessi nell'ordine d'ingresso.
    pub keep: Vec<usize>,
    pub esito: EsitoCapienza,
}

/// Il verdetto su UN candidato, nell'ordine dichiarato dal design.
///
/// `freschezza_max_s` e' la soglia oltre cui un'osservazione non descrive piu'
/// il minuto corrente: sopra quella eta' il verdetto e' `Ignota`, non un
/// `Capiente` di comodo ne' un'esclusione al buio.
///
/// `adesso` e' un PARAMETRO e non `Utc::now()`: il criterio resta puro e
/// golden-abile, e l'istante lo dichiara chi fa l'I/O.
pub fn capienza(
    obs: Option<&OsservazioneTpm>,
    richiesta_token: i64,
    freschezza_max_s: i64,
    adesso: DateTime<Utc>,
) -> VerdettoCapienza {
    let Some(o) = obs else {
        return VerdettoCapienza::Ignota {
            motivo: MotivoIgnota::MaiOsservata,
        };
    };
    // 2. Freschezza. L'eta' negativa (osservazione col timestamp nel futuro,
    // orologi disallineati) NON e' una scadenza: si tratta come fresca.
    let eta_s = (adesso - o.observed_at).num_seconds();
    if eta_s > freschezza_max_s {
        return VerdettoCapienza::Ignota {
            motivo: MotivoIgnota::OsservazioneScaduta { eta_s },
        };
    }
    // 3. Il tetto. Un limite non positivo non e' un limite: non si esclude
    // tutto il parco per una riga di telemetria malformata.
    let Some(limite) = o.tokens_limit.filter(|l| *l > 0) else {
        return VerdettoCapienza::Ignota {
            motivo: MotivoIgnota::LimiteNonDichiarato,
        };
    };
    // 4. Il caso STRUTTURALE: vale sempre, indipendentemente dal residuo. E'
    // il caso groq/180K del 17/08.
    if richiesta_token > limite {
        return VerdettoCapienza::OltreIlLimite {
            richiesta: richiesta_token,
            limite,
        };
    }
    // 5. Reset gia' passato: il bucket si e' rigenerato, e il confronto col
    // limite pieno e' gia' avvenuto al punto 4.
    if let Some(reset) = o.tokens_reset_at {
        if reset <= adesso {
            return VerdettoCapienza::Capiente;
        }
    }
    // 6. Il residuo. Assente = la domanda sulla congestione non ha risposta.
    let Some(residuo) = o.tokens_remaining else {
        return VerdettoCapienza::Ignota {
            motivo: MotivoIgnota::ResiduoNonDichiarato,
        };
    };
    if richiesta_token > residuo {
        return VerdettoCapienza::ResiduoInsufficiente {
            richiesta: richiesta_token,
            residuo,
            reset_fra_s: o
                .tokens_reset_at
                .map(|r| (r - adesso).num_seconds().max(0)),
        };
    }
    VerdettoCapienza::Capiente
}

/// Applica il criterio a un POOL: esclude gli `OltreIlLimite`, RETROCEDE in
/// coda i `ResiduoInsufficiente`, lascia dov'e' tutto il resto (regola Q), e
/// se l'esclusione svuota il pool RICADE sul pool intero dichiarandolo.
///
/// `osservazioni` e' parallelo al pool (una voce per candidato, `None` = mai
/// osservato).
///
/// La retrocessione e' una partizione STABILE: l'ordine relativo dentro i due
/// gruppi resta quello d'ingresso, cosi' il criterio non riscrive la
/// preferenza di chi l'ha prodotta (costo, telemetria) — la sposta soltanto in
/// coda. Se tutti sono retrocessi il pool resta intero nel suo ordine: «se e'
/// l'unico rimasto e' meglio provarlo che non avere nessuno».
pub fn ordina_per_capienza(
    osservazioni: &[Option<OsservazioneTpm>],
    richiesta_token: i64,
    freschezza_max_s: i64,
    adesso: DateTime<Utc>,
) -> OrdineCapienza {
    let mut ammessi: Vec<usize> = Vec::with_capacity(osservazioni.len());
    let mut retrocessi: Vec<usize> = Vec::new();
    let mut oltre = 0usize;
    for (i, obs) in osservazioni.iter().enumerate() {
        match capienza(obs.as_ref(), richiesta_token, freschezza_max_s, adesso) {
            VerdettoCapienza::OltreIlLimite { .. } => oltre += 1,
            VerdettoCapienza::ResiduoInsufficiente { .. } => retrocessi.push(i),
            VerdettoCapienza::Capiente | VerdettoCapienza::Ignota { .. } => ammessi.push(i),
        }
    }
    if ammessi.is_empty() && retrocessi.is_empty() && !osservazioni.is_empty() {
        return OrdineCapienza {
            keep: (0..osservazioni.len()).collect(),
            esito: EsitoCapienza::RicadutaPoolPieno {
                oltre_limite: oltre,
            },
        };
    }
    let n_retrocessi = retrocessi.len();
    ammessi.extend(retrocessi);
    OrdineCapienza {
        keep: ammessi,
        esito: EsitoCapienza::Applicato {
            esclusi: oltre,
            retrocessi: n_retrocessi,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("istante valido")
            .with_timezone(&Utc)
    }

    /// L'osservazione VERA del 17/08/2026, coi numeri della riga scritta dal
    /// sensore un minuto prima del 429.
    fn osservazione_groq_17_08() -> OsservazioneTpm {
        OsservazioneTpm {
            tokens_limit: Some(8_000),
            tokens_remaining: Some(120),
            tokens_reset_at: Some(t("2026-08-17T14:59:18Z")),
            observed_at: t("2026-08-17T14:58:19Z"),
        }
    }

    /// Test 2 del design: IL CASO MISURATO, coi numeri veri.
    ///
    /// Il run portava ~180.000 token e groq dichiarava 8000 TPM: e'
    /// `OltreIlLimite` PERCHE' supera il limite pieno, non perche' il residuo
    /// fosse 120 — la distinzione e' l'intero punto del criterio, e i due casi
    /// hanno rimedi opposti (uno passa da solo al reset, l'altro mai).
    ///
    /// MUTAZIONE: se il punto 4 (`richiesta > limite`) venisse dopo il
    /// controllo del reset, o se confrontasse il residuo invece del limite, il
    /// verdetto diventa `ResiduoInsufficiente` e la prima asserzione
    /// rosseggia col fornitore che avrebbe preso 429.
    #[test]
    fn il_caso_misurato_del_17_08() {
        let obs = osservazione_groq_17_08();
        let adesso = t("2026-08-17T14:58:30Z");
        assert_eq!(
            capienza(Some(&obs), 180_000, 120, adesso),
            VerdettoCapienza::OltreIlLimite {
                richiesta: 180_000,
                limite: 8_000
            },
            "180K contro 8000 TPM: strutturale, nessun reset lo cambia"
        );
        // Stessa riga, richiesta piccola: qui e' congestione, e il tempo al
        // reset e' calcolato (14:59:18 - 14:58:30 = 48s).
        assert_eq!(
            capienza(Some(&obs), 5_000, 120, adesso),
            VerdettoCapienza::ResiduoInsufficiente {
                richiesta: 5_000,
                residuo: 120,
                reset_fra_s: Some(48)
            }
        );
    }

    /// I tre esiti dell'ignoto che il design elenca, piu' il quarto che i dati
    /// reali hanno imposto. NESSUNO esclude (regola Q).
    ///
    /// MUTAZIONE: se il criterio trattasse l'assenza di osservazione (o un
    /// limite non dichiarato) come esclusione, la prima o la terza asserzione
    /// rosseggia.
    #[test]
    fn i_quattro_modi_di_non_sapere() {
        let adesso = t("2026-08-17T15:00:00Z");
        assert_eq!(
            capienza(None, 180_000, 120, adesso),
            VerdettoCapienza::Ignota {
                motivo: MotivoIgnota::MaiOsservata
            }
        );
        // Osservata mezz'ora fa: non descrive il minuto corrente.
        let vecchia = OsservazioneTpm {
            observed_at: t("2026-08-17T14:30:00Z"),
            ..osservazione_groq_17_08()
        };
        assert_eq!(
            capienza(Some(&vecchia), 180_000, 120, adesso),
            VerdettoCapienza::Ignota {
                motivo: MotivoIgnota::OsservazioneScaduta { eta_s: 1_800 }
            },
            "un residuo di mezz'ora fa non e' una misura di adesso"
        );
        // Perplexity: risponde, ma non manda gli header (MISURATO).
        let senza_limite = OsservazioneTpm {
            tokens_limit: None,
            tokens_remaining: None,
            tokens_reset_at: None,
            observed_at: t("2026-08-17T14:59:50Z"),
        };
        assert_eq!(
            capienza(Some(&senza_limite), 180_000, 120, adesso),
            VerdettoCapienza::Ignota {
                motivo: MotivoIgnota::LimiteNonDichiarato
            },
            "chi non dichiara il tetto non va penalizzato per questo"
        );
        // Limite dichiarato, residuo no: la domanda strutturale ha risposta
        // (ci sta), quella sulla congestione no.
        let senza_residuo = OsservazioneTpm {
            tokens_limit: Some(2_000_000),
            tokens_remaining: None,
            tokens_reset_at: None,
            observed_at: t("2026-08-17T14:59:50Z"),
        };
        assert_eq!(
            capienza(Some(&senza_residuo), 180_000, 120, adesso),
            VerdettoCapienza::Ignota {
                motivo: MotivoIgnota::ResiduoNonDichiarato
            }
        );
    }

    /// Il reset GIA' PASSATO rigenera il bucket: col residuo di ieri si
    /// escluderebbe un fornitore perfettamente disponibile.
    ///
    /// MUTAZIONE: togliere il punto 5 (o invertire il confronto in
    /// `reset > adesso`) fa uscire `ResiduoInsufficiente` e il test rosseggia.
    #[test]
    fn il_reset_passato_rigenera_il_bucket() {
        let obs = osservazione_groq_17_08();
        // 14:59:30 e' DOPO il reset (14:59:18) e l'osservazione (14:58:19) e'
        // ancora fresca (71s < 120).
        assert_eq!(
            capienza(Some(&obs), 5_000, 120, t("2026-08-17T14:59:30Z")),
            VerdettoCapienza::Capiente,
            "il bucket si e' rigenerato: 5000 sta negli 8000 pieni"
        );
        // Ma il caso strutturale resta strutturale anche dopo il reset.
        assert_eq!(
            capienza(Some(&obs), 180_000, 120, t("2026-08-17T14:59:30Z")),
            VerdettoCapienza::OltreIlLimite {
                richiesta: 180_000,
                limite: 8_000
            }
        );
    }

    /// Mistral: limite e residuo dichiarati, istante di reset MAI (MISURATO
    /// sul DB vivo il 17/08). La retrocessione vale lo stesso; l'attesa non si
    /// inventa.
    ///
    /// MUTAZIONE: un `reset_fra_s` scalare costringerebbe a scrivere uno 0 (o
    /// un default) e la seconda asserzione rosseggia.
    #[test]
    fn senza_istante_di_reset_si_retrocede_senza_promettere_un_attesa() {
        let obs = OsservazioneTpm {
            tokens_limit: Some(2_000_000),
            tokens_remaining: Some(1_000),
            tokens_reset_at: None,
            observed_at: t("2026-08-17T14:59:50Z"),
        };
        let v = capienza(Some(&obs), 180_000, 120, t("2026-08-17T15:00:00Z"));
        assert_eq!(
            v,
            VerdettoCapienza::ResiduoInsufficiente {
                richiesta: 180_000,
                residuo: 1_000,
                reset_fra_s: None
            }
        );
    }

    /// La capienza piena: la richiesta sta nel limite E nel residuo.
    #[test]
    fn dentro_limite_e_residuo_e_capiente() {
        let obs = OsservazioneTpm {
            tokens_limit: Some(2_000_000),
            tokens_remaining: Some(1_996_407),
            tokens_reset_at: None,
            observed_at: t("2026-08-17T14:59:50Z"),
        };
        assert_eq!(
            capienza(Some(&obs), 180_000, 120, t("2026-08-17T15:00:00Z")),
            VerdettoCapienza::Capiente
        );
    }

    /// Il pool: l'esclusione toglie, la retrocessione sposta in coda, l'ignoto
    /// resta dov'e'.
    ///
    /// MUTAZIONE: se la retrocessione ESCLUDESSE invece di spostare, l'indice
    /// 1 sparisce e la prima asserzione rosseggia; se l'ignoto venisse escluso
    /// sparisce l'indice 2.
    #[test]
    fn il_pool_esclude_retrocede_e_lascia_stare_l_ignoto() {
        let adesso = t("2026-08-17T14:58:30Z");
        let oss = vec![
            // 0: oltre il limite -> fuori.
            Some(osservazione_groq_17_08()),
            // 1: residuo scarso -> in coda.
            Some(OsservazioneTpm {
                tokens_limit: Some(2_000_000),
                tokens_remaining: Some(1_000),
                tokens_reset_at: None,
                observed_at: t("2026-08-17T14:58:20Z"),
            }),
            // 2: mai osservato -> resta dov'e'.
            None,
            // 3: capiente -> resta dov'e'.
            Some(OsservazioneTpm {
                tokens_limit: Some(2_000_000),
                tokens_remaining: Some(1_996_407),
                tokens_reset_at: None,
                observed_at: t("2026-08-17T14:58:20Z"),
            }),
        ];
        let o = ordina_per_capienza(&oss, 180_000, 120, adesso);
        assert_eq!(
            o.keep,
            vec![2, 3, 1],
            "ammessi in ordine, poi i retrocessi; l'escluso non c'e'"
        );
        assert_eq!(
            o.esito,
            EsitoCapienza::Applicato {
                esclusi: 1,
                retrocessi: 1
            }
        );
        assert_eq!(
            o.esito.segnali(),
            vec![SEGNALE_OLTRE_LIMITE, SEGNALE_RESIDUO_SCARSO]
        );
    }

    /// Tutti oltre il limite -> pool INTERO servito, dichiarandolo. Mai
    /// fail-closed: «nessun modello» ferma il run, un 429 no.
    ///
    /// MUTAZIONE: fail-closed (keep vuoto) -> rosseggia `keep`; segnale
    /// assente -> rosseggia l'ultima asserzione.
    #[test]
    fn pool_tutto_oltre_il_limite_ricade_dichiarando() {
        let adesso = t("2026-08-17T14:58:30Z");
        let oss = vec![
            Some(osservazione_groq_17_08()),
            Some(OsservazioneTpm {
                tokens_limit: Some(6_000),
                tokens_remaining: Some(5_963),
                tokens_reset_at: None,
                observed_at: t("2026-08-17T14:58:20Z"),
            }),
        ];
        let o = ordina_per_capienza(&oss, 180_000, 120, adesso);
        assert_eq!(o.keep, vec![0, 1], "la ricaduta serve il pool INTERO");
        assert_eq!(
            o.esito,
            EsitoCapienza::RicadutaPoolPieno { oltre_limite: 2 }
        );
        assert_eq!(o.esito.segnali(), vec![SEGNALE_RICADUTA]);
    }

    /// Tutti retrocessi: NON e' una ricaduta — il pool resta intero nel suo
    /// ordine e il segnale e' quello della retrocessione, perche' fra qualche
    /// decina di secondi quei candidati tornano validi.
    #[test]
    fn tutti_retrocessi_non_e_una_ricaduta() {
        let adesso = t("2026-08-17T14:58:30Z");
        let scarso = Some(OsservazioneTpm {
            tokens_limit: Some(2_000_000),
            tokens_remaining: Some(1_000),
            tokens_reset_at: None,
            observed_at: t("2026-08-17T14:58:20Z"),
        });
        let o = ordina_per_capienza(&[scarso, scarso], 180_000, 120, adesso);
        assert_eq!(o.keep, vec![0, 1]);
        assert_eq!(
            o.esito,
            EsitoCapienza::Applicato {
                esclusi: 0,
                retrocessi: 2
            }
        );
        assert_eq!(o.esito.segnali(), vec![SEGNALE_RESIDUO_SCARSO]);
    }

    /// Pool senza nulla da ridire: nessun segnale (il criterio silenzioso e'
    /// il criterio che non ha trovato niente, non uno che non ha guardato).
    #[test]
    fn pool_capiente_non_segnala_nulla() {
        let o = ordina_per_capienza(&[None, None], 180_000, 120, t("2026-08-17T15:00:00Z"));
        assert_eq!(o.keep, vec![0, 1]);
        assert_eq!(
            o.esito,
            EsitoCapienza::Applicato {
                esclusi: 0,
                retrocessi: 0
            }
        );
        assert!(o.esito.segnali().is_empty());
    }

    /// Pool vuoto in ingresso: nessuna ricaduta da inventare.
    #[test]
    fn pool_vuoto_resta_vuoto() {
        let o = ordina_per_capienza(&[], 180_000, 120, t("2026-08-17T15:00:00Z"));
        assert!(o.keep.is_empty());
        assert_eq!(
            o.esito,
            EsitoCapienza::Applicato {
                esclusi: 0,
                retrocessi: 0
            }
        );
    }
}
