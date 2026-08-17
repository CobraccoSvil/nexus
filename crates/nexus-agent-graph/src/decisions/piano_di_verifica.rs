//! PUNTO UNICO (regola L) della domanda: **quali PROVE ESEGUIBILI questo run ha
//! dichiarato, e come si giudicano senza interpretare nulla?**
//!
//! ## Il difetto, misurato il 17/08/2026
//!
//! Il final gate ha sette criteri, ognuno con la sua domanda cablata: il server
//! risponde? la pagina mostra contenuto? la suite passa? lo stile dichiarato e'
//! applicato? il codice prodotto si carica (mig 0734)? Ogni volta che il sistema
//! ha sbagliato in un modo NUOVO, il rimedio e' stato aggiungere una voce.
//!
//! Su un progetto senza porte il gate ha dichiarato «passato» due volte su un
//! run che aveva prodotto un file di test non eseguibile. Non aveva niente da
//! chiedere. **Il catalogo e' incompleto PER COSTRUZIONE**: nessuna lista
//! conterra' mai «crea un libro via POST, rileggilo via GET, controlla che sia
//! nella tabella, cancellalo e verifica che sparisca». Quella prova la sa
//! scrivere solo chi conosce il task.
//!
//! E il sistema **la sa gia' scrivere**: per lo stesso task il Consiglio ha
//! emesso 17 requisiti, ma in PROSA, e il riscontro ha potuto dire soltanto
//! `applicati=0, non_applicati=2, non_verificabili=15`. Quindici requisiti
//! giusti e inerti, perche' «i test devono coprire i casi limite» non e' una
//! cosa che si possa eseguire — limite gia' dichiarato e MISURATO in
//! [`super::requirement_conformance`] e [`super::advisory_requirements`] (89
//! requisiti unici sul parco progetti, UNO SOLO con un letterale cercabile).
//!
//! Questo modulo e' l'altra meta': far emettere PROVE al posto delle frasi, ed
//! eseguirle.
//!
//! ## Il modello PROPONE, la macchina EMETTE IL VERDETTO
//!
//! Nessuna [`Attesa`] ammette un giudizio del modello: il codice d'uscita, un
//! testo presente, un testo assente. E' la stessa divisione che
//! `task_complete.endpoints` gia' regge — l'agente dichiara quali endpoint
//! provare e il gate LI PROVA — generalizzata al comando.
//!
//! Il corollario della regola Q vale per intero: una `Prova` e' una
//! DICHIARAZIONE, non un accertamento. Diventa stato tecnico solo dopo che
//! [`giudica_prova`] ha guardato l'osservazione.
//!
//! ## Il pavimento resta, e non e' qui
//!
//! Le tre domande universali — il codice prodotto si carica ([`super::codice_eseguibile`]),
//! il servizio con una porta allocata risponde ([`super::endpoint_probes`]), la
//! pagina non e' vuota ([`super::static_render`]) — sono criteri PROPRI del gate
//! e restano tali. Non sono `Prova` e non passano di qui, per tre ragioni gia'
//! misurate:
//!
//!  - **il silenzio non e' innocuo**: senza pavimento, un run che non dichiara
//!    prove passerebbe senza controlli, e il run che non dichiara nulla e'
//!    tipicamente quello in difficolta';
//!  - **chi ha sbagliato non conosce il proprio errore**: l'agente che ha scritto
//!    il test Jest in un progetto senza Jest non si sarebbe mai autoimposto
//!    «verifica che il test parta»;
//!  - **giudice != worker** e' gia' regola di casa (`veto_del_giudice`): se il
//!    piano lo scrive solo l'esecutore, puo' dichiarare le prove facili e
//!    omettere quella che lo inchioda. Da qui la precedenza fra le origini.
//!
//! Percio' [`OriginePiano`] NON ha una variante `Pavimento`: il pavimento non e'
//! una prova dichiarata, e dargli una variante qui creerebbe una seconda strada
//! per costruire criteri che hanno gia' la loro (regola L).
//!
//! ## Confine (regola L)
//!
//! Qui vive il CRITERIO, puro: quali prove entrano ([`PianoDiVerifica`]), quali
//! si possono eseguire ([`PoliticaEsecuzione`]), come si giudica una singola
//! osservazione ([`giudica_prova`]) e che verdetto ne esce ([`classifica_piano`]).
//! L'I/O — eseguire i comandi, raccogliere gli output — vive in
//! `mcp-core::agent_graph_adapter::criteria_runner`, che porta i FATTI e non li
//! giudica.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use super::advisory_requirements::AdvisorySource;
use super::step_gate::{classify_step, CriticalityRule, StepCriticality};

/// Il tipo di criterio nel vocabolario del runner (regola N).
pub const CRITERION_TYPE: &str = "piano_di_verifica";

/// Chiave extra nello stato del grafo: le prove che gli apparati advisory di
/// questo run hanno emesso.
///
/// UNA chiave con DUE scrittori — il ramo classico, che le ha all'avvio del run,
/// e la release della barriera in overlap — per la stessa ragione gia' misurata
/// su [`super::advisory_requirements::ADVISORY_REQUIREMENTS_KEY`]: un dato che
/// esiste solo in una delle configurazioni possibili e', nella configurazione
/// reale, un dato che non esiste (200 run con resoconto, zero note di riscontro).
pub const PIANO_VERIFICA_KEY: &str = "piano_di_verifica";

/// Campo con cui una figura, un panel o l'agente dichiarano le proprie prove.
/// Un nome solo per i tre produttori e per la sintesi che li aggrega.
pub const CAMPO_PROVE: &str = "prove";

/// Chiavi della spec del criterio, con un solo punto di scrittura (i test le
/// referenziano da qui, mai come letterali sparsi).
pub const CHIAVE_PROVE: &str = "prove";
pub const CHIAVE_POLITICA: &str = "politica";
pub const CHIAVE_MAX_PROVE: &str = "max_prove";

/// I campi di UNA prova sul wire. Un punto di scrittura solo: li scrive
/// [`Prova::to_value`], li rilegge [`Prova::from_value`], li dichiara lo schema
/// dei tool — e un refuso in uno dei tre e' una prova che attraversa lo stato e
/// arriva vuota.
const CAMPO_DESCRIZIONE: &str = "descrizione";
const CAMPO_COMANDO: &str = "comando";
const CAMPO_WORKING_DIR: &str = "working_dir";
const CAMPO_ATTESA: &str = "attesa";
const CAMPO_ORIGINE: &str = "origine";

// ─── Vocabolario ──────────────────────────────────────────────────────────────

/// Quando una prova e' passata: il criterio e' MECCANICO, mai un giudizio.
///
/// **`Http` non c'e', ed e' una scelta**: la prova HTTP ha gia' il suo punto
/// unico — `task_complete.endpoints` -> [`super::endpoint_probes`] -> criterio
/// `http`, con il proprio vocabolario di status attesi, il proprio client e la
/// propria attesa di readiness. Riprodurla qui sarebbe una SECONDA strada per la
/// stessa domanda, con due idee di «2xx accettabile» destinate a divergere al
/// primo ritocco (regola L). Chi deve provare un endpoint lo dichiara dove il
/// gate gia' lo chiama.
///
/// Le tre varianti sono ORTOGONALI e non si combinano in una prova sola: una
/// prova ha UNA attesa. Chi vuole due condizioni dichiara due prove, e il
/// referto dira' quale delle due e' caduta — che con una attesa composta si
/// perderebbe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attesa {
    /// Il comando deve uscire con questo codice.
    Uscita { codice: i32 },
    /// L'output del comando DEVE contenere questo testo.
    OutputContiene { testo: String },
    /// L'output del comando NON deve contenerlo (es. `FAILED`, `Traceback`).
    OutputNonContiene { testo: String },
}

impl Attesa {
    /// Identificatore canonico (regola N).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Uscita { .. } => "exit_code",
            Self::OutputContiene { .. } => "output_contains",
            Self::OutputNonContiene { .. } => "output_not_contains",
        }
    }

    /// La forma canonica su cui si deduplica: due prove con lo stesso comando e
    /// la stessa attesa sono UNA prova, anche se le descrizioni differiscono.
    pub fn chiave(&self) -> String {
        match self {
            Self::Uscita { codice } => format!("exit_code:{codice}"),
            Self::OutputContiene { testo } => format!("output_contains:{testo}"),
            Self::OutputNonContiene { testo } => format!("output_not_contains:{testo}"),
        }
    }

    /// Serializza per il wire (schema del tool, `extra` dello stato, spec).
    pub fn to_value(&self) -> Value {
        match self {
            Self::Uscita { codice } => json!({ "tipo": self.as_str(), "codice": codice }),
            Self::OutputContiene { testo } | Self::OutputNonContiene { testo } => {
                json!({ "tipo": self.as_str(), "testo": testo })
            }
        }
    }

    /// Rilegge un'attesa dichiarata. `None` fuori vocabolario o con il campo
    /// portante vuoto: un'attesa che non sappiamo giudicare non diventa un
    /// `exit_code 0` per comodita' — quella e' un'altra prova, e la
    /// dichiarerebbe superata chiunque esca 0 per caso.
    pub fn from_value(v: &Value) -> Option<Self> {
        let tipo = v.get("tipo").and_then(Value::as_str)?.trim();
        let testo = || {
            v.get("testo")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        match tipo {
            "exit_code" => Some(Self::Uscita {
                // Il codice ASSENTE e' lo zero DICHIARATO dallo schema, non un
                // ripiego nascosto: «il comando deve riuscire» e' il caso
                // normale e lo schema lo documenta come default.
                codice: v.get("codice").and_then(Value::as_i64).unwrap_or(0) as i32,
            }),
            "output_contains" => testo().map(|testo| Self::OutputContiene { testo }),
            "output_not_contains" => testo().map(|testo| Self::OutputNonContiene { testo }),
            _ => None,
        }
    }
}

/// Chi ha proposto la prova, in ordine di preferenza dichiarata dal design.
///
/// I due apparati advisory restano DISTINTI e non collassano in un «Consiglio»
/// unico: [`super::advisory_requirements`] ha MISURATO che fonderli perde meta'
/// di cio' che e' stato emesso (8 requisiti del panel multi-provider scartati da
/// una selezione che rispondeva a un'altra domanda).
///
/// Non c'e' una variante per il REVISORE: il ciclo di review e' un gate di
/// chiusura gemello del final gate, non lo precede, e le sue prove non
/// arriverebbero a nessun esecutore. Una variante senza produttore sarebbe
/// vocabolario inerte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginePiano {
    /// Consiglio delle Competenze: figure di dominio, prima del lavoro.
    Consiglio,
    /// Panel multi-provider: lo stesso task analizzato da provider diversi.
    MultiProvider,
    /// L'agente esecutore, nella propria dichiarazione di chiusura.
    Agente,
}

impl OriginePiano {
    /// Identificatore canonico (regola N).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Consiglio => "council",
            Self::MultiProvider => "multi_provider",
            Self::Agente => "agent",
        }
    }

    /// Come si nomina l'origine a un lettore umano. Nasce DALL'identificatore:
    /// chi compone un referto non riconia un nome.
    pub fn etichetta(self) -> &'static str {
        match self {
            Self::Consiglio => "Consiglio delle Competenze",
            Self::MultiProvider => "analisi multi-provider",
            Self::Agente => "agente esecutore",
        }
    }

    /// Riconosce l'identificatore canonico. `None` fuori vocabolario: una prova
    /// la cui origine non sappiamo nominare non si attribuisce al Consiglio per
    /// comodita' — sarebbe la stessa bugia che
    /// [`super::advisory_requirements`] evita sui requisiti.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "council" => Some(Self::Consiglio),
            "multi_provider" => Some(Self::MultiProvider),
            "agent" => Some(Self::Agente),
            _ => None,
        }
    }

    /// L'origine di un apparato advisory. Deriva dal vocabolario che quel
    /// modulo gia' possiede, invece di riconiarne uno parallelo.
    pub fn da_advisory(source: AdvisorySource) -> Self {
        match source {
            AdvisorySource::Council => Self::Consiglio,
            AdvisorySource::MultiProvider => Self::MultiProvider,
        }
    }
}

/// Una prova che il gate sa eseguire e giudicare senza interpretare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prova {
    /// Cosa si sta accertando, per l'umano che legge il referto.
    pub descrizione: String,
    /// Come si accerta: la riga di comando.
    pub comando: String,
    /// Directory di lavoro relativa alla radice del run. `None` = la radice.
    pub working_dir: Option<String>,
    /// Quando e' passata.
    pub attesa: Attesa,
    /// Chi l'ha proposta (per il referto e per il conflitto d'interesse).
    pub origine: OriginePiano,
}

impl Prova {
    /// La chiave su cui si deduplica: il COMANDO e l'ATTESA, mai la descrizione
    /// ne' l'origine. Due apparati che chiedono la stessa prova la chiedono una
    /// volta sola — consegnarla due volte non la rende piu' vera, e nel referto
    /// si leggerebbe come due prove cadute invece di una.
    pub fn chiave(&self) -> (String, String, String) {
        (
            self.comando.trim().to_string(),
            self.working_dir.clone().unwrap_or_default(),
            self.attesa.chiave(),
        )
    }

    /// Serializza per il wire. Campi espliciti, mai una stringa da rileggere
    /// (regola Q).
    pub fn to_value(&self) -> Value {
        let mut o = Map::new();
        o.insert(CAMPO_DESCRIZIONE.to_string(), json!(self.descrizione));
        o.insert(CAMPO_COMANDO.to_string(), json!(self.comando));
        if let Some(wd) = &self.working_dir {
            o.insert(CAMPO_WORKING_DIR.to_string(), json!(wd));
        }
        o.insert(CAMPO_ATTESA.to_string(), self.attesa.to_value());
        o.insert(CAMPO_ORIGINE.to_string(), json!(self.origine.as_str()));
        Value::Object(o)
    }

    /// Rilegge una prova dichiarata, imponendo l'origine quando il produttore
    /// non la dichiara (una figura scrive la prova, non sa di essere «il
    /// Consiglio»).
    ///
    /// `None` quando manca il comando o l'attesa non e' riconoscibile: una prova
    /// senza comando non e' eseguibile e una senza attesa non e' giudicabile, e
    /// inventare l'una o l'altra darebbe un verdetto che nessuno ha chiesto.
    pub fn from_value(v: &Value, origine: OriginePiano) -> Option<Self> {
        let comando = campo_non_vuoto(v, CAMPO_COMANDO)?;
        let attesa = Attesa::from_value(v.get(CAMPO_ATTESA)?)?;
        let descrizione =
            campo_non_vuoto(v, CAMPO_DESCRIZIONE).unwrap_or_else(|| comando.clone());
        Some(Self {
            descrizione,
            comando,
            working_dir: campo_non_vuoto(v, CAMPO_WORKING_DIR),
            attesa,
            // L'origine DICHIARATA nel valore vince solo quando c'e' ed e' nel
            // vocabolario: e' il caso della rilettura dallo stato, dove la
            // provenienza e' gia' stata attribuita. Una figura che la scrivesse
            // da se' non potrebbe attribuirsi un apparato che non conosce.
            origine: v
                .get(CAMPO_ORIGINE)
                .and_then(Value::as_str)
                .and_then(OriginePiano::parse)
                .unwrap_or(origine),
        })
    }
}

/// Il valore TRIMMATO di un campo stringa, `None` se assente o vuoto. Un campo
/// di soli spazi non e' un valore: un comando vuoto non e' eseguibile e una
/// descrizione vuota non descrive niente.
fn campo_non_vuoto(v: &Value, campo: &str) -> Option<String> {
    v.get(campo)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Il piano di verifica di un run: le prove, deduplicate e in ordine di
/// emissione.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PianoDiVerifica {
    pub prove: Vec<Prova>,
}

impl PianoDiVerifica {
    /// Le prove emesse dagli apparati advisory, nell'ordine di dichiarazione dei
    /// panel. Gemella di
    /// [`super::advisory_requirements::EmittedRequirements::from_panels`] e per
    /// la stessa ragione: le prove sono l'UNIONE, mai quelle del panel che vince
    /// la selezione dell'enforcement — che risponde a un'altra domanda.
    pub fn dai_pareri(panels: &[(AdvisorySource, Value)]) -> Self {
        let mut piano = Self::default();
        for (source, synthesis) in panels {
            piano.assorbi(prove_da_campo(
                synthesis,
                OriginePiano::da_advisory(*source),
            ));
        }
        piano
    }

    /// Le prove che l'AGENTE dichiara chiudendo (`task_complete.prove`).
    ///
    /// Vengono per ULTIME e non possono rimuovere nulla: `assorbi` scarta i
    /// duplicati conservando la PRIMA origine, quindi una prova gia' chiesta da
    /// un apparato resta attribuita a lui. E' l'incarnazione di «giudice !=
    /// worker»: l'esecutore puo' aggiungere prove, non sostituire quelle di chi
    /// non ha scritto il codice.
    pub fn da_dichiarazione(declared_outcome: Option<&Value>) -> Self {
        let mut piano = Self::default();
        if let Some(d) = declared_outcome {
            piano.assorbi(prove_da_campo(d, OriginePiano::Agente));
        }
        piano
    }

    /// L'UNIONE, nell'ordine dei pezzi ricevuti. E' l'unico punto in cui un
    /// piano nasce da piu' fonti: due composizioni darebbero due ordini, e con
    /// due ordini la dedup attribuirebbe la stessa prova a origini diverse a
    /// seconda di chi ha composto.
    pub fn unione(pezzi: &[Self]) -> Self {
        let mut piano = Self::default();
        for p in pezzi {
            piano.assorbi(p.prove.clone());
        }
        piano
    }

    /// Aggiunge le prove non ancora presenti, conservando la prima origine.
    fn assorbi(&mut self, prove: Vec<Prova>) {
        for p in prove {
            if self.prove.iter().any(|e| e.chiave() == p.chiave()) {
                continue;
            }
            self.prove.push(p);
        }
    }

    /// Quante prove sono state dichiarate in tutto.
    pub fn len(&self) -> usize {
        self.prove.len()
    }

    /// Nessuna prova dichiarata. NON significa «niente da verificare»: e' il
    /// caso in cui il criterio dichiara di non aver misurato nulla.
    pub fn is_empty(&self) -> bool {
        self.prove.is_empty()
    }

    /// Serializza per [`PIANO_VERIFICA_KEY`] e per la spec del criterio.
    pub fn to_value(&self) -> Value {
        Value::Array(self.prove.iter().map(Prova::to_value).collect())
    }

    /// Rilegge cio' che [`Self::to_value`] ha scritto. Una voce malformata si
    /// SCARTA invece di diventare una prova con un'attesa inventata.
    ///
    /// L'origine di ripiego e' [`OriginePiano::Agente`] — la meno autorevole —
    /// perche' una voce che non dichiara la propria provenienza non deve
    /// guadagnarne una piu' forte passando da una serializzazione.
    pub fn from_value(v: Option<&Value>) -> Self {
        let mut piano = Self::default();
        let Some(arr) = v.and_then(Value::as_array) else {
            return piano;
        };
        piano.assorbi(
            arr.iter()
                .filter_map(|item| Prova::from_value(item, OriginePiano::Agente))
                .collect(),
        );
        piano
    }
}

/// Legge il campo [`CAMPO_PROVE`] da un oggetto che lo dichiara (la sintesi di
/// un panel, la dichiarazione di chiusura dell'agente).
fn prove_da_campo(contenitore: &Value, origine: OriginePiano) -> Vec<Prova> {
    contenitore
        .get(CAMPO_PROVE)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|item| Prova::from_value(item, origine))
                .collect()
        })
        .unwrap_or_default()
}

// ─── Ammissibilita': il piano NON e' un canale privilegiato ───────────────────

/// L'esito dell'ammissione di una prova all'esecuzione.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ammissione {
    /// Si puo' eseguire: la classificazione la colloca sotto la soglia.
    Ammessa { livello: StepCriticality },
    /// NON si esegue, e il motivo e' dichiarato.
    Rifiutata { motivo: String },
}

/// La politica con cui si decide se una prova PUO' essere eseguita.
///
/// ## Perche' esiste, e perche' non convoca giudici
///
/// Una prova e' una riga di shell proposta da un MODELLO ed eseguita dalla
/// macchina senza che nessun umano la veda: e' la differenza rispetto ai comandi
/// che il gate gia' esegue (`verify_steps` dal profilo di progetto, i runtime del
/// vocabolario di [`super::codice_eseguibile`]), che vengono dalla configurazione.
/// Il piano di verifica non deve diventare la scorciatoia con cui un comando
/// arbitrario aggira i presidi del resto del sistema.
///
/// La classificazione delega INTERAMENTE al punto unico
/// [`super::step_gate::classify_step`], con lo stesso vocabolario DB del gate
/// duale: nessun secondo elenco di comandi pericolosi nasce qui (regola L).
///
/// **Il gate duale non e' convocabile da un criterio**: il `criteria_runner` non
/// ha la porta di validazione e non puo' chiedere un parere a due fornitori.
/// Restano tre strade, e due sono chiuse — eseguire tutto e' il canale
/// privilegiato che il design vieta, rifiutare tutto rende il criterio inerte
/// (`run_command` e' `Unconfined` per CONTRATTO, quindi il suo pavimento e'
/// `Critical` e una soglia a `Mutating` non ammetterebbe una sola prova). La
/// terza e' questa: si esegue fino alla soglia dichiarata, e cio' che sta sopra
/// e' `NonEseguibile` col motivo.
///
/// LIMITE DICHIARATO: con la soglia di default (`critical`) una prova classificata
/// `Critical` — una migrazione di schema travestita da prova — viene eseguita.
/// Restringere la soglia a `observation` la chiude, al prezzo di ammettere le
/// sole righe fatte di comandi del vocabolario di osservazione: e' una decisione
/// dell'amministratore, e sta nel DB perche' possa prenderla senza un deploy.
#[derive(Debug, Clone, PartialEq)]
pub struct PoliticaEsecuzione {
    /// Il livello PIU' ALTO ancora ammesso. Oltre: `Rifiutata`.
    pub soglia: StepCriticality,
    /// Vocabolario dei tool mutatori (`agent.tools.result_cache_mutators`).
    pub mutatori: Vec<String>,
    /// Regole lessicali di criticita' (`orchestrator.critical_step_rules`).
    pub regole: Vec<CriticalityRule>,
    /// Artefatti rigenerabili (`orchestrator.rebuildable_artifacts`).
    pub rigenerabili: Vec<String>,
    /// Comandi di osservazione (`orchestrator.step_reach.observation_commands`).
    pub osservazione: Vec<String>,
}

impl PoliticaEsecuzione {
    /// Classifica la prova come se fosse il passo `run_command` che e', e
    /// decide.
    ///
    /// Il tool dichiarato e' `run_command` perche' e' cio' che la prova FA: la
    /// portata la dichiara il contratto del tool, mai il testo del comando
    /// ([`super::step_reach`]).
    pub fn ammissione(&self, prova: &Prova) -> Ammissione {
        let mut input = Map::new();
        input.insert("command".to_string(), json!(prova.comando));
        if let Some(wd) = &prova.working_dir {
            input.insert("working_dir".to_string(), json!(wd));
        }
        let classificazione = classify_step(
            TOOL_DELLA_PROVA,
            &Value::Object(input),
            &self.mutatori,
            &self.regole,
            &self.rigenerabili,
            &self.osservazione,
        );
        if classificazione.level <= self.soglia {
            return Ammissione::Ammessa {
                livello: classificazione.level,
            };
        }
        let categoria = classificazione
            .matched_category
            .unwrap_or_else(|| classificazione.reach.as_str().to_string());
        Ammissione::Rifiutata {
            motivo: format!(
                "prova non eseguita: classificata '{}' ({categoria}), oltre la soglia '{}' \
                 ammessa per le prove del piano",
                classificazione.level.as_str(),
                self.soglia.as_str()
            ),
        }
    }

    /// Serializza per la spec: la misura resta leggibile in cio' che ha
    /// dichiarato di aver usato per misurare (stessa disciplina del vocabolario
    /// di [`super::codice_eseguibile`]).
    pub fn to_value(&self) -> Value {
        json!({
            "soglia": self.soglia,
            "mutatori": self.mutatori,
            "regole": self.regole,
            "rigenerabili": self.rigenerabili,
            "osservazione": self.osservazione,
        })
    }

    /// Rilegge la politica dalla spec. `None` = politica assente o illeggibile:
    /// chi verifica lo DICHIARA e non esegue nulla, perche' senza politica non
    /// si sa cosa sia ammesso e «eseguo tutto» e' esattamente il canale
    /// privilegiato che questa struttura esiste per negare.
    pub fn from_value(v: Option<&Value>) -> Option<Self> {
        let v = v?;
        Some(Self {
            soglia: serde_json::from_value(v.get("soglia")?.clone()).ok()?,
            mutatori: lista_di_stringhe(v.get("mutatori")),
            regole: regole_da_valore(v.get("regole")),
            rigenerabili: lista_di_stringhe(v.get("rigenerabili")),
            osservazione: lista_di_stringhe(v.get("osservazione")),
        })
    }
}

/// Le regole di criticita' rilette dalla spec. Una voce che non si deserializza
/// si SCARTA, come fa `step_gate::parse_rules` col vocabolario del DB: una
/// regola rotta non deve portarsi via le altre, che sono quelle che vietano.
fn regole_da_valore(v: Option<&Value>) -> Vec<CriticalityRule> {
    v.and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|r| serde_json::from_value(r.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Il tool con cui una prova viene classificata ED eseguita: sono lo stesso
/// nome, in un posto solo. Classificare come `run_command` ed eseguire con un
/// altro tool giudicherebbe una cosa e ne farebbe un'altra.
pub const TOOL_DELLA_PROVA: &str = "run_command";

fn lista_di_stringhe(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

// ─── Il giudizio su UNA prova ─────────────────────────────────────────────────

/// Cio' che si e' OSSERVATO eseguendo una prova. Campi, non prosa (regola Q):
/// il giudizio nasce da qui e non da una stringa da rileggere.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Osservazione {
    /// Codice d'uscita STRUTTURATO. `None` = il processo non ne ha prodotto uno.
    pub exit_code: Option<i32>,
    /// Output combinato del comando (stdout + stderr, come lo consegna il tool).
    pub output: String,
}

/// Come e' andata UNA prova.
///
/// `NonEseguibile` NON boccia ed e' distinta da `Fallita` (regola Q): «il
/// comando non e' partito» e «il comando ha risposto e la risposta e' quella
/// sbagliata» hanno rimedi opposti — la prima si rimedia sull'ambiente, la
/// seconda sul codice — e collassarle rimanderebbe l'agente a correggere un
/// difetto che non esiste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EsitoSingolo {
    /// Osservata e conforme all'attesa.
    Superata,
    /// Osservata e NON conforme: e' l'unico esito che prova un difetto.
    Fallita { osservato: String },
    /// Non si e' potuta eseguire, e il perche' e' dichiarato.
    NonEseguibile { motivo: String },
}

impl EsitoSingolo {
    /// Identificatore canonico (regola N).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Superata => "passed",
            Self::Fallita { .. } => "failed",
            Self::NonEseguibile { .. } => "not_runnable",
        }
    }
}

/// Una prova e cio' che se ne e' accertato.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EsitoProva {
    pub prova: Prova,
    pub esito: EsitoSingolo,
}

/// IL GIUDIZIO, in un posto solo: l'attesa contro l'osservazione.
///
/// Un exit code ASSENTE non e' un exit code SBAGLIATO: il processo non ha
/// prodotto uno stato d'uscita, quindi la prova non e' stata misurata. E' la
/// stessa distinzione che `check_run_command` ha gia' dovuto imparare (regola Q),
/// e vale solo per l'attesa che il codice d'uscita lo GUARDA — le due attese
/// sull'output giudicano l'output, e un comando ucciso che ha comunque scritto
/// il testo cercato lo ha scritto davvero.
pub fn giudica_prova(attesa: &Attesa, oss: &Osservazione) -> EsitoSingolo {
    match attesa {
        Attesa::Uscita { codice } => giudica_uscita(*codice, oss.exit_code),
        Attesa::OutputContiene { testo } => conforme_se(
            oss.output.contains(testo.as_str()),
            format!("l'output NON contiene '{testo}'"),
        ),
        Attesa::OutputNonContiene { testo } => conforme_se(
            !oss.output.contains(testo.as_str()),
            format!("l'output contiene '{testo}'"),
        ),
    }
}

/// Il codice d'uscita OSSERVATO contro quello atteso.
fn giudica_uscita(atteso: i32, osservato: Option<i32>) -> EsitoSingolo {
    match osservato {
        Some(visto) if visto == atteso => EsitoSingolo::Superata,
        Some(visto) => EsitoSingolo::Fallita {
            osservato: format!("exit code {visto}, atteso {atteso}"),
        },
        None => EsitoSingolo::NonEseguibile {
            motivo: "il comando non ha prodotto un codice d'uscita: l'esito non e' \
                     stato misurato"
                .to_string(),
        },
    }
}

/// Superata quando la condizione regge, altrimenti fallita con cio' che si e'
/// osservato. Le due attese sull'output differiscono SOLO nella condizione: due
/// rami copiati direbbero la stessa cosa in due posti.
fn conforme_se(conforme: bool, osservato: String) -> EsitoSingolo {
    if conforme {
        EsitoSingolo::Superata
    } else {
        EsitoSingolo::Fallita { osservato }
    }
}

// ─── Il verdetto sul PIANO ────────────────────────────────────────────────────

/// Il verdetto del criterio sul run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerdettoPiano {
    /// Almeno una prova eseguita, nessuna fallita.
    PianoSuperato {
        superate: usize,
        non_eseguibili: usize,
    },
    /// Almeno una prova FALLITA: il run non ha finito.
    ProvaFallita { fallite: Vec<EsitoProva> },
    /// Nessuna prova dichiarata.
    ///
    /// NON e' un via libera, ed e' la differenza rispetto al
    /// `NienteDaProvare` di [`super::codice_eseguibile`]: li' il criterio ha
    /// GUARDATO i file prodotti e ha constatato che nessuno era codice; qui
    /// nessuno ha dichiarato niente, che e' un'assenza di dichiarazione e non
    /// una misura. Contarla come misura positiva farebbe salire il conteggio
    /// dei criteri misurati del gate proprio nei run che non hanno dichiarato
    /// nulla — cioe' renderebbe «verificato» il silenzio.
    PianoVuoto,
    /// C'erano prove e nessuna si e' potuta eseguire.
    NonEseguito {
        motivo: String,
        non_eseguibili: usize,
    },
}

impl VerdettoPiano {
    /// Identificatore canonico (regola N).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PianoSuperato { .. } => "plan_passed",
            Self::ProvaFallita { .. } => "proof_failed",
            Self::PianoVuoto => "no_plan",
            Self::NonEseguito { .. } => "plan_not_run",
        }
    }

    /// Il verdetto BOCCIA il run? Solo una prova osservata e non conforme.
    pub fn e_bloccante(&self) -> bool {
        matches!(self, Self::ProvaFallita { .. })
    }

    /// Il criterio ha MISURATO qualcosa? Solo quando almeno una prova e' stata
    /// eseguita.
    pub fn ha_misurato(&self) -> bool {
        matches!(
            self,
            Self::PianoSuperato { .. } | Self::ProvaFallita { .. }
        )
    }

    /// Il FATTO da opporre all'agente quando il verdetto boccia. `None` quando
    /// non c'e' niente da contestare.
    ///
    /// E' l'unico punto in cui la misura diventa testo (regola Q): nasce DAI
    /// campi, e i chiamanti non ricompongono una loro descrizione.
    pub fn fatto_opponibile(&self) -> Option<String> {
        let Self::ProvaFallita { fallite } = self else {
            return None;
        };
        let elenco: Vec<String> = fallite
            .iter()
            .map(|e| {
                let osservato = match &e.esito {
                    EsitoSingolo::Fallita { osservato } => osservato.as_str(),
                    // Irraggiungibile per costruzione (`fallite` contiene solo
                    // `Fallita`): non si inventa un'osservazione, si dichiara.
                    _ => "esito non dichiarato",
                };
                format!(
                    "[{}] {} -> `{}`: {osservato}",
                    e.prova.origine.etichetta(),
                    e.prova.descrizione,
                    e.prova.comando
                )
            })
            .collect();
        Some(format!(
            "{} prove di verifica dichiarate per questo run NON sono superate: {}",
            fallite.len(),
            elenco.join(" | ")
        ))
    }
}

/// IL CRITERIO sul piano, in un posto solo.
///
/// L'ordine di precedenza e' load-bearing:
///
///  1. basta UNA prova fallita perche' il run non abbia finito, per quante ne
///     passino accanto. E' la stessa asimmetria di
///     [`super::codice_eseguibile::classifica_esecuzione`], e per la stessa
///     ragione: il caso misurato aveva un file sano e uno rotto;
///  2. basta UNA prova superata perche' il criterio abbia una misura positiva:
///     le `NonEseguibile` che l'accompagnano non declassano nulla, perche' un
///     comando che non parte non e' un difetto del codice prodotto;
///  3. senza nemmeno una prova eseguita, decide la CAUSA — nessuna prova
///     dichiarata, oppure prove dichiarate e tutte rifiutate/non partite.
pub fn classifica_piano(esiti: &[EsitoProva]) -> VerdettoPiano {
    let fallite: Vec<EsitoProva> = esiti
        .iter()
        .filter(|e| matches!(e.esito, EsitoSingolo::Fallita { .. }))
        .cloned()
        .collect();
    if !fallite.is_empty() {
        return VerdettoPiano::ProvaFallita { fallite };
    }
    let superate = esiti
        .iter()
        .filter(|e| e.esito == EsitoSingolo::Superata)
        .count();
    let non_eseguibili = esiti.len() - superate;
    if superate > 0 {
        return VerdettoPiano::PianoSuperato {
            superate,
            non_eseguibili,
        };
    }
    if esiti.is_empty() {
        return VerdettoPiano::PianoVuoto;
    }
    // Il motivo nasce dalla PRIMA prova non eseguita: e' cio' che dice a chi
    // legge se rimediare all'ambiente o alla soglia della politica.
    let motivo = esiti
        .iter()
        .find_map(|e| match &e.esito {
            EsitoSingolo::NonEseguibile { motivo } => Some(motivo.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "nessuna prova eseguita".to_string());
    VerdettoPiano::NonEseguito {
        motivo,
        non_eseguibili,
    }
}

/// L'evidenza del criterio, composta DAI campi (regola Q): nessun consumatore
/// ricostruisce il verdetto da questo testo.
pub fn evidenza_piano(verdetto: &VerdettoPiano, esiti: &[EsitoProva]) -> Value {
    let mut out = json!({
        "verdict": verdetto.as_str(),
        "bloccante": verdetto.e_bloccante(),
        "misurato": verdetto.ha_misurato(),
        "prove": {
            "dichiarate": esiti.len(),
            "superate": esiti.iter().filter(|e| e.esito == EsitoSingolo::Superata).count(),
            "fallite": esiti
                .iter()
                .filter(|e| matches!(e.esito, EsitoSingolo::Fallita { .. }))
                .count(),
            "non_eseguibili": esiti
                .iter()
                .filter(|e| matches!(e.esito, EsitoSingolo::NonEseguibile { .. }))
                .count(),
        },
    });
    let Some(o) = out.as_object_mut() else {
        return out;
    };
    // L'ELENCO per intero, con la PROVENIENZA: e' cio' su cui l'agente deve
    // tornare, e la provenienza dice se il vincolo veniva da chi non ha scritto
    // il codice — l'informazione che rende il rilievo non contestabile.
    o.insert("dettaglio".to_string(), json!(dettaglio_esiti(esiti)));
    o.insert("per_origine".to_string(), json!(per_origine(esiti)));
    if let Some(fatto) = verdetto.fatto_opponibile() {
        o.insert("verdict_text".to_string(), json!(fatto));
    }
    if let VerdettoPiano::NonEseguito { motivo, .. } = verdetto {
        o.insert("skipped_reason".to_string(), json!(motivo));
    }
    if matches!(verdetto, VerdettoPiano::PianoVuoto) {
        o.insert(
            "skipped_reason".to_string(),
            json!(
                "nessuna prova eseguibile dichiarata: ne' gli apparati advisory ne' \
                 l'agente ne hanno emesse, quindi questo criterio non ha misurato nulla"
            ),
        );
    }
    out
}

/// Una riga per prova, coi campi con cui si corregge.
fn dettaglio_esiti(esiti: &[EsitoProva]) -> Vec<Value> {
    esiti
        .iter()
        .map(|e| {
            let mut o = Map::new();
            o.insert(CAMPO_DESCRIZIONE.to_string(), json!(e.prova.descrizione));
            o.insert(CAMPO_COMANDO.to_string(), json!(e.prova.comando));
            o.insert(CAMPO_ATTESA.to_string(), e.prova.attesa.to_value());
            o.insert(CAMPO_ORIGINE.to_string(), json!(e.prova.origine.as_str()));
            o.insert("esito".to_string(), json!(e.esito.as_str()));
            match &e.esito {
                EsitoSingolo::Fallita { osservato } => {
                    o.insert("osservato".to_string(), json!(osservato));
                }
                EsitoSingolo::NonEseguibile { motivo } => {
                    o.insert("motivo".to_string(), json!(motivo));
                }
                EsitoSingolo::Superata => {}
            }
            Value::Object(o)
        })
        .collect()
}

/// Quante prove per origine: e' il dato con cui si misura se le figure stiano
/// imparando a emettere prove invece di prosa — la metrica che il design
/// dichiara come obiettivo del cambiamento.
fn per_origine(esiti: &[EsitoProva]) -> BTreeMap<&'static str, usize> {
    let mut per: BTreeMap<&'static str, usize> = BTreeMap::new();
    for e in esiti {
        *per.entry(e.prova.origine.as_str()).or_default() += 1;
    }
    per
}

// ─── Il criterio del gate ─────────────────────────────────────────────────────

/// I parametri della misura, risolti dal DB da chi costruisce il criterio
/// (regola G) e non dal runner.
#[derive(Debug, Clone, PartialEq)]
pub struct ParametriPiano {
    /// La chiave lo accende (`agent.final_gate.piano_verifica_enabled`).
    pub abilitato: bool,
    /// Pazienza per UNA prova.
    pub timeout_s: f64,
    /// Tetto di prove effettivamente eseguite in un giro di gate: oltre, la
    /// prova resta dichiarata ([`EsitoSingolo::NonEseguibile`]) e non e' una
    /// prova in piu'. Senza, un piano da duecento prove farebbe durare il gate
    /// quanto una build.
    pub max_prove: usize,
}

/// La spec del criterio, costruita QUI e non dai chiamanti: il produttore e' uno
/// solo, cosi' i test possono attraversarlo invece di fabbricare la spec a mano
/// (regola O).
///
/// Il PIANO non entra qui: a t=0 gli apparati advisory non hanno ancora
/// deliberato e l'agente non ha ancora dichiarato niente. Lo inietta
/// [`con_piano`] al momento in cui il gate costruisce i propri criteri, che e'
/// l'unico punto in cui lo stato del run e' visibile.
///
/// `politica = None` NON impedisce al criterio di nascere, ed e' deliberato: un
/// criterio che sparisse quando la sua configurazione manca sarebbe un gate
/// silenziosamente inerte — cioe' il punto di partenza di questo lavoro. Nasce,
/// e chi verifica dichiara di non aver potuto misurare.
pub fn criterio_piano(
    politica: Option<&PoliticaEsecuzione>,
    p: &ParametriPiano,
) -> Option<crate::runtime::ports::CriterionSpec> {
    use crate::runtime::ports::{CriterionProvenance, CriterionSpec};
    if !p.abilitato {
        return None;
    }
    let mut spec = Map::new();
    spec.insert(CHIAVE_MAX_PROVE.to_string(), json!(p.max_prove));
    // La chiave entra solo se la politica c'e': assente significa «non l'ho
    // potuta leggere», e un oggetto vuoto scritto qui sarebbe indistinguibile
    // da una politica che ammette tutto.
    if let Some(pol) = politica {
        spec.insert(CHIAVE_POLITICA.to_string(), pol.to_value());
    }
    Some(CriterionSpec {
        criterion_type: CRITERION_TYPE.to_string(),
        provenance: CriterionProvenance::Gate,
        spec: Value::Object(spec),
        expected: json!({}),
        timeout_s: Some(p.timeout_s),
    })
}

/// L'unico punto in cui il PIANO entra nella spec del criterio.
///
/// Sta qui e non nel nodo per la stessa ragione di
/// [`super::static_render::con_contenitore`]: chi costruisce i criteri conosce
/// lo stato, non la forma della spec — e con due punti di iniezione due gate
/// potrebbero eseguire due piani diversi sullo stesso run.
///
/// Il piano si scrive SEMPRE, anche vuoto: «nessuno ha dichiarato prove» e «non
/// ho letto il piano» sono due cose diverse, e distinguerle e' tutto il punto
/// (regola Q).
pub fn con_piano(
    mut spec: crate::runtime::ports::CriterionSpec,
    piano: &PianoDiVerifica,
) -> crate::runtime::ports::CriterionSpec {
    if let Value::Object(map) = &mut spec.spec {
        map.insert(CHIAVE_PROVE.to_string(), piano.to_value());
    }
    spec
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decisions::advisory_panel::{
        compose_advisory_synthesis, AdvisoryPolicy, AdvisoryRoster,
    };
    use crate::decisions::step_gate::MatcherKind;

    /// Una sintesi VERA composta dal produttore (regola O): un JSON scritto a
    /// mano proverebbe solo che questo modulo sa leggere cio' che il test sa
    /// scrivere, e il campo `prove` potrebbe smettere di attraversare la
    /// composizione senza che nessun test se ne accorga.
    fn sintesi(prove: &[Value]) -> Value {
        let parere = json!({
            "success": true,
            "advisory": {
                "verdict": "proceed_with_changes",
                "risks": [],
                "requirements": ["I test devono coprire i casi limite"],
                "recommendations": [],
                "prove": prove,
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

    /// LA PROVA CHE IL CASO MISURATO AVREBBE VOLUTO: il Consiglio del 17/08 aveva
    /// emesso il rischio esatto («senza un framework di test dichiarato il file
    /// di test puo' essere non eseguibile col runner predefinito») in prosa. In
    /// forma di prova e' una riga di comando e un exit code.
    fn prova_del_caso_reale() -> Value {
        json!({
            "descrizione": "il file di test parte col runner del progetto",
            "comando": "node --test calcolatrice.test.js",
            "attesa": { "tipo": "exit_code", "codice": 0 },
        })
    }

    fn politica(soglia: StepCriticality) -> PoliticaEsecuzione {
        PoliticaEsecuzione {
            soglia,
            mutatori: vec!["run_command".into(), "write_file".into()],
            regole: vec![CriticalityRule {
                matcher_kind: MatcherKind::CommandToken,
                pattern: "rm -rf".into(),
                level: StepCriticality::Irreversible,
                category: "destructive_delete".into(),
            }],
            rigenerabili: vec!["dist".into()],
            osservazione: vec!["git status".into(), "ls".into()],
        }
    }

    fn prova(comando: &str, attesa: Attesa, origine: OriginePiano) -> Prova {
        Prova {
            descrizione: format!("prova su {comando}"),
            comando: comando.to_string(),
            working_dir: None,
            attesa,
            origine,
        }
    }

    fn esito(p: Prova, e: EsitoSingolo) -> EsitoProva {
        EsitoProva { prova: p, esito: e }
    }

    // ── il vocabolario ───────────────────────────────────────────────────────

    /// Identificatori canonici e distinti (regola N).
    #[test]
    fn identificatori_canonici_distinti() {
        assert_eq!(Attesa::Uscita { codice: 0 }.as_str(), "exit_code");
        assert_eq!(
            Attesa::OutputContiene { testo: "x".into() }.as_str(),
            "output_contains"
        );
        assert_eq!(
            Attesa::OutputNonContiene { testo: "x".into() }.as_str(),
            "output_not_contains"
        );
        assert_eq!(OriginePiano::Consiglio.as_str(), "council");
        assert_eq!(OriginePiano::MultiProvider.as_str(), "multi_provider");
        assert_eq!(OriginePiano::Agente.as_str(), "agent");
        assert_eq!(
            VerdettoPiano::PianoSuperato {
                superate: 1,
                non_eseguibili: 0
            }
            .as_str(),
            "plan_passed"
        );
        assert_eq!(
            VerdettoPiano::ProvaFallita { fallite: vec![] }.as_str(),
            "proof_failed"
        );
        assert_eq!(VerdettoPiano::PianoVuoto.as_str(), "no_plan");
        assert_eq!(
            VerdettoPiano::NonEseguito {
                motivo: String::new(),
                non_eseguibili: 0
            }
            .as_str(),
            "plan_not_run"
        );
    }

    /// L'origine deriva dal vocabolario che [`super::advisory_requirements`] gia'
    /// possiede: due vocabolari per gli stessi due apparati divergerebbero al
    /// primo apparato aggiunto.
    #[test]
    fn l_origine_deriva_dal_vocabolario_advisory() {
        assert_eq!(
            OriginePiano::da_advisory(AdvisorySource::Council),
            OriginePiano::Consiglio
        );
        assert_eq!(
            OriginePiano::da_advisory(AdvisorySource::MultiProvider),
            OriginePiano::MultiProvider
        );
        assert_eq!(OriginePiano::parse("council"), Some(OriginePiano::Consiglio));
        assert_eq!(OriginePiano::parse("consiglio"), None);
        assert_eq!(OriginePiano::parse(""), None);
    }

    /// Un'attesa fuori vocabolario NON degrada a «exit 0»: quella e' un'altra
    /// prova, e la dichiarerebbe superata chiunque esca 0 per caso.
    #[test]
    fn un_attesa_fuori_vocabolario_non_diventa_exit_zero() {
        assert_eq!(Attesa::from_value(&json!({"tipo": "chissa"})), None);
        assert_eq!(Attesa::from_value(&json!({})), None);
        assert_eq!(
            Attesa::from_value(&json!({"tipo": "output_contains", "testo": "  "})),
            None,
            "un testo vuoto non e' un'attesa: la conterrebbe qualunque output"
        );
        // Il codice ASSENTE e' lo zero dichiarato dallo schema, non un ripiego.
        assert_eq!(
            Attesa::from_value(&json!({"tipo": "exit_code"})),
            Some(Attesa::Uscita { codice: 0 })
        );
    }

    /// Una prova senza comando o senza attesa si SCARTA: non e' eseguibile o non
    /// e' giudicabile, e inventare l'una o l'altra darebbe un verdetto che
    /// nessuno ha chiesto.
    #[test]
    fn una_prova_malformata_si_scarta() {
        for v in [
            json!({"attesa": {"tipo": "exit_code"}}),
            json!({"comando": "   ", "attesa": {"tipo": "exit_code"}}),
            json!({"comando": "node x.js"}),
            json!({"comando": "node x.js", "attesa": {"tipo": "boh"}}),
        ] {
            assert_eq!(
                Prova::from_value(&v, OriginePiano::Consiglio),
                None,
                "{v}"
            );
        }
        // Senza descrizione si ricade sul comando: e' cio' che si sta provando,
        // e un referto senza descrizione e' peggio di uno che ripete il comando.
        let p = Prova::from_value(
            &json!({"comando": "node --test a.js", "attesa": {"tipo": "exit_code"}}),
            OriginePiano::Consiglio,
        )
        .expect("prova valida");
        assert_eq!(p.descrizione, "node --test a.js");
        assert_eq!(p.origine, OriginePiano::Consiglio);
    }

    // ── la raccolta del piano ────────────────────────────────────────────────

    /// LE PROVE SONO L'UNIONE dei due apparati, come i requisiti: un apparato che
    /// perde la selezione dell'enforcement ha emesso le sue prove lo stesso.
    ///
    /// MUTAZIONE: comporre da un solo panel (togliere la seconda voce da
    /// `dai_pareri`) rende rosso questo test — la prova del multi-provider
    /// sparisce, che e' esattamente il difetto gia' misurato sui requisiti.
    #[test]
    fn le_prove_sono_l_unione_dei_due_apparati() {
        let piano = PianoDiVerifica::dai_pareri(&[
            (AdvisorySource::Council, sintesi(&[prova_del_caso_reale()])),
            (
                AdvisorySource::MultiProvider,
                sintesi(&[json!({
                    "descrizione": "nessun innerHTML nel bundle",
                    "comando": "grep -r innerHTML src",
                    "attesa": {"tipo": "output_not_contains", "testo": "innerHTML"},
                })]),
            ),
        ]);
        assert_eq!(piano.len(), 2, "nessun apparato viene scartato");
        assert_eq!(piano.prove[0].origine, OriginePiano::Consiglio);
        assert_eq!(piano.prove[1].origine, OriginePiano::MultiProvider);
        assert_eq!(
            piano.prove[0].comando,
            "node --test calcolatrice.test.js",
            "la prova attraversa la composizione della sintesi"
        );
    }

    /// La stessa prova chiesta da due apparati e' UNA prova, e vince la PRIMA
    /// provenienza: due voci identiche nel referto si leggerebbero come due
    /// prove cadute invece di una.
    #[test]
    fn la_stessa_prova_da_due_apparati_e_una_sola() {
        let piano = PianoDiVerifica::dai_pareri(&[
            (AdvisorySource::Council, sintesi(&[prova_del_caso_reale()])),
            (
                AdvisorySource::MultiProvider,
                sintesi(&[prova_del_caso_reale()]),
            ),
        ]);
        assert_eq!(piano.len(), 1);
        assert_eq!(piano.prove[0].origine, OriginePiano::Consiglio);
    }

    /// GIUDICE != WORKER: l'agente puo' AGGIUNGERE prove, non sostituire quelle
    /// di chi non ha scritto il codice. La sua ridichiarazione della stessa prova
    /// non le cambia l'origine, e il referto continua a dire chi l'ha chiesta.
    ///
    /// MUTAZIONE: invertire l'ordine in `unione` (l'agente per primo) fa passare
    /// la prova del Consiglio sotto l'origine `agent`, e questo test rosseggia.
    #[test]
    fn l_agente_aggiunge_prove_e_non_ne_sostituisce_nessuna() {
        let dal_consiglio =
            PianoDiVerifica::dai_pareri(&[(AdvisorySource::Council, sintesi(&[prova_del_caso_reale()]))]);
        let dall_agente = PianoDiVerifica::da_dichiarazione(Some(&json!({
            "outcome": "done",
            "summary": "fatto",
            "prove": [
                prova_del_caso_reale(),
                {
                    "descrizione": "la sorgente si importa",
                    "comando": "node -e \"require('./calcolatrice.js')\"",
                    "attesa": {"tipo": "exit_code", "codice": 0},
                },
            ],
        })));
        assert_eq!(dall_agente.len(), 2);

        let piano = PianoDiVerifica::unione(&[dal_consiglio, dall_agente]);
        assert_eq!(piano.len(), 2, "la prova ripetuta non si conta due volte");
        assert_eq!(
            piano.prove[0].origine,
            OriginePiano::Consiglio,
            "l'esecutore non si intesta la prova di chi lo giudica"
        );
        assert_eq!(piano.prove[1].origine, OriginePiano::Agente);
    }

    /// Nessuna prova dichiarata -> piano vuoto. Una dichiarazione senza il campo
    /// non e' un errore: e' un run in cui nessuno ha emesso prove.
    #[test]
    fn nessuna_prova_nessun_piano() {
        assert!(PianoDiVerifica::dai_pareri(&[]).is_empty());
        assert!(PianoDiVerifica::da_dichiarazione(None).is_empty());
        assert!(PianoDiVerifica::da_dichiarazione(Some(&json!({"outcome": "done"}))).is_empty());
        assert!(PianoDiVerifica::dai_pareri(&[(AdvisorySource::Council, sintesi(&[]))]).is_empty());
    }

    /// Il piano attraversa l'`extra` dello stato senza perdere nulla: comando,
    /// attesa, working dir e PROVENIENZA.
    ///
    /// MUTAZIONE: omettere `origine` in `Prova::to_value` fa ricadere tutte le
    /// prove su `agent` alla rilettura, e questo test rosseggia — il referto
    /// direbbe che le prove le ha chieste l'esecutore.
    #[test]
    fn il_piano_attraversa_lo_stato_senza_perdere_la_provenienza() {
        let piano = PianoDiVerifica::unione(&[
            PianoDiVerifica::dai_pareri(&[(
                AdvisorySource::MultiProvider,
                sintesi(&[json!({
                    "descrizione": "la build passa",
                    "comando": "npm run build",
                    "working_dir": "frontend",
                    "attesa": {"tipo": "output_not_contains", "testo": "error during build"},
                })]),
            )]),
            PianoDiVerifica::da_dichiarazione(Some(&json!({"prove": [prova_del_caso_reale()]}))),
        ]);
        let riletto = PianoDiVerifica::from_value(Some(&piano.to_value()));
        assert_eq!(riletto, piano, "andata e ritorno senza perdite");
        assert_eq!(riletto.prove[0].origine, OriginePiano::MultiProvider);
        assert_eq!(riletto.prove[0].working_dir.as_deref(), Some("frontend"));
        assert_eq!(riletto.prove[1].origine, OriginePiano::Agente);
    }

    // ── ammissibilita' ───────────────────────────────────────────────────────

    /// IL PIANO NON E' UN CANALE PRIVILEGIATO: una prova che chiede un comando
    /// irreversibile non si esegue, e il rifiuto nomina la regola che l'ha
    /// colpita. Il vocabolario e' quello del gate duale, non un secondo elenco.
    ///
    /// MUTAZIONE ESEGUITA: cambiare `<=` in `<` nella soglia di `ammissione`
    /// (cioe' rendere la soglia esclusiva) rifiuta anche la prova legittima e i
    /// test dell'esecuzione rosseggiano; togliere il confronto rende ammesso il
    /// `rm -rf` e rosseggia questo.
    #[test]
    fn una_prova_irreversibile_non_si_esegue() {
        let pol = politica(StepCriticality::Critical);
        let Ammissione::Rifiutata { motivo } = pol.ammissione(&prova(
            "rm -rf /var/dati",
            Attesa::Uscita { codice: 0 },
            OriginePiano::Agente,
        )) else {
            panic!("una prova distruttiva non puo' essere ammessa");
        };
        assert!(motivo.contains("irreversible"), "{motivo}");
        assert!(motivo.contains("destructive_delete"), "{motivo}");
    }

    /// Una prova ORDINARIA passa: senza, il criterio sarebbe inerte. `node
    /// --test` e' `Unconfined` per CONTRATTO del tool (esegue una riga di
    /// shell), quindi il suo pavimento e' `Critical` — ed e' la ragione per cui
    /// la soglia di default e' `critical` e non piu' stretta.
    #[test]
    fn una_prova_ordinaria_e_ammessa() {
        let pol = politica(StepCriticality::Critical);
        assert_eq!(
            pol.ammissione(&prova(
                "node --test calcolatrice.test.js",
                Attesa::Uscita { codice: 0 },
                OriginePiano::Consiglio,
            )),
            Ammissione::Ammessa {
                livello: StepCriticality::Critical
            }
        );
    }

    /// La soglia e' CONFIGURAZIONE e stringe davvero: a `observation` passano le
    /// sole righe fatte di comandi del vocabolario di osservazione. E' la leva
    /// con cui un amministratore chiude il rischio residuo senza un deploy.
    #[test]
    fn la_soglia_stretta_ammette_solo_l_osservazione() {
        let pol = politica(StepCriticality::ReadOnly);
        assert_eq!(
            pol.ammissione(&prova(
                "git status",
                Attesa::OutputContiene {
                    testo: "nothing to commit".into()
                },
                OriginePiano::Consiglio,
            )),
            Ammissione::Ammessa {
                livello: StepCriticality::ReadOnly
            },
            "un comando del vocabolario di osservazione resta ammesso"
        );
        assert!(matches!(
            pol.ammissione(&prova(
                "node --test a.js",
                Attesa::Uscita { codice: 0 },
                OriginePiano::Consiglio
            )),
            Ammissione::Rifiutata { .. }
        ));
    }

    /// La politica attraversa la spec senza perdere il vocabolario: la misura
    /// resta leggibile in cio' che ha dichiarato di aver usato per misurare.
    #[test]
    fn la_politica_attraversa_la_spec() {
        let pol = politica(StepCriticality::Critical);
        let riletta =
            PoliticaEsecuzione::from_value(Some(&pol.to_value())).expect("politica riletta");
        assert_eq!(riletta, pol);
        // Politica assente = «non l'ho potuta leggere», mai «ammetto tutto».
        assert_eq!(PoliticaEsecuzione::from_value(None), None);
        assert_eq!(PoliticaEsecuzione::from_value(Some(&json!({}))), None);
    }

    // ── il giudizio ──────────────────────────────────────────────────────────

    /// Il giudizio e' MECCANICO e nasce dai campi (regola M/Q): il codice
    /// d'uscita STRUTTURATO, il testo presente, il testo assente.
    #[test]
    fn il_giudizio_e_meccanico() {
        let ok = Osservazione {
            exit_code: Some(0),
            output: "5 pass 0 fail".into(),
        };
        assert_eq!(giudica_prova(&Attesa::Uscita { codice: 0 }, &ok), EsitoSingolo::Superata);
        assert_eq!(
            giudica_prova(
                &Attesa::OutputContiene {
                    testo: "5 pass".into()
                },
                &ok
            ),
            EsitoSingolo::Superata
        );
        assert_eq!(
            giudica_prova(
                &Attesa::OutputNonContiene {
                    testo: "fail 1".into()
                },
                &ok
            ),
            EsitoSingolo::Superata
        );
    }

    /// IL CASO REALE, in forma di giudizio: `calcolatrice.test.js` con sintassi
    /// Jest esce 1 e stampa `ReferenceError: describe is not defined`. La prova
    /// che il Consiglio avrebbe potuto emettere lo inchioda, e l'osservazione
    /// finisce nel referto.
    ///
    /// MUTAZIONE ESEGUITA: far ritornare `Superata` al ramo `Some(visto)` non
    /// conforme di `giudica` rende rosso questo test — e con esso il gate
    /// riapproverebbe il file rotto, come il 17/08.
    #[test]
    fn la_prova_del_caso_reale_boccia_il_file_rotto() {
        let rotto = Osservazione {
            exit_code: Some(1),
            output: "ReferenceError: describe is not defined".into(),
        };
        assert_eq!(
            giudica_prova(&Attesa::Uscita { codice: 0 }, &rotto),
            EsitoSingolo::Fallita {
                osservato: "exit code 1, atteso 0".into()
            }
        );
        assert_eq!(
            giudica_prova(
                &Attesa::OutputNonContiene {
                    testo: "ReferenceError".into()
                },
                &rotto
            ),
            EsitoSingolo::Fallita {
                osservato: "l'output contiene 'ReferenceError'".into()
            }
        );
    }

    /// IL CASO REALE nella forma che il design prescrive: la stessa prova
    /// (`node --test`) con attesa `OutputNonContiene "fail 1"`, contro l'output
    /// del file ROTTO e contro quello del file RIPARATO.
    ///
    /// Gli output sono quelli MISURATI il 17/08/2026 sui due file veri: la
    /// versione Jest-senza-Jest muore prima di eseguire un solo caso
    /// (`describe is not defined`, exit 1), la riscrittura con `node:test`
    /// esegue i cinque casi e li passa. La riga di riepilogo di `node --test`
    /// e' `# fail 0` quando tutto passa: e' il motivo per cui l'attesa cerca
    /// «fail 1» e non «fail», che comparirebbe SEMPRE.
    ///
    /// MUTAZIONE ESEGUITA: invertire la condizione di `OutputNonContiene` in
    /// [`giudica_prova`] scambia i due esiti — il file rotto passa e quello
    /// riparato viene bocciato — e il test rosseggia su entrambe le meta'.
    #[test]
    fn la_stessa_prova_sul_file_rotto_e_su_quello_riparato() {
        let attesa = Attesa::OutputNonContiene {
            testo: "fail 1".to_string(),
        };
        let rotto = Osservazione {
            exit_code: Some(1),
            output: "ReferenceError: describe is not defined\n\
                     # tests 1\n# pass 0\n# fail 1\n"
                .to_string(),
        };
        let riparato = Osservazione {
            exit_code: Some(0),
            output: "# tests 5\n# suites 1\n# pass 5\n# fail 0\n# cancelled 0\n".to_string(),
        };
        assert_eq!(
            giudica_prova(&attesa, &rotto),
            EsitoSingolo::Fallita {
                osservato: "l'output contiene 'fail 1'".to_string()
            }
        );
        assert_eq!(giudica_prova(&attesa, &riparato), EsitoSingolo::Superata);
        // La stessa coppia sull'attesa sul CODICE D'USCITA: le due attese sono
        // ortogonali e su questo caso concordano, il che e' cio' che rende
        // legittimo lasciare alla figura la scelta di quale emettere.
        let uscita = Attesa::Uscita { codice: 0 };
        assert!(matches!(
            giudica_prova(&uscita, &rotto),
            EsitoSingolo::Fallita { .. }
        ));
        assert_eq!(giudica_prova(&uscita, &riparato), EsitoSingolo::Superata);
    }

    /// Un exit code ASSENTE non e' un exit code sbagliato: il processo non ha
    /// prodotto uno stato d'uscita, quindi la prova non e' stata MISURATA.
    /// Bocciare qui rimanderebbe in correzione un lavoro che nessuno ha provato.
    ///
    /// Le due attese sull'OUTPUT restano giudicabili: un comando ucciso che ha
    /// comunque scritto il testo cercato lo ha scritto davvero.
    #[test]
    fn un_exit_code_assente_non_boccia() {
        let muto = Osservazione {
            exit_code: None,
            output: "qualcosa e' uscito".into(),
        };
        assert!(matches!(
            giudica_prova(&Attesa::Uscita { codice: 0 }, &muto),
            EsitoSingolo::NonEseguibile { .. }
        ));
        assert_eq!(
            giudica_prova(
                &Attesa::OutputContiene {
                    testo: "qualcosa".into()
                },
                &muto
            ),
            EsitoSingolo::Superata
        );
    }

    // ── il verdetto sul piano ────────────────────────────────────────────────

    /// IL CASO DEL DESIGN: tre prove, una superata, una fallita, una non
    /// eseguibile -> il gate NON passa, e il referto le elenca con la
    /// provenienza.
    ///
    /// MUTAZIONE ESEGUITA: far ignorare le `Fallita` a `classifica_piano`
    /// (filtrarle via prima del controllo) riporta il verdetto a
    /// `PianoSuperato { superate: 1 }` e questo test rosseggia sul verdetto e
    /// sul `fatto_opponibile`, che sparisce.
    #[test]
    fn una_prova_fallita_basta_a_bocciare_e_il_referto_la_nomina() {
        let esiti = vec![
            esito(
                prova(
                    "node --check calcolatrice.js",
                    Attesa::Uscita { codice: 0 },
                    OriginePiano::Consiglio,
                ),
                EsitoSingolo::Superata,
            ),
            esito(
                prova(
                    "node --test calcolatrice.test.js",
                    Attesa::Uscita { codice: 0 },
                    OriginePiano::Consiglio,
                ),
                EsitoSingolo::Fallita {
                    osservato: "exit code 1, atteso 0".into(),
                },
            ),
            esito(
                prova(
                    "rm -rf /tmp/x",
                    Attesa::Uscita { codice: 0 },
                    OriginePiano::Agente,
                ),
                EsitoSingolo::NonEseguibile {
                    motivo: "classificata 'irreversible'".into(),
                },
            ),
        ];
        let v = classifica_piano(&esiti);
        let VerdettoPiano::ProvaFallita { fallite } = &v else {
            panic!("atteso ProvaFallita, ottenuto {v:?}");
        };
        assert_eq!(fallite.len(), 1);
        assert!(v.e_bloccante());
        assert!(v.ha_misurato());

        let fatto = v.fatto_opponibile().expect("c'e' un fatto da opporre");
        assert!(fatto.contains("Consiglio delle Competenze"), "{fatto}");
        assert!(fatto.contains("node --test calcolatrice.test.js"), "{fatto}");
        assert!(fatto.contains("exit code 1, atteso 0"), "{fatto}");

        let ev = evidenza_piano(&v, &esiti);
        assert_eq!(ev["verdict"], json!("proof_failed"));
        assert_eq!(ev["bloccante"], json!(true));
        assert_eq!(ev["prove"]["dichiarate"], json!(3));
        assert_eq!(ev["prove"]["superate"], json!(1));
        assert_eq!(ev["prove"]["fallite"], json!(1));
        assert_eq!(ev["prove"]["non_eseguibili"], json!(1));
        assert_eq!(ev["per_origine"]["council"], json!(2));
        assert_eq!(ev["per_origine"]["agent"], json!(1));
        // Il rifiuto per soglia resta LEGGIBILE nel dettaglio: e' cio' che dice a
        // chi legge che il gate ha rinunciato a quella prova, e perche'.
        assert_eq!(ev["dettaglio"][2]["esito"], json!("not_runnable"));
        assert!(ev["dettaglio"][2]["motivo"]
            .as_str()
            .is_some_and(|m| m.contains("irreversible")));
    }

    /// Una `NonEseguibile` accanto a una `Superata` non declassa nulla: un
    /// comando che non parte non e' un difetto del codice prodotto.
    #[test]
    fn le_non_eseguibili_non_declassano_una_misura_positiva() {
        let esiti = vec![
            esito(
                prova("node --check a.js", Attesa::Uscita { codice: 0 }, OriginePiano::Agente),
                EsitoSingolo::Superata,
            ),
            esito(
                prova("pytest", Attesa::Uscita { codice: 0 }, OriginePiano::Agente),
                EsitoSingolo::NonEseguibile {
                    motivo: "oltre il tetto".into(),
                },
            ),
        ];
        assert_eq!(
            classifica_piano(&esiti),
            VerdettoPiano::PianoSuperato {
                superate: 1,
                non_eseguibili: 1
            }
        );
        assert!(classifica_piano(&esiti).ha_misurato());
    }

    /// PIANO VUOTO: il criterio non ha misurato NIENTE, e lo dichiara. Non e' un
    /// via libera — contarlo come misura positiva farebbe salire il conteggio dei
    /// criteri misurati del gate proprio nei run che non hanno dichiarato nulla.
    ///
    /// MUTAZIONE ESEGUITA: far ritornare `true` a `ha_misurato` per `PianoVuoto`
    /// rende rosso questo test, e con esso il gate tornerebbe a dire
    /// «verificato» sul silenzio — il difetto del 17/08 in forma nuova.
    #[test]
    fn un_piano_vuoto_non_e_una_verifica() {
        let v = classifica_piano(&[]);
        assert_eq!(v, VerdettoPiano::PianoVuoto);
        assert!(!v.e_bloccante(), "nessuno ha dichiarato prove: non e' un difetto");
        assert!(!v.ha_misurato(), "e nemmeno una verifica");
        assert_eq!(v.fatto_opponibile(), None);
        let ev = evidenza_piano(&v, &[]);
        assert_eq!(ev["misurato"], json!(false));
        assert!(ev["skipped_reason"].as_str().is_some_and(|m| m.contains("nessuna prova")));
    }

    /// Prove dichiarate e nessuna eseguita: non e' un piano vuoto, ed e' la
    /// distinzione che dice a chi legge se rimediare all'ambiente o alla soglia.
    #[test]
    fn prove_tutte_rifiutate_non_sono_un_piano_vuoto() {
        let esiti = vec![esito(
            prova("rm -rf /", Attesa::Uscita { codice: 0 }, OriginePiano::Agente),
            EsitoSingolo::NonEseguibile {
                motivo: "classificata 'irreversible' (destructive_delete)".into(),
            },
        )];
        let v = classifica_piano(&esiti);
        let VerdettoPiano::NonEseguito {
            motivo,
            non_eseguibili,
        } = &v
        else {
            panic!("atteso NonEseguito, ottenuto {v:?}");
        };
        assert_eq!(*non_eseguibili, 1);
        assert!(motivo.contains("irreversible"), "{motivo}");
        assert!(!v.e_bloccante(), "il codice non c'entra: non si e' guardato");
        assert!(!v.ha_misurato());
        assert!(evidenza_piano(&v, &esiti)["skipped_reason"]
            .as_str()
            .is_some_and(|m| m.contains("irreversible")));
    }

    // ── la spec del criterio ─────────────────────────────────────────────────

    fn parametri(abilitato: bool) -> ParametriPiano {
        ParametriPiano {
            abilitato,
            timeout_s: 60.0,
            max_prove: 20,
        }
    }

    /// A flag spento il criterio NON nasce: il gate resta bit-identico a prima.
    #[test]
    fn a_flag_spento_il_criterio_non_nasce() {
        assert!(criterio_piano(Some(&politica(StepCriticality::Critical)), &parametri(false)).is_none());
    }

    /// Acceso, nasce con la politica dentro la spec e riceve il piano dal solo
    /// punto che lo inietta.
    #[test]
    fn il_criterio_porta_politica_e_piano_nella_spec() {
        let pol = politica(StepCriticality::Critical);
        let base = criterio_piano(Some(&pol), &parametri(true)).expect("criterio acceso");
        assert_eq!(base.criterion_type, CRITERION_TYPE);
        assert_eq!(base.timeout_s, Some(60.0));
        assert_eq!(base.spec[CHIAVE_MAX_PROVE], json!(20));
        assert!(
            base.spec.get(CHIAVE_PROVE).is_none(),
            "a t=0 nessuno ha ancora dichiarato prove"
        );

        let piano = PianoDiVerifica::dai_pareri(&[(
            AdvisorySource::Council,
            sintesi(&[prova_del_caso_reale()]),
        )]);
        let con = con_piano(base, &piano);
        assert_eq!(
            PianoDiVerifica::from_value(con.spec.get(CHIAVE_PROVE)),
            piano
        );
        assert_eq!(
            PoliticaEsecuzione::from_value(con.spec.get(CHIAVE_POLITICA)),
            Some(pol)
        );
    }

    /// Senza politica il criterio nasce COMUNQUE e senza la chiave: e' cio' che
    /// permette a chi verifica di dichiarare «non ho potuto misurare» invece di
    /// sparire in silenzio, che sarebbe di nuovo un gate inerte.
    #[test]
    fn senza_politica_il_criterio_nasce_e_lo_dichiara() {
        let c = criterio_piano(None, &parametri(true)).expect("criterio acceso");
        assert!(c.spec.get(CHIAVE_POLITICA).is_none());
    }

    /// Il piano VUOTO si scrive lo stesso: «nessuno ha dichiarato prove» e «non
    /// ho letto il piano» sono due cose diverse (regola Q).
    #[test]
    fn il_piano_vuoto_si_scrive_lo_stesso() {
        let c = con_piano(
            criterio_piano(Some(&politica(StepCriticality::Critical)), &parametri(true))
                .expect("criterio acceso"),
            &PianoDiVerifica::default(),
        );
        assert_eq!(c.spec[CHIAVE_PROVE], json!([]));
    }
}
