//! Le MISURE dei profili multi-step: quanti anelli il modello ha davvero
//! concatenato, e se ha recuperato leggendo l'errore.
//!
//! Parte PURA (niente rete, niente DB): prende la traccia delle tool-call di un
//! tentativo e i token che il mondo sa di aver emesso, e ne ricava fatti. Il loop
//! che produce la traccia sta altrove; qui c'e' cio' che decide, ed e' separato
//! apposta perche' e' la parte che va provata riga per riga.
//!
//! # Perche' non si contano le chiamate
//!
//! `min_chained_calls: 3` letto come "ha fatto 3 chiamate" e' gameable: tre
//! `list_files` di fila certificherebbero `high`. E' precisamente cio' che il
//! response checker di BFCL si rifiuta di fare. Il conteggio dev'essere una
//! CONSEGUENZA della dipendenza di dati, non il predicato:
//!
//!   un anello e' una chiamata che porta un token che NOI sappiamo di aver emesso
//!   in risposta a una chiamata precedente.
//!
//! Il modello non puo' produrre quel token in altro modo che leggendolo: e' ~47 bit
//! di SHA-256 su un seme fresco. Quindi il taint tracking e' un'uguaglianza di
//! stringhe su un fatto, non un'interpretazione — e non serve nessun giudice.
//!
//! # Perche' si tollerano le chiamate in piu'
//!
//! Il predicato e' "esiste una sottosequenza dipendente lunga N", non "ha fatto
//! esattamente N chiamate" (il subset matching di BFCL). Esplorare, sbagliare
//! bersaglio, chiamare due tool nello stesso turno: sono comportamenti che da un
//! agente vogliamo, e penalizzarli misurerebbe la nostra idea di eleganza. Rigore
//! sul fatto, tolleranza sulla forma.

use serde_json::Value;

/// UNA chiamata a tool nella traccia di un tentativo.
#[derive(Debug, Clone)]
pub(crate) struct TracedCall {
    /// Il turno in cui e' stata emessa (0-based). Serve a non contare come anello
    /// un token che il modello ha ricevuto DOPO averlo usato.
    pub turno: usize,
    pub nome: String,
    /// Gli argomenti cosi' come il modello li ha emessi.
    pub input: Value,
    /// `true` se gli argomenti non erano JSON valido. Non e' un dettaglio: senza
    /// questo, un modello che non sa serializzare gli `arguments` collassa in
    /// `input = {}` e il checker lo legge come "non ha concatenato" — due difetti
    /// opposti che diventano lo stesso verdetto, e la diagnosi e' impossibile.
    pub input_malformato: bool,
}

/// Cosa il tentativo ha DIMOSTRATO. Sono fatti, non voti: il predicato li confronta
/// con le soglie del profilo.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AttemptMeasures {
    /// La lunghezza della catena di dipendenza piu' lunga osservata.
    pub chained_links: usize,
    /// Il modello ha portato, dopo il guasto, il token che viveva SOLO nel messaggio
    /// d'errore: l'ha letto.
    pub recovered: bool,
    /// Ha ripetuto identica (stesso tool, stessi argomenti) una chiamata gia'
    /// fallita.
    pub repeated_failed: bool,
    /// Almeno una chiamata aveva argomenti non parsabili: il modello puo' aver
    /// fallito per FORMATO, non per pianificazione.
    pub bad_tool_syntax: bool,
}

/// La firma di una chiamata: nome + argomenti canonicalizzati. Due chiamate sono
/// "la stessa" se hanno la stessa firma — l'ordine delle chiavi in un oggetto JSON
/// non e' informazione, e trattarlo come tale farebbe passare per "azione diversa"
/// una ripetizione identica.
pub(crate) fn firma(nome: &str, input: &Value) -> String {
    format!("{nome}({})", canonicalizza(input))
}

fn canonicalizza(v: &Value) -> String {
    match v {
        Value::Object(o) => {
            let mut chiavi: Vec<_> = o.iter().collect();
            chiavi.sort_by(|a, b| a.0.cmp(b.0));
            let corpo: Vec<String> = chiavi
                .iter()
                .map(|(k, v)| format!("{k}={}", canonicalizza(v)))
                .collect();
            format!("{{{}}}", corpo.join(","))
        }
        Value::Array(a) => {
            let corpo: Vec<String> = a.iter().map(canonicalizza).collect();
            format!("[{}]", corpo.join(","))
        }
        Value::String(s) => s.trim().to_string(),
        altro => altro.to_string(),
    }
}

/// Tutti i valori-foglia di un input, concatenati: il token si cerca OVUNQUE il
/// modello l'abbia messo. Quale campo usare lo decide lui.
fn foglie(v: &Value) -> String {
    let mut out = String::new();
    fn scendi(v: &Value, out: &mut String) {
        match v {
            Value::String(s) => {
                out.push_str(s);
                out.push('\n');
            }
            Value::Array(a) => a.iter().for_each(|x| scendi(x, out)),
            Value::Object(o) => o.values().for_each(|x| scendi(x, out)),
            altro => {
                out.push_str(&altro.to_string());
                out.push('\n');
            }
        }
    }
    scendi(v, &mut out);
    out
}

/// Un token emesso dal mondo, con il turno in cui e' stato consegnato.
#[derive(Debug, Clone)]
pub(crate) struct TokenEmesso {
    pub token: String,
    /// Il turno in cui il mondo l'ha consegnato. Una chiamata puo' consumarlo solo
    /// DOPO: contare un token "usato" prima di essere emesso significherebbe
    /// premiare una coincidenza.
    pub turno: usize,
}

/// La catena di dipendenza piu' lunga osservata nella traccia.
///
/// Un anello e' una chiamata che porta un token emesso in un turno PRECEDENTE. La
/// profondita' e' quanti token distinti della catena sono stati consumati, in
/// ordine di emissione: e' la sottosequenza dipendente, non il numero di chiamate.
pub(crate) fn conta_anelli(traccia: &[TracedCall], emessi: &[TokenEmesso]) -> usize {
    let mut consumati = 0usize;
    for (i, tok) in emessi.iter().enumerate() {
        let usato = traccia.iter().any(|c| {
            // Solo DOPO l'emissione: un token non puo' essere usato prima di esistere.
            c.turno > tok.turno && foglie(&c.input).contains(&tok.token)
        });
        if !usato {
            // La catena si spezza qui: gli anelli oltre non sono raggiungibili
            // (ogni token nasce solo dalla risposta al precedente).
            break;
        }
        consumati = i + 1;
    }
    consumati
}

/// `true` se dopo il guasto il modello ha portato il token che viveva SOLO nel
/// messaggio d'errore.
///
/// E' la prova che ha LETTO l'errore invece di ripetere o inventare: quel token non
/// esiste in nessun altro punto della conversazione. Non c'e' prosa da interpretare,
/// non serve un giudice — che e' esattamente cio' che il campo non sa fare senza
/// (ToolFailBench misura k~0,65 fra regola e giudice LLM).
pub(crate) fn ha_recuperato(traccia: &[TracedCall], token_errore: Option<&str>, turno_errore: usize) -> bool {
    let Some(tok) = token_errore else { return false };
    traccia
        .iter()
        .any(|c| c.turno > turno_errore && foglie(&c.input).contains(tok))
}

/// `true` se il modello ha rimandato una chiamata IDENTICA (stessa firma) a una che
/// gli era gia' fallita.
///
/// Attenzione a cosa significa: su un guasto TRANSITORIO ritentare e' la mossa
/// giusta, e BFCL promuove esplicitamente chi lo fa ("it ultimately achieved the
/// goal"). Questo fatto va letto solo dove il guasto e' dichiarato PERMANENTE — se
/// no si boccia il comportamento corretto, e per giunta quello che il nostro stesso
/// prompt di sistema ordina ("se un'operazione fallisce, riprova").
pub(crate) fn ha_ripetuto_la_fallita(traccia: &[TracedCall], firme_fallite: &[String]) -> bool {
    // Una firma fallita e' NELLA traccia per definizione: e' la chiamata che ha
    // fallito. Ripetere significa che compare PIU' di una volta — contare la prima
    // occorrenza accuserebbe di ripetizione chiunque sbagli anche una sola volta,
    // cioe' proprio il modello che poi si corregge.
    firme_fallite.iter().any(|f| {
        traccia.iter().filter(|c| &firma(&c.nome, &c.input) == f).count() > 1
    })
}

/// Le misure di un tentativo, dalla traccia. PURA.
pub(crate) fn misura(
    traccia: &[TracedCall],
    emessi: &[TokenEmesso],
    token_errore: Option<&str>,
    turno_errore: usize,
    firme_fallite: &[String],
) -> AttemptMeasures {
    AttemptMeasures {
        chained_links: conta_anelli(traccia, emessi),
        recovered: ha_recuperato(traccia, token_errore, turno_errore),
        repeated_failed: ha_ripetuto_la_fallita(traccia, firme_fallite),
        bad_tool_syntax: traccia.iter().any(|c| c.input_malformato),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(turno: usize, nome: &str, input: Value) -> TracedCall {
        TracedCall { turno, nome: nome.into(), input, input_malformato: false }
    }

    fn emesso(turno: usize, token: &str) -> TokenEmesso {
        TokenEmesso { token: token.into(), turno }
    }

    /// IL TEST SIMMETRICO, quello che la ricerca chiede per primo: tre chiamate che
    /// NON sono concatenate non fanno una catena. Senza, il rischio si sposta solo
    /// dal "predicato muto" al "loop che conta gli anelli sbagliati" — la stessa
    /// verifica cieca, un piano piu' in la'.
    #[test]
    fn tre_chiamate_non_concatenate_non_sono_una_catena() {
        let traccia = vec![
            call(1, "list_files", json!({ "path": "/a" })),
            call(2, "list_files", json!({ "path": "/b" })),
            call(3, "list_files", json!({ "path": "/c" })),
        ];
        let emessi = vec![emesso(1, "H-AAA"), emesso(2, "H-BBB"), emesso(3, "H-CCC")];
        assert_eq!(
            conta_anelli(&traccia, &emessi),
            0,
            "tre chiamate a caso non certificano high: nessuna porta un token nostro"
        );
    }

    /// La catena vera: ogni chiamata porta il token consegnato dalla precedente.
    #[test]
    fn la_catena_conta_gli_anelli_consumati() {
        let traccia = vec![
            call(1, "read_file", json!({ "path": "H-AAA" })),
            call(2, "read_file", json!({ "path": "H-BBB" })),
            call(3, "read_file", json!({ "path": "H-CCC" })),
        ];
        let emessi = vec![emesso(0, "H-AAA"), emesso(1, "H-BBB"), emesso(2, "H-CCC")];
        assert_eq!(conta_anelli(&traccia, &emessi), 3);
    }

    /// Il fan-out non e' una catena: tre chiamate nello STESSO turno non possono
    /// dipendere l'una dall'altra, perche' il modello non ha ancora visto le
    /// risposte. E' il difetto piu' probabile coi provider che emettono piu'
    /// tool-call per turno.
    #[test]
    fn tre_chiamate_nello_stesso_turno_non_sono_una_catena() {
        let traccia = vec![
            call(1, "read_file", json!({ "path": "H-AAA" })),
            call(1, "read_file", json!({ "path": "H-BBB" })),
            call(1, "read_file", json!({ "path": "H-CCC" })),
        ];
        // I token B e C sono stati emessi al turno 1: usarli al turno 1 e'
        // impossibile, quindi non contano.
        let emessi = vec![emesso(0, "H-AAA"), emesso(1, "H-BBB"), emesso(1, "H-CCC")];
        assert_eq!(
            conta_anelli(&traccia, &emessi),
            1,
            "solo il primo anello e' provato: gli altri due token non erano ancora \
             stati visti quando le chiamate sono partite"
        );
    }

    /// Chiamate in piu' NON penalizzano: esplorare e' cio' che un agente fa (il
    /// subset matching di BFCL). Conta che la sottosequenza dipendente ci sia.
    #[test]
    fn le_chiamate_in_piu_non_rompono_la_catena() {
        let traccia = vec![
            call(1, "list_files", json!({ "path": "/tmp" })),      // esplorazione
            call(1, "read_file", json!({ "path": "H-AAA" })),      // anello 1
            call(2, "search_in_files", json!({ "q": "todo" })),    // esplorazione
            call(2, "read_file", json!({ "path": "H-BBB" })),      // anello 2
        ];
        let emessi = vec![emesso(0, "H-AAA"), emesso(1, "H-BBB")];
        assert_eq!(conta_anelli(&traccia, &emessi), 2);
    }

    /// Il token conta ovunque sia finito: dentro un comando, in un campo con un altro
    /// nome, annidato. Il nome del campo e' una convenzione nostra.
    #[test]
    fn il_token_conta_ovunque_il_modello_l_abbia_messo() {
        let traccia = vec![
            call(1, "run_command", json!({ "command": "cat H-AAA | head" })),
            call(2, "custom", json!({ "args": { "nested": ["x", "H-BBB"] } })),
        ];
        let emessi = vec![emesso(0, "H-AAA"), emesso(1, "H-BBB")];
        assert_eq!(conta_anelli(&traccia, &emessi), 2);
    }

    /// RECUPERO: porta il token che viveva solo nell'errore -> l'ha letto.
    #[test]
    fn il_recupero_e_provato_dal_token_dell_errore() {
        let traccia = vec![
            call(1, "read_file", json!({ "path": "H-AAA" })),
            call(2, "read_file", json!({ "epoch": "E-ZZZ" })),
        ];
        assert!(ha_recuperato(&traccia, Some("E-ZZZ"), 1));
        // Senza il token: puo' aver fatto qualunque cosa, ma non ha letto l'errore.
        assert!(!ha_recuperato(&traccia, Some("E-QQQ"), 1));
    }

    /// La prosa non e' un recupero. Se il modello si scusa e ritenta senza il dato,
    /// non ha recuperato — e nessuno deve giudicare il tono delle scuse.
    #[test]
    fn le_scuse_non_sono_un_recupero() {
        let traccia = vec![call(2, "read_file", json!({ "path": "H-AAA", "note": "riprovo, scusa" }))];
        assert!(!ha_recuperato(&traccia, Some("E-ZZZ"), 1));
    }

    /// La ripetizione si riconosce dalla FIRMA canonica: l'ordine delle chiavi non
    /// rende diversa una chiamata identica.
    #[test]
    fn l_ordine_delle_chiavi_non_traveste_una_ripetizione() {
        let a = firma("read_file", &json!({ "path": "x", "mode": "r" }));
        let b = firma("read_file", &json!({ "mode": "r", "path": "x" }));
        assert_eq!(a, b);
        // La chiamata fallita (turno 1) e la sua ripetizione (turno 2), scritte con
        // le chiavi in ordine diverso: e' la stessa chiamata.
        let traccia = vec![
            call(1, "read_file", json!({ "path": "x", "mode": "r" })),
            call(2, "read_file", json!({ "mode": "r", "path": "x" })),
        ];
        assert!(ha_ripetuto_la_fallita(&traccia, &[a]));
        // Chi sbaglia UNA volta e poi cambia azione non ha ripetuto.
        let corretto = vec![
            call(1, "read_file", json!({ "path": "x", "mode": "r" })),
            call(2, "read_file", json!({ "epoch": "E-ZZZ" })),
        ];
        assert!(!ha_ripetuto_la_fallita(&corretto, &[b]));
    }

    /// Argomenti malformati sono un fatto SEPARATO: senza, un modello che non sa
    /// serializzare il JSON collassa in "non ha concatenato" e la diagnosi confonde
    /// il formato con la pianificazione. I modelli piccoli falliscono per formato.
    #[test]
    fn gli_argomenti_malformati_sono_un_fatto_a_parte() {
        let traccia = vec![TracedCall {
            turno: 1,
            nome: "read_file".into(),
            input: json!({}),
            input_malformato: true,
        }];
        let m = misura(&traccia, &[], None, 0, &[]);
        assert!(m.bad_tool_syntax, "va distinto da wrong_plan");
        assert_eq!(m.chained_links, 0);
    }
}
