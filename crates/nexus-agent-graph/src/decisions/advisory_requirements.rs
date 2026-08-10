//! PUNTO UNICO (regola L) della domanda: **quali requisiti ha emesso l'apparato
//! advisory di QUESTO run — da TUTTI gli apparati — e come si consegnano a chi
//! li deve usare come metro?**
//!
//! ## Il difetto che ha reso necessario il modulo (10/08/2026)
//!
//! Il sistema convoca DUE apparati advisory prima del lavoro — il Consiglio
//! delle Competenze (figure di dominio) e il panel multi-provider — e a valle
//! esistevano DUE risposte parziali alla stessa domanda, piu' un consumatore
//! che non se la poneva affatto:
//!
//! 1. [`super::requirement_conformance`] riscontra i requisiti sul contenuto dei
//!    file, ma legge la chiave `pre_run_advisory_synthesis`, che porta la
//!    sintesi di UN SOLO apparato (vedi sotto);
//! 2. il ciclo di review — che gira, e che a differenza del riscontro ha una
//!    CONSEGUENZA reale (`needs_changes` -> rimando in correzione) — riceveva un
//!    mandato senza alcun requisito: «rivedi le modifiche, verifica correttezza,
//!    sicurezza, edge case e regressioni». Il metro non arrivava a chi giudica.
//!
//! MISURATO il 10/08/2026 sul progetto `batteria-todo-deepseek`: 8 pareri
//! advisory (6 figure + 2 provider), 89 requisiti unici sul parco progetti,
//! 6 sub-run di review convocati — e zero requisiti nel loro mandato. Il caso da
//! cui e' emerso: la figura UI aveva emesso il requisito giusto («scala
//! tipografica di al massimo 5 livelli», «contrasto >= 4.5:1», «responsive a
//! 320px») e il prodotto era `font-family: Arial` con intestazione «Benvenuto
//! nella mia pagina» su una todo app. Il parere era corretto ed era PRE-RUN:
//! nessuno lo ha riscontrato dopo.
//!
//! ## Il riscontro non era «poco efficace»: non girava affatto
//!
//! La chiave `pre_run_advisory_synthesis` la scriveva UN SOLO ramo — quello in
//! cui i panel deliberano PRIMA che il run parta. Ma `advisory_overlap_enabled`
//! (mig 0606) e' `true` in produzione: li' il run parte subito, i panel
//! deliberano in parallelo e la sintesi arriva dalla barriera di scrittura,
//! che porta l'`enforcement` e non la sintesi. La chiave non veniva quindi mai
//! scritta, e `map_outcome` leggeva un'assenza.
//!
//! MISURATO sul parco progetti (10/08/2026): **200 run con resoconto, ZERO che
//! portino la nota del riscontro** — nemmeno quella che il modulo scrive
//! apposta quando tutto risulta applicato, perche' «il silenzio non distingue
//! verificato-conforme da nessuno-ha-guardato». Il riscontro deterministico e i
//! suoi venti test erano corretti e irraggiungibili — una misura costruita che
//! non e' mai entrata in esercizio. Da qui la scelta di UNA chiave con DUE scrittori
//! ([`ADVISORY_REQUIREMENTS_KEY`]): un dato che esiste solo in una delle
//! configurazioni possibili e' un dato che, nella configurazione reale, non
//! esiste.
//!
//! ## Perche' il metro va ai revisori e non a un criterio meccanico
//!
//! Il riscontro deterministico esiste ed e' quello giusto per cio' che sa
//! misurare, ma **non puo' chiudere l'anello da solo**: [`derive_criterion`]
//! pretende un letterale fra backtick, e sui requisiti realmente emessi in
//! produzione quel letterale non c'e' quasi mai.
//!
//! MISURATO sul parco progetti (10/08/2026): **89 requisiti unici, UNO SOLO
//! porta un letterale fra backtick**. Gli altri 88 sono prosa — «il contrasto
//! tra testo e sfondo deve essere >= 4.5:1», «centralizzare TUTTI gli accessi a
//! localStorage in un'unica coppia di funzioni» — verificabilissimi da chi legge
//! il codice, e non esprimibili come «cerca questa stringa in quel file».
//!
//! Da qui la scelta: dare conseguenza al verdetto del riscontro meccanico
//! avrebbe dato conseguenza a un SILENZIO (`unverifiable` 88 volte su 89), e
//! declassare un run su quell'esito lo declasserebbe praticamente sempre. Il
//! giudizio su un requisito descrittivo lo puo' dare solo chi legge il codice —
//! e quel lettore esiste gia', e' il review panel, e ha gia' la conseguenza.
//! Qui non si aggiunge un giudice: si consegna il metro a quello che c'e'.
//!
//! ## Il crinale: questo NON introduce un giudizio estetico
//!
//! Il repo ha gia' preso posizione — «bello» non e' un criterio, e un giudice
//! senza metro moltiplica i rimandi a vuoto (vedi
//! [`nexus_agent_tools::ui_styling`]). Il metro qui NON e' il gusto del
//! revisore: sono i requisiti che una figura ha emesso, per iscritto, prima del
//! lavoro. La domanda che il revisore riceve non e' «ti piace?» ma «questo
//! requisito risulta applicato nel codice che hai davanti?», che e' la stessa
//! forma di domanda del riscontro meccanico su un oggetto che il grep non
//! raggiunge.
//!
//! ## L'UNIONE, non il vincitore
//!
//! `select_pre_run_advisory` sceglie UNA delle due sintesi — quella col verdetto
//! piu' restrittivo — ed e' corretto per la domanda a cui risponde: quale
//! verdetto governa l'ENFORCEMENT al tool_dispatch, dove il messaggio da
//! emettere e' uno solo. Ma i REQUISITI da riscontrare sono un'altra domanda, e
//! la risposta e' l'unione: un requisito emesso dal panel «perdente» e' stato
//! emesso lo stesso.
//!
//! MISURATO sullo stesso run: entrambi gli apparati avevano deliberato
//! `proceed_with_changes`, quindi rango PARI, quindi vinceva il Consiglio — e gli
//! **8 requisiti del panel multi-provider** (fra cui «sostituire l'uso di
//! innerHTML con metodi DOM sicuri», emesso con `direction: must_be_absent`)
//! non raggiungevano ne' il riscontro ne' alcun lettore. Non erano stati
//! giudicati meno importanti: erano stati scartati da una selezione che
//! rispondeva a un'altra domanda.
//!
//! ## Confine (regola L)
//!
//! Qui vive la REGOLA: quali requisiti entrano, in che ordine, come si
//! deduplicano, come si rendono in un mandato. Nessun I/O e nessuna chiamata al
//! modello. L'estrazione da UNA sintesi resta delegata a
//! [`super::requirement_conformance::requirements_from_synthesis`] (che a sua
//! volta delega al parser unico di [`super::advisory_panel`]): tre parser dello
//! stesso campo darebbero tre idee di cosa sia un requisito.

use super::advisory_panel::Requirement;
use serde_json::Value;

/// Chiave extra nello stato del grafo: i requisiti emessi dagli apparati
/// advisory di questo run.
///
/// E' UNA sola chiave con DUE scrittori — il ramo classico, che li ha gia'
/// all'avvio del run, e la release della barriera in overlap, dove i panel
/// deliberano mentre il run gira — perche' i lettori a valle non devono sapere
/// da quale ramo e' passato il run: con due chiavi, un lettore che ne conoscesse
/// una sola sarebbe cieco su meta' delle configurazioni, che e' precisamente il
/// difetto misurato (vedi doc di modulo).
pub const ADVISORY_REQUIREMENTS_KEY: &str = "advisory_requirements";

/// Chi ha emesso il requisito. Identificatori canonici (regola N): sono le
/// STESSE stringhe con cui il `source` dell'enforcement nomina i due apparati,
/// e vivono qui perche' un secondo vocabolario per gli stessi due oggetti
/// divergerebbe al primo apparato aggiunto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdvisorySource {
    /// Consiglio delle Competenze: figure di dominio (ui_ux_designer,
    /// software_architect, security_engineer, ...).
    Council,
    /// Panel multi-provider: lo stesso task analizzato da provider diversi.
    MultiProvider,
}

impl AdvisorySource {
    /// Identificatore canonico (regola N).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Council => "council_synthesis",
            Self::MultiProvider => "multi_provider_synthesis",
        }
    }

    /// Come si nomina l'apparato a un lettore umano (il revisore, il resoconto).
    /// Nasce DALL'identificatore: chi compone un mandato non riconia un nome.
    pub fn etichetta(self) -> &'static str {
        match self {
            Self::Council => "Consiglio delle Competenze",
            Self::MultiProvider => "analisi multi-provider",
        }
    }

    /// Riconosce l'identificatore canonico. `None` fuori vocabolario: mai un
    /// default indovinato — un apparato che non sappiamo nominare non deve
    /// essere attribuito al Consiglio per comodita'.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "council_synthesis" => Some(Self::Council),
            "multi_provider_synthesis" => Some(Self::MultiProvider),
            _ => None,
        }
    }
}

/// Un requisito con l'apparato che lo ha emesso. La provenienza NON e'
/// decorazione: un revisore che legge «lo ha chiesto il Consiglio delle
/// Competenze» sa che dietro c'e' una figura di dominio, e il resoconto puo'
/// dire quale apparato e' stato disatteso.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcedRequirement {
    /// Il requisito, con la direzione dichiarata alla fonte quando c'e'.
    pub requirement: Requirement,
    /// Chi lo ha emesso.
    pub source: AdvisorySource,
}

/// Tutti i requisiti emessi dagli apparati advisory di un run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmittedRequirements {
    /// Nell'ordine di emissione, deduplicati per testo.
    pub items: Vec<SourcedRequirement>,
}

/// Tetto di requisiti resi per esteso nel mandato dei revisori.
///
/// Non e' un campione silenzioso: [`EmittedRequirements::metro`] DICHIARA
/// quanti ne ha omessi (regola O — un taglio taciuto si legge come «era tutto
/// qui»). Misurato: 35 requisiti unici sul run reale, cioe' sotto questo tetto;
/// esiste per il run che ne emettesse molti di piu', dove un mandato di
/// quindicimila caratteri sposterebbe il costo senza aggiungere metro.
const MAX_REQUISITI_NEL_METRO: usize = 40;

impl EmittedRequirements {
    /// Compone dai panel dichiarati: ogni voce e' `(chi, la sua sintesi)`.
    ///
    /// **Dedup per testo**, conservando la PRIMA provenienza: due apparati
    /// possono chiedere la stessa cosa (misurato: le figure del Consiglio
    /// ripetono i propri requisiti a ogni meta-step, e un requisito di sicurezza
    /// sul rendering compare sia in `security_engineer` sia nel provider
    /// `mistral`), e consegnare due volte lo stesso vincolo a un revisore non lo
    /// rende piu' vero: lo fa sembrare due requisiti disattesi invece di uno.
    ///
    /// L'ordine e' quello di dichiarazione dei panel e, dentro un panel, quello
    /// di emissione: due run con gli stessi pareri producono lo stesso mandato.
    pub fn from_panels(panels: &[(AdvisorySource, Value)]) -> Self {
        let mut items: Vec<SourcedRequirement> = Vec::new();
        for (source, synthesis) in panels {
            for requirement in
                super::requirement_conformance::requirements_from_synthesis(synthesis)
            {
                if items.iter().any(|i| i.requirement.text == requirement.text) {
                    continue;
                }
                items.push(SourcedRequirement {
                    requirement,
                    source: *source,
                });
            }
        }
        Self { items }
    }

    /// Serializza per [`ADVISORY_REQUIREMENTS_KEY`] nell'`extra` dello stato.
    ///
    /// Campi espliciti, mai una stringa da rileggere (regola Q): il consumatore
    /// che compone il mandato dei revisori deve poter distinguere il testo dalla
    /// provenienza dalla direzione senza parsare nulla.
    pub fn to_value(&self) -> Value {
        Value::Array(
            self.items
                .iter()
                .map(|i| {
                    let mut o = serde_json::Map::new();
                    o.insert("text".to_string(), Value::String(i.requirement.text.clone()));
                    o.insert(
                        "source".to_string(),
                        Value::String(i.source.as_str().to_string()),
                    );
                    if let Some(d) = i.requirement.direction {
                        o.insert("direction".to_string(), Value::String(d.as_str().to_string()));
                    }
                    Value::Object(o)
                })
                .collect(),
        )
    }

    /// Rilegge cio' che [`Self::to_value`] ha scritto.
    ///
    /// Una voce senza testo, o con una provenienza fuori vocabolario, viene
    /// SCARTATA invece di essere attribuita a un apparato per comodita': un
    /// requisito la cui origine non sappiamo nominare non e' un requisito di cui
    /// possiamo dire «lo ha chiesto il Consiglio» a un revisore.
    pub fn from_value(v: &Value) -> Self {
        let items = v
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let text = item.get("text").and_then(Value::as_str)?.trim();
                        if text.is_empty() {
                            return None;
                        }
                        let source =
                            AdvisorySource::parse(item.get("source").and_then(Value::as_str)?)?;
                        Some(SourcedRequirement {
                            requirement: Requirement {
                                text: text.to_string(),
                                direction: item
                                    .get("direction")
                                    .and_then(Value::as_str)
                                    .and_then(super::requirement_conformance::Direction::parse),
                            },
                            source,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { items }
    }

    /// Quanti requisiti sono stati emessi in tutto.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Nessun requisito emesso.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// I soli requisiti, per il riscontro meccanico di
    /// [`super::requirement_conformance`], che giudica un requisito per volta e
    /// non ha bisogno della provenienza.
    pub fn requirements(&self) -> Vec<Requirement> {
        self.items.iter().map(|i| i.requirement.clone()).collect()
    }

    /// Il METRO da consegnare a chi rivede il codice. `None` quando non c'e'
    /// nulla da consegnare: un mandato con una sezione vuota insegna al revisore
    /// che quella sezione si puo' ignorare.
    ///
    /// E' l'unico punto in cui questi requisiti diventano testo (come
    /// [`super::requirement_conformance::ConformanceReport::nota`]): chi convoca
    /// il panel non ricompone un elenco per conto proprio, o due convocazioni
    /// consegnerebbero due metri diversi.
    ///
    /// La consegna e' una DOMANDA, non un ordine di bocciare: il revisore ha il
    /// proprio vocabolario di gravita' e il proprio verdetto, e un requisito
    /// disatteso e' un fatto che gli si mette davanti — non una conclusione che
    /// gli si detta.
    pub fn metro(&self) -> Option<String> {
        if self.items.is_empty() {
            return None;
        }
        let mut s = String::from(
            "\n\nREQUISITI EMESSI PRIMA DEL LAVORO (da apparati advisory consultati a monte). \
             Sono il METRO di questa revisione: per ciascuno, verifica NEL CODICE se risulta \
             applicato. Un requisito disatteso e' un rilievo con evidenza (file e punto), non \
             un'opinione; se un requisito non e' verificabile leggendo il codice, dillo invece \
             di darlo per buono.\n",
        );
        for item in self.items.iter().take(MAX_REQUISITI_NEL_METRO) {
            s.push_str(&format!(
                "\n- [{}] {}",
                item.source.etichetta(),
                item.requirement.text
            ));
        }
        if let Some(omessi) = self.len().checked_sub(MAX_REQUISITI_NEL_METRO).filter(|n| *n > 0) {
            s.push_str(&format!(
                "\n\n(altri {omessi} requisiti non elencati per brevita': i {MAX_REQUISITI_NEL_METRO} \
                 sopra sono i primi emessi, non una selezione per importanza)"
            ));
        }
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decisions::advisory_panel::{
        compose_advisory_synthesis, AdvisoryPolicy, AdvisoryRoster,
    };
    use crate::decisions::requirement_conformance::Direction;

    /// Una sintesi VERA composta dal produttore (regola O): un JSON scritto a
    /// mano proverebbe solo che questo modulo sa leggere cio' che il test sa
    /// scrivere, e il campo `requirements` potrebbe cambiare nome senza che
    /// nessun test se ne accorga.
    fn sintesi(requisiti: &[Value]) -> Value {
        let parere = serde_json::json!({
            "success": true,
            "advisory": {
                "verdict": "proceed_with_changes",
                "risks": [],
                "requirements": requisiti,
                "recommendations": ["Testare su tre browser"],
            }
        });
        compose_advisory_synthesis(
            &[parere],
            &AdvisoryPolicy::default(),
            AdvisoryRoster::Convened(1),
        )
        .expect("sintesi composta")
        .to_value()
    }

    /// I due requisiti REALI del run misurato: quello della figura UI (Consiglio)
    /// e quello del provider `mistral` (multi-provider), che con rango pari
    /// veniva scartato.
    const REQ_UI: &str =
        "La pagina deve avere una scala tipografica di al massimo 5 livelli e spaziature con \
         al massimo 5 valori distinti.";
    const REQ_MULTI: &str =
        "Sostituire l'uso di innerHTML con metodi DOM sicuri (es. createElement, textContent) \
         per inserire task utente.";

    /// IL DIFETTO (regola O): entrambi gli apparati deliberano
    /// `proceed_with_changes`, la selezione per l'enforcement ne sceglie uno —
    /// e i requisiti sono l'UNIONE, non quelli del vincitore.
    ///
    /// MUTAZIONE: comporre da un solo panel (togliere la seconda voce da
    /// `from_panels`) rende rosso questo test: il requisito del multi-provider
    /// sparisce dal metro, che e' esattamente cio' che accadeva in produzione.
    #[test]
    fn i_requisiti_sono_l_unione_dei_due_apparati() {
        let emessi = EmittedRequirements::from_panels(&[
            (AdvisorySource::Council, sintesi(&[Value::String(REQ_UI.into())])),
            (
                AdvisorySource::MultiProvider,
                sintesi(&[serde_json::json!({
                    "text": REQ_MULTI,
                    "direction": "must_be_absent",
                })]),
            ),
        ]);

        assert_eq!(emessi.len(), 2, "nessun apparato viene scartato");
        assert_eq!(emessi.items[0].source, AdvisorySource::Council);
        assert_eq!(emessi.items[1].source, AdvisorySource::MultiProvider);
        // La direzione dichiarata ALLA FONTE attraversa fin qui (regola M): il
        // riscontro meccanico a valle non deve re-indovinarla dai verbi.
        assert_eq!(
            emessi.items[1].requirement.direction,
            Some(Direction::DeveMancare)
        );

        let metro = emessi.metro().expect("un metro c'e'");
        assert!(metro.contains(REQ_UI), "{metro}");
        assert!(
            metro.contains(REQ_MULTI),
            "il requisito del panel 'perdente' e' nel metro: {metro}"
        );
        assert!(metro.contains("Consiglio delle Competenze"), "{metro}");
        assert!(metro.contains("analisi multi-provider"), "{metro}");
    }

    /// Lo stesso vincolo chiesto da due apparati e' UN requisito, non due: un
    /// duplicato nel mandato si legge come due rilievi distinti.
    #[test]
    fn lo_stesso_requisito_da_due_apparati_e_uno_solo() {
        let emessi = EmittedRequirements::from_panels(&[
            (AdvisorySource::Council, sintesi(&[Value::String(REQ_MULTI.into())])),
            (
                AdvisorySource::MultiProvider,
                sintesi(&[Value::String(REQ_MULTI.into())]),
            ),
        ]);
        assert_eq!(emessi.len(), 1);
        assert_eq!(
            emessi.items[0].source,
            AdvisorySource::Council,
            "vince la PRIMA provenienza, cosi' l'ordine e' deterministico"
        );
    }

    /// Nessun requisito -> nessun metro. Una sezione vuota nel mandato insegna
    /// al revisore che quella sezione si puo' ignorare.
    #[test]
    fn nessun_requisito_nessun_metro() {
        assert!(EmittedRequirements::default().metro().is_none());
        assert!(EmittedRequirements::from_panels(&[]).is_empty());
        // Un panel che ha deliberato SENZA porre vincoli non produce metro.
        let emessi = EmittedRequirements::from_panels(&[(AdvisorySource::Council, sintesi(&[]))]);
        assert!(emessi.metro().is_none());
    }

    /// Le RACCOMANDAZIONI non entrano nel metro: sono l'altra lista, e una
    /// raccomandazione non applicata non e' uno scostamento. Il confine e' del
    /// produttore e si rispetta al consumo (delega al punto unico che gia' legge
    /// il solo campo `requirements`).
    #[test]
    fn le_raccomandazioni_non_entrano_nel_metro() {
        let emessi =
            EmittedRequirements::from_panels(&[(AdvisorySource::Council, sintesi(&[Value::String(REQ_UI.into())]))]);
        let metro = emessi.metro().expect("un metro c'e'");
        assert!(!metro.contains("Testare su tre browser"), "{metro}");
    }

    /// Il taglio si DICHIARA (regola O): un mandato che tronca in silenzio si
    /// legge come «i requisiti erano questi».
    #[test]
    fn il_taglio_del_metro_e_dichiarato() {
        let molti: Vec<Value> = (0..MAX_REQUISITI_NEL_METRO + 3)
            .map(|i| Value::String(format!("Requisito numero {i} da applicare")))
            .collect();
        let emessi =
            EmittedRequirements::from_panels(&[(AdvisorySource::Council, sintesi(&molti))]);
        assert_eq!(emessi.len(), MAX_REQUISITI_NEL_METRO + 3);
        let metro = emessi.metro().expect("un metro c'e'");
        assert!(metro.contains("altri 3 requisiti non elencati"), "{metro}");
        // I conteggi restano completi: il taglio riguarda solo la resa.
        assert_eq!(emessi.requirements().len(), MAX_REQUISITI_NEL_METRO + 3);
    }

    /// I requisiti attraversano l'`extra` dello stato senza perdere nulla: il
    /// testo, la PROVENIENZA e la direzione dichiarata alla fonte.
    ///
    /// E' il viaggio reale (regola O): in overlap i requisiti nascono nel task
    /// dei panel, passano dalla barriera, vengono scritti nello stato e riletti
    /// dal review gate a run concluso. Se l'andata e ritorno perdesse la
    /// direzione, il riscontro meccanico a valle tornerebbe a indovinarla dai
    /// verbi — il bug del 30/07/2026 rientrato da un'altra porta.
    ///
    /// MUTAZIONE: omettere `direction` in `to_value` rende rosso questo test.
    #[test]
    fn i_requisiti_attraversano_lo_stato_senza_perdere_la_provenienza() {
        let emessi = EmittedRequirements::from_panels(&[
            (AdvisorySource::Council, sintesi(&[Value::String(REQ_UI.into())])),
            (
                AdvisorySource::MultiProvider,
                sintesi(&[serde_json::json!({
                    "text": REQ_MULTI,
                    "direction": "must_be_absent",
                })]),
            ),
        ]);
        let riletti = EmittedRequirements::from_value(&emessi.to_value());
        assert_eq!(riletti, emessi, "andata e ritorno senza perdite");
        assert_eq!(
            riletti.items[1].requirement.direction,
            Some(Direction::DeveMancare)
        );
        assert_eq!(riletti.items[1].source, AdvisorySource::MultiProvider);
        // E il metro ricomposto dai dati riletti e' lo stesso.
        assert_eq!(riletti.metro(), emessi.metro());
    }

    /// Una voce malformata non diventa un requisito attribuito a caso: si
    /// scarta. Un `source` fuori vocabolario significa «scritto da una versione
    /// che non parla questo contratto», e inventargli un'origine farebbe dire a
    /// un revisore «lo ha chiesto il Consiglio» di qualcosa che il Consiglio
    /// potrebbe non aver mai detto.
    #[test]
    fn una_voce_malformata_si_scarta_invece_di_attribuirla() {
        let v = serde_json::json!([
            {"text": REQ_UI, "source": "council_synthesis"},
            {"text": "senza provenienza"},
            {"text": "provenienza ignota", "source": "chissa"},
            {"text": "   ", "source": "council_synthesis"},
        ]);
        let riletti = EmittedRequirements::from_value(&v);
        assert_eq!(riletti.len(), 1);
        assert_eq!(riletti.items[0].requirement.text, REQ_UI);
        // Uno stato senza la chiave non e' un errore: e' un run senza panel.
        assert!(EmittedRequirements::from_value(&Value::Null).is_empty());
    }

    /// Vocabolario chiuso (regola N): fuori vocabolario si dichiara di non
    /// sapere, non si attribuisce al Consiglio per comodita'.
    #[test]
    fn la_provenienza_fuori_vocabolario_non_si_indovina() {
        assert_eq!(
            AdvisorySource::parse("council_synthesis"),
            Some(AdvisorySource::Council)
        );
        assert_eq!(
            AdvisorySource::parse("multi_provider_synthesis"),
            Some(AdvisorySource::MultiProvider)
        );
        assert_eq!(AdvisorySource::parse("consiglio"), None);
        assert_eq!(AdvisorySource::parse(""), None);
    }
}
