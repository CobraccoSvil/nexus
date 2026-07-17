//! Il MONDO FINTO della batteria di qualificazione: risponde alle tool-call dei
//! profili multi-step (`tool_chain`, `tool_recovery`) in modo deterministico, senza
//! toccare filesystem, processi o rete.
//!
//! # Perche' finto
//!
//! Non e' un ripiego, e' la conclusione a cui e' arrivata la letteratura dopo
//! essersi scottata: API-Bank hard-codifica le risposte "to maintain result
//! consistency", e StableToolBench ha misurato che solo il 44,4% delle chiamate API
//! reali di ToolBench riesce — un benchmark che dipende da servizi vivi misura la
//! salute dei servizi, non i modelli. Con un filesystem vero `min_chained_calls`
//! misurerebbe *cosa c'e' nella tempdir*, e il recupero dipenderebbe dai messaggi
//! d'errore del sistema operativo, che cambiano per OS.
//!
//! Sintetiche sono le RISPOSTE, non gli schemi: i tool dichiarati al modello restano
//! quelli veri del catalogo. Un tool finto misurerebbe il nostro mock.
//!
//! # Gli handle opachi, e perche' non si accettano i path
//!
//! I nomi dei tool (`read_file`, `list_files`) hanno un prior enorme nel
//! pre-training: il modello ha visto milioni di `read_file("src/main.rs")` e, se il
//! mondo accettasse un path plausibile, salterebbe il primo anello della catena
//! inventandoselo — e la dipendenza di dati non sarebbe provata. Qui un bersaglio e'
//! valido solo se e' un HANDLE che questo mondo ha emesso: un path letterale prende
//! `E_HANDLE_REQUIRED`. E' l'avvertimento di ToolFailBench (il ritorno del tool deve
//! contraddire il valore plausibile in memoria parametrica) e la forma del Gorilla
//! File System, dove "the errors are not exceptions but return values".
//!
//! # Determinismo e freschezza insieme
//!
//! Ogni token nasce da SHA-256 di (provider, model, profile_key, attempt, anello):
//! stabile — la stessa riga di evidence si riproduce bit a bit — ma diverso a ogni
//! tentativo, quindi non memorizzabile. Una costante nel repo proverebbe la memoria,
//! non la lettura: il needle fisso `NX7K2P9QW4` di `long_context` e' esattamente
//! l'errore da non ripetere (GPT-4-base recita il GUID di BIG-bench).

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Alfabeto base32 senza vocali e senza caratteri ambigui (0/O, 1/I/L): i token
/// finiscono nei log e negli argomenti JSON, e non devono poter essere confusi a
/// occhio ne' formare parole che il modello possa "riconoscere".
const ALFABETO: &[u8] = b"23456789BCDFGHJKMNPQRSTVWXZ";

/// Da cosa nasce ogni token di un tentativo. Sta tutto in `ai_model_probe_evidence`
/// tranne il `seed`, che la migrazione 0610 aggiunge: senza, un fallimento contestato
/// non e' riproducibile e la diagnosi e' cieca (la replayable fault injection di
/// ToolMisuseBench).
#[derive(Debug, Clone)]
pub(crate) struct TokenSeed {
    pub provider: String,
    pub model: String,
    pub profile_key: String,
    pub attempt: i32,
    /// Il seme della singola esecuzione: rende l'istanza fresca. Registrato
    /// nell'evidenza per poterla rigiocare identica.
    pub seed: u64,
}

impl TokenSeed {
    /// Un token opaco di `lunghezza` caratteri per l'etichetta `label`.
    ///
    /// 10 caratteri su questo alfabeto sono ~47 bit: indovinarlo e' dell'ordine di
    /// 1e-14. "Passato per fortuna" non e' escluso per convinzione, e' escluso per
    /// misura.
    fn token(&self, label: &str, lunghezza: usize) -> String {
        let mut h = Sha256::new();
        h.update(self.provider.as_bytes());
        h.update([0u8]);
        h.update(self.model.as_bytes());
        h.update([0u8]);
        h.update(self.profile_key.as_bytes());
        h.update([0u8]);
        h.update(self.attempt.to_le_bytes());
        h.update(self.seed.to_le_bytes());
        h.update(label.as_bytes());
        let d = h.finalize();
        d.iter()
            .take(lunghezza)
            .map(|b| ALFABETO[*b as usize % ALFABETO.len()] as char)
            .collect()
    }

    /// L'handle dell'anello `k` della catena. E' il bersaglio che il modello deve
    /// portare nella chiamata successiva.
    pub(crate) fn handle(&self, k: usize) -> String {
        format!("H-{}", self.token(&format!("chain:{k}"), 10))
    }

    /// Un handle DISTRATTORE per l'anello `k`: nasce dallo stesso seme ma con
    /// etichetta diversa, quindi e' della stessa forma e della stessa lunghezza del
    /// vero. Serve a distinguere chi discrimina da chi abbina per somiglianza.
    pub(crate) fn esca(&self, k: usize) -> String {
        format!("H-{}", self.token(&format!("esca:{k}"), 10))
    }

    /// Il token che vive UNICAMENTE dentro un messaggio d'errore. E' cio' che rende
    /// verificabile "il recupero e' informato dall'errore" senza chiedere il parere
    /// di un LLM: se la chiamata dopo lo contiene, il modello ha LETTO l'errore —
    /// non c'e' altro posto da cui possa averlo preso.
    pub(crate) fn token_errore(&self, label: &str) -> String {
        format!("E-{}", self.token(&format!("errore:{label}"), 10))
    }
}

/// La risposta del mondo a UNA tool-call.
#[derive(Debug, Clone)]
pub(crate) struct WorldReply {
    /// Cio' che vede il MODELLO. Solo testo: il messaggio di tool_result non ha
    /// campi `is_error`/`exit_code` sul filo, esattamente come in produzione.
    pub text: String,
    /// Cio' che sappiamo NOI, per costruzione: l'errore l'abbiamo piantato noi.
    /// Il predicato legge QUESTO, mai `text`. Non e' una violazione della regola M:
    /// la regola vieta di dedurre lo stato tecnico dalla prosa ALTRUI, e questa
    /// prosa e' nostra.
    pub is_error: bool,
    /// Il token che questa risposta ha piantato, se ne ha piantato uno. E' la sola
    /// fonte di verita' del taint tracking: contiamo un anello solo se la chiamata
    /// successiva porta un token che SAPPIAMO di aver emesso.
    pub planted: Option<String>,
}

impl WorldReply {
    fn ok(text: impl Into<String>, planted: Option<String>) -> Self {
        Self { text: text.into(), is_error: false, planted }
    }

    /// Errore-come-valore-di-ritorno (Gorilla File System): il modello lo riceve
    /// come un normale tool_result, non come un'eccezione di trasporto.
    fn errore(codice: &str, extra: Value) -> Self {
        let mut body = json!({ "error": { "code": codice } });
        if let Some(o) = extra.as_object() {
            for (k, v) in o {
                body["error"][k] = v.clone();
            }
        }
        Self { text: body.to_string(), is_error: true, planted: None }
    }
}

/// Cosa sta misurando questo mondo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorldKind {
    /// `tool_chain` (high): la profondita' di data-flow.
    Catena,
    /// `tool_recovery` (heavy): il recupero informato da un errore.
    Recupero,
}

/// Il mondo: una funzione (nome_tool, input) -> risposta, piu' lo stato minimo dei
/// token emessi. Puro: niente filesystem, niente processi, niente DB.
#[derive(Debug)]
pub(crate) struct ScriptedWorld {
    kind: WorldKind,
    seed: TokenSeed,
    /// Gli handle che il mondo ha gia' consegnato, in ordine di anello.
    emessi: Vec<String>,
    /// Quante volte ogni tool e' stato chiamato: serve al trigger del guasto
    /// (`nth_call`), che e' dichiarativo e deterministico — mai probabilistico.
    chiamate: Vec<String>,
    /// Il guasto e' gia' scattato? L'attivazione e' FIRST-TOUCH: al primo contatto
    /// col tool bersaglio, sempre, non con una probabilita'.
    guasto_scattato: bool,
    /// Il token piantato nell'errore, quando il guasto e' scattato.
    token_errore: Option<String>,
}

impl ScriptedWorld {
    /// Costruisce il mondo. `Err` se un token che va GUADAGNATO comparirebbe gia'
    /// nella richiesta iniziale: `long_context` affida la stessa regola a un COMMENTO
    /// ("il needle non compare MAI nel system prompt"), qui e' un invariante che costa
    /// due `contains` e trasforma una promessa in un guard. Un token a valle gia'
    /// visibile renderebbe la catena scorciatoiabile e il test misurerebbe la nostra
    /// ingenuita' invece del modello.
    ///
    /// `handle(0)` e' l'ECCEZIONE, e non e' un'indulgenza: e' l'anello di partenza, e
    /// la richiesta DEVE nominarlo o il modello non ha da dove cominciare (lo dice
    /// `handle_iniziale`). Vietarlo insieme agli altri e' costato 32 giri su 32
    /// inconclusive: il guard rifiutava esattamente cio' che il progetto impone.
    /// Il mondo sa da se' qual e' il suo ingresso e non chiede al chiamante di
    /// dichiararlo: nessun accordo da tenere allineato, quindi nessuno da rompere.
    pub(crate) fn new(
        kind: WorldKind,
        seed: TokenSeed,
        richiesta_iniziale: &[&str],
    ) -> Result<Self, String> {
        let mondo = Self {
            kind,
            seed,
            emessi: Vec::new(),
            chiamate: Vec::new(),
            guasto_scattato: false,
            token_errore: None,
        };
        let nella_richiesta =
            |tok: &str| richiesta_iniziale.iter().any(|t| t.contains(tok));
        for k in 0..8 {
            // Gli handle a valle si guadagnano seguendo la catena; le esche viaggiano
            // solo nelle risposte, quindi nessuna e' ammessa nella richiesta.
            if k > 0 && nella_richiesta(&mondo.seed.handle(k)) {
                return Err(format!(
                    "token a valle {} gia' presente nella richiesta",
                    mondo.seed.handle(k)
                ));
            }
            if nella_richiesta(&mondo.seed.esca(k)) {
                return Err(format!("esca {} gia' presente nella richiesta", mondo.seed.esca(k)));
            }
        }
        Ok(mondo)
    }

    /// L'anello di partenza: e' l'UNICO bersaglio che la richiesta iniziale nomina.
    pub(crate) fn handle_iniziale(&self) -> String {
        self.seed.handle(0)
    }

    /// Gli handle emessi finora (il taint tracking confronta contro questi).
    pub(crate) fn emessi(&self) -> &[String] {
        &self.emessi
    }

    /// Il token piantato nell'errore, se il guasto e' scattato.
    pub(crate) fn token_errore_emesso(&self) -> Option<&str> {
        self.token_errore.as_deref()
    }

    /// Risponde a UNA tool-call. Il bersaglio si cerca in TUTTI i valori-foglia
    /// dell'input, mai in un campo con un nome preciso: quale campo usare lo decide
    /// il modello, e bocciarlo perche' ha scritto `handle` invece di `path`
    /// misurerebbe la nostra convenzione, non la sua capacita'.
    pub(crate) fn answer(&mut self, nome: &str, input: &Value) -> WorldReply {
        self.chiamate.push(nome.to_string());
        let pagliaio = foglie_concatenate(input);
        match self.kind {
            WorldKind::Catena => self.risposta_catena(&pagliaio),
            WorldKind::Recupero => self.risposta_recupero(nome, &pagliaio),
        }
    }

    /// La catena: chi indirizza l'anello k riceve l'anello k+1. Il match sul token
    /// PRECEDE qualunque considerazione sul nome del tool: `run_command` con
    /// `cat H-XXX` deve valere quanto `read_file` con `path: H-XXX` — il modello e'
    /// libero di scegliere lo strumento, e bocciare una preferenza di stile fra
    /// `cat` e `read_file` misurerebbe noi.
    fn risposta_catena(&mut self, pagliaio: &str) -> WorldReply {
        // Dall'anello piu' avanti all'indietro: un modello che ripresenta un handle
        // vecchio non guadagna un anello nuovo.
        for k in (0..=self.emessi.len()).rev() {
            if pagliaio.contains(&self.seed.handle(k)) {
                return self.pianta_prossimo(k);
            }
        }
        if pagliaio.contains(&self.seed.esca(0)) || pagliaio.contains(&self.seed.esca(1)) {
            // Ha abboccato al distrattore: non e' un errore di trasporto, e' una
            // risorsa che non esiste. Nessun anello.
            return WorldReply::errore("E_HANDLE_UNKNOWN", json!({}));
        }
        // Nessun handle: il modello ha inventato un bersaglio (tipicamente un path
        // plausibile dal pre-training).
        WorldReply::errore(
            "E_HANDLE_REQUIRED",
            json!({ "hint": "il bersaglio deve essere un handle ottenuto da una chiamata precedente" }),
        )
    }

    /// Consegna l'anello k+1 a chi ha indirizzato il k.
    fn pianta_prossimo(&mut self, k: usize) -> WorldReply {
        let prossimo = self.seed.handle(k + 1);
        let esca = self.seed.esca(k + 1);
        if self.emessi.len() <= k {
            self.emessi.resize(k + 1, String::new());
        }
        if !self.emessi.contains(&prossimo) {
            self.emessi.push(prossimo.clone());
        }
        // Il distrattore viaggia INSIEME al vero, nella stessa risposta e nella
        // stessa forma: chi abbina per somiglianza sbaglia, chi legge discrimina.
        WorldReply::ok(
            json!({
                "entries": [
                    { "ref": prossimo, "state": "current" },
                    { "ref": esca, "state": "superseded" }
                ],
                "note": "usa la voce current"
            })
            .to_string(),
            Some(prossimo),
        )
    }

    /// Il recupero: il primo contatto col tool bersaglio fallisce, SEMPRE, e l'errore
    /// porta con se' il dato che serve a rimediare. Il token nell'errore e' cio' che
    /// rende "informato" un fatto invece di un giudizio: non esiste altro posto da
    /// cui il modello possa averlo preso.
    fn risposta_recupero(&mut self, nome: &str, pagliaio: &str) -> WorldReply {
        if !self.guasto_scattato {
            self.guasto_scattato = true;
            let tok = self.seed.token_errore(nome);
            self.token_errore = Some(tok.clone());
            // Il dato c'e', l'azione no: l'handle e' scaduto e l'errore dice quale
            // sia quello valido. Ripetere identico non puo' funzionare; leggere si'.
            return WorldReply::errore(
                "E_HANDLE_STALE",
                json!({ "current_epoch": tok, "retryable": false }),
            );
        }
        match self.token_errore.as_deref() {
            // Ha portato il token che solo l'errore conteneva: ha letto e si e'
            // adattato.
            Some(t) if pagliaio.contains(t) => WorldReply::ok(
                json!({ "ok": true, "note": "epoch accettata" }).to_string(),
                Some(t.to_string()),
            ),
            // Ha riprovato senza il token: la stessa azione, o una diversa ma cieca.
            _ => WorldReply::errore("E_HANDLE_STALE", json!({ "retryable": false })),
        }
    }
}

/// Tutti i valori-foglia dell'input, concatenati. Serve a cercare un token
/// OVUNQUE il modello l'abbia messo: in `path`, in `handle`, dentro `command`, in un
/// array annidato. Il nome del campo e' una convenzione nostra; il fatto e' che il
/// token sia stato riportato.
fn foglie_concatenate(v: &Value) -> String {
    let mut out = String::new();
    raccogli(v, &mut out);
    out
}

fn raccogli(v: &Value, out: &mut String) {
    match v {
        Value::String(s) => {
            out.push_str(s);
            out.push('\n');
        }
        Value::Array(a) => a.iter().for_each(|x| raccogli(x, out)),
        Value::Object(o) => o.values().for_each(|x| raccogli(x, out)),
        altro => {
            out.push_str(&altro.to_string());
            out.push('\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seme() -> TokenSeed {
        TokenSeed {
            provider: "p".into(),
            model: "m".into(),
            profile_key: "agentic_chain".into(),
            attempt: 1,
            seed: 42,
        }
    }

    /// Stesso seme = stessi token (una riga di evidence si rigioca identica);
    /// tentativo diverso = token diversi (l'istanza e' fresca, non memorizzabile).
    #[test]
    fn i_token_sono_deterministici_ma_freschi() {
        let a = seme();
        let b = seme();
        assert_eq!(a.handle(0), b.handle(0), "stesso seme -> stesso token");

        let mut c = seme();
        c.attempt = 2;
        assert_ne!(a.handle(0), c.handle(0), "tentativo diverso -> token diverso");

        let mut d = seme();
        d.seed = 43;
        assert_ne!(a.handle(0), d.handle(0), "seed diverso -> token diverso");
    }

    /// Il vero e l'esca sono indistinguibili per forma: chi discrimina lo fa
    /// leggendo, non guardando la lunghezza.
    #[test]
    fn l_esca_ha_la_stessa_forma_del_vero() {
        let s = seme();
        assert_ne!(s.handle(1), s.esca(1));
        assert_eq!(s.handle(1).len(), s.esca(1).len());
        assert!(s.handle(1).starts_with("H-") && s.esca(1).starts_with("H-"));
    }

    /// L'INVARIANTE: nessun token puo' essere gia' nella richiesta iniziale,
    /// altrimenti la catena e' scorciatoiabile e il test misura la nostra ingenuita'
    /// invece del modello. E' la regola del needle, resa guard.
    #[test]
    fn un_token_gia_nella_richiesta_rende_il_mondo_non_costruibile() {
        let s = seme();
        let trapelato = s.handle(2);
        let e = ScriptedWorld::new(WorldKind::Catena, seme(), &[&trapelato]);
        assert!(e.is_err(), "un token visibile nella richiesta deve impedire il giro");
        assert!(ScriptedWorld::new(WorldKind::Catena, seme(), &["nessun token qui"]).is_ok());
    }

    /// Un path inventato non apre la catena: e' il prior del pre-training
    /// (`read_file("src/main.rs")`) che va negato, o il primo anello si salta.
    #[test]
    fn un_path_letterale_non_e_un_bersaglio() {
        let mut w = ScriptedWorld::new(WorldKind::Catena, seme(), &[]).unwrap();
        let r = w.answer("read_file", &json!({ "path": "src/main.rs" }));
        assert!(r.is_error);
        assert!(r.text.contains("E_HANDLE_REQUIRED"));
        assert!(r.planted.is_none(), "un bersaglio inventato non consegna anelli");
    }

    /// Il tool non conta, conta il bersaglio: `run_command` con `cat <handle>` deve
    /// valere quanto `read_file`. Bocciare la scelta dello strumento misurerebbe la
    /// nostra preferenza di stile.
    #[test]
    fn il_bersaglio_conta_il_nome_del_tool_no() {
        let mut w = ScriptedWorld::new(WorldKind::Catena, seme(), &[]).unwrap();
        let h0 = w.handle_iniziale();
        let r = w.answer("run_command", &json!({ "command": format!("cat {h0}") }));
        assert!(!r.is_error, "l'handle era dentro il comando: e' un bersaglio valido");
        assert!(r.planted.is_some(), "chi indirizza l'anello 0 riceve l'anello 1");
    }

    /// L'esca non apre nulla, e non e' un errore di trasporto: e' una risorsa che
    /// non esiste.
    #[test]
    fn chi_abbocca_all_esca_non_avanza() {
        let mut w = ScriptedWorld::new(WorldKind::Catena, seme(), &[]).unwrap();
        let h0 = w.handle_iniziale();
        w.answer("read_file", &json!({ "path": h0 })); // anello 1 consegnato (con esca)
        let esca = seme().esca(1);
        let r = w.answer("read_file", &json!({ "path": esca }));
        assert!(r.is_error);
        assert!(r.text.contains("E_HANDLE_UNKNOWN"));
        assert!(r.planted.is_none());
    }

    /// RECUPERO: il primo contatto fallisce sempre (first-touch, mai una
    /// probabilita') e l'errore porta il dato che serve.
    #[test]
    fn il_primo_contatto_fallisce_e_l_errore_porta_il_dato() {
        let mut w = ScriptedWorld::new(WorldKind::Recupero, seme(), &[]).unwrap();
        let r = w.answer("read_file", &json!({ "path": "qualunque" }));
        assert!(r.is_error);
        assert!(r.text.contains("E_HANDLE_STALE"));
        let tok = w.token_errore_emesso().expect("il guasto ha piantato il token").to_string();
        assert!(r.text.contains(&tok), "il token vive dentro il messaggio d'errore");
    }

    /// Il RECUPERO VERO: la chiamata dopo porta il token che SOLO l'errore conteneva.
    /// Non c'e' altro posto da cui possa averlo preso: "informato dall'errore" e' un
    /// fatto, non un giudizio.
    #[test]
    fn chi_porta_il_token_dell_errore_ha_letto_l_errore() {
        let mut w = ScriptedWorld::new(WorldKind::Recupero, seme(), &[]).unwrap();
        w.answer("read_file", &json!({ "path": "x" }));
        let tok = w.token_errore_emesso().unwrap().to_string();
        let r = w.answer("read_file", &json!({ "epoch": tok }));
        assert!(!r.is_error, "ha usato il dato dell'errore: recupero riuscito");
    }

    /// Ripetere la chiamata fallita non recupera. E nemmeno cambiare tool a caso:
    /// senza il token, l'azione e' cieca.
    #[test]
    fn ripetere_o_cambiare_alla_cieca_non_e_un_recupero() {
        let mut w = ScriptedWorld::new(WorldKind::Recupero, seme(), &[]).unwrap();
        w.answer("read_file", &json!({ "path": "x" }));
        assert!(w.answer("read_file", &json!({ "path": "x" })).is_error, "identica");
        assert!(w.answer("list_files", &json!({ "path": "y" })).is_error, "diversa ma cieca");
    }
}
