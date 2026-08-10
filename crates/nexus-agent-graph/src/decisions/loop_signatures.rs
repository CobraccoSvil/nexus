//! `loop_signatures`: rilevazione PURA del loop di tool call dell'executor e
//! aggiornamento del contatore di esplorazione. Porting 1:1 di
//! `brain/agents/nodes/__init__.py` (`executor_node`, blocchi ~3135-3317),
//! limitato alle parti DETERMINISTICHE: la costruzione della signature, la
//! rilevazione del loop per signature ripetuta, e l'aggiornamento del contatore
//! di chiamate esplorative consecutive.
//!
//! Cosa NON e' qui (per design): l'AUTO-ESCALATION al primo loop
//! (`__init__.py:~3159-3284`) e' I/O (query DB sulla catena di escalation +
//! chiamata LLM di fallback) -> resta nel nodo (PR-H). Qui sta solo la
//! RILEVAZIONE pura: dati le signature recenti + le nuove call del turno,
//! ritorna l'eventuale signature in loop e la coda aggiornata.
//!
//! Tutte le funzioni sono pure (nessun IO, nessuna lettura DB): gli input
//! arrivano come parametri espliciti, cosi' restano deterministiche e
//! golden-validabili in isolamento (regola L: punto unico della rilevazione
//! loop-by-signature, i call site Rust delegheranno qui).

use serde_json::{json, Value};
use sha1::{Digest, Sha1};

use crate::py_json::{py_json_dumps, SortKeys};

/// Numero di occorrenze della STESSA signature (nella finestra recente) oltre il
/// quale il turno e' considerato in loop. `LOOP_THRESHOLD` Python (1:1).
pub const LOOP_THRESHOLD: usize = 3;

/// Numero massimo di signature mantenute nello stato per la rilevazione loop.
/// `updated_signatures = (recent + new)[-12:]` Python (1:1).
pub const RECENT_SIGNATURES_CAP: usize = 12;

/// Soglie DB-driven (regola G) della rilevazione loop-by-signature. I default
/// coincidono con le costanti Python 1:1 [`LOOP_THRESHOLD`] /
/// [`RECENT_SIGNATURES_CAP`] e valgono SOLO se il DB non fornisce i setting
/// (`agent.loop.signature_threshold` / `agent.loop.recent_signatures_cap`) —
/// mai come magic fallback nella logica. Passate come parametro alle varianti
/// `_with` cosi' il modulo resta puro e golden-validabile a qualunque soglia.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopThresholds {
    /// Occorrenze della STESSA signature (nella finestra recente) oltre cui il
    /// turno e' in loop. Default [`LOOP_THRESHOLD`]. La finestra di conteggio e'
    /// `signature * 2` (coerente col Python `combined[-LOOP_THRESHOLD*2:]`).
    pub signature: usize,
    /// Cap della coda di signature mantenute nello stato. Default
    /// [`RECENT_SIGNATURES_CAP`].
    pub cap: usize,
}

impl Default for LoopThresholds {
    fn default() -> Self {
        Self {
            signature: LOOP_THRESHOLD,
            cap: RECENT_SIGNATURES_CAP,
        }
    }
}

/// Costruisce la signature di una tool call: `f"{name}|{sha1_hex12}"` dove
/// `sha1_hex12` sono i primi 12 char esadecimali dello SHA-1 di
/// `json.dumps(input or {}, sort_keys=True, ensure_ascii=False)`.
///
/// Bit-identica al Python (`__init__.py:3140-3141`):
/// ```python
/// sig_input = json.dumps(tu.get("input") or {}, sort_keys=True, ensure_ascii=False)
/// sig = f"{tu.get('name', '')}|{hashlib.sha1(sig_input.encode()).hexdigest()[:12]}"
/// ```
/// La canonicalizzazione dell'input usa il PUNTO UNICO [`py_json_dumps`] con
/// [`SortKeys::Yes`] (chiavi ordinate alfabeticamente in modo ricorsivo,
/// separatori `", "`/`": "`, unicode letterale): senza questa serializzazione
/// dedicata `serde_json::to_string` produrrebbe separatori compatti e, con
/// `preserve_order` attivo nel workspace, ordine d'inserimento -> hash diverso.
///
/// `input` falsy (None / oggetto vuoto in Python) collassa su `{}`: qui
/// [`Value::Null`] (input assente) viene trattato come `{}` per replicare
/// `tu.get("input") or {}` (in Python `None`, `{}`, `[]`, `""`, `0`, `False`
/// sono falsy; nel contratto delle tool call l'input e' SEMPRE un oggetto o
/// assente, quindi normalizziamo il solo caso `Null` -> `{}`).
pub fn build_signature(name: &str, input: &Value) -> String {
    let canonical_input = if input.is_null() {
        py_json_dumps(&json!({}), SortKeys::Yes)
    } else {
        py_json_dumps(input, SortKeys::Yes)
    };
    let mut hasher = Sha1::new();
    hasher.update(canonical_input.as_bytes());
    let digest = hasher.finalize();
    // hexdigest()[:12]: 6 byte -> 12 char esadecimali (lowercase, come Python).
    let mut hex12 = String::with_capacity(12);
    for byte in digest.iter().take(6) {
        hex12.push_str(&format!("{byte:02x}"));
    }
    format!("{name}{SEP_INPUT}{hex12}")
}

/// Separatore fra nome del tool e hash nella firma dell'INPUT
/// ([`build_signature`]).
const SEP_INPUT: char = '|';

/// Separatore fra nome del tool e hash nella firma dell'ESITO
/// ([`firma_esito_ricerca`]). DISTINTO da [`SEP_INPUT`] perche' le due firme
/// rispondono a domande diverse e non devono mai confondersi in un confronto.
const SEP_ESITO: char = '@';

/// Il NOME DEL TOOL letto da una firma, qualunque sia la domanda a cui quella
/// firma risponde (input o esito).
///
/// Punto unico (regola L): il nome si legge in due posti — lo sconto
/// post-progresso di [`detect_signature_loop_progress_aware_with`], che deve
/// sapere se il tool e' read-only, e il messaggio di chiusura dell'executor, che
/// lo dice all'utente. Finche' il separatore era uno solo, `split_once('|')` a
/// mano bastava; con due, una firma d'esito letta da chi conosce il solo `|`
/// ritorna la stringa intera come "nome del tool" — cioe' non e' in nessuna
/// lista di tool e viene classificata produttiva, che e' l'esatto contrario.
pub fn nome_tool_da_firma(firma: &str) -> &str {
    firma
        .split_once(SEP_INPUT)
        .or_else(|| firma.split_once(SEP_ESITO))
        .map(|(nome, _)| nome)
        .unwrap_or(firma)
}

/// Natura della ripetizione rilevata: che cosa, esattamente, si sta ripetendo.
///
/// Sono due fatti diversi con due rimedi diversi, e il consumatore (il messaggio
/// all'utente, il payload della fase) deve poterli distinguere senza leggere il
/// formato della firma (regola Q: l'esito sta in un campo).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NaturaStallo {
    /// Stessa tool call, stesso input, senza progresso in mezzo.
    StessoInput,
    /// Chiamate DIVERSE che hanno restituito lo stesso identico insieme di
    /// risultati: il modello varia la domanda e riceve sempre la stessa risposta.
    StessoEsito,
}

impl NaturaStallo {
    /// Chiave macchina per il payload strutturato della fase emessa (regola M:
    /// chi legge quell'evento decide sul campo, non sulla frase).
    pub fn chiave(self) -> &'static str {
        match self {
            Self::StessoInput => "signature_loop",
            Self::StessoEsito => "same_result_loop",
        }
    }
}

/// La firma dell'ESITO di una ricerca: l'insieme ORDINATO dei risultati che ha
/// restituito, non il testo con cui li si e' chiesti.
///
/// PERCHE' ESISTE. [`build_signature`] firma `nome | hash(input)`, cioe' cio'
/// che si CHIEDE. Un agente che varia la query di una parola produce una firma
/// nuova a ogni giro, e per il rilevatore non sta ripetendo: sta esplorando.
///
/// MISURATO il 10/08/2026 su `prova-fix-10-08`, per una domanda che si risolve
/// leggendo un file di 183 righe: 17 chiamate a `nexus_search_semantic`, 16
/// riuscite, TUTTE con gli stessi identici quattro hit da `index.html`. Le
/// query erano `card prodotto`, `card prodotto product card`, `product card
/// component card container product-card`, `card prodotto classe CSS`... —
/// diciassette firme diverse per un unico risultato. 852K token e $0.11 spesi
/// dopo che la prima chiamata aveva gia' la risposta.
///
/// Nessuno degli altri presidi poteva vederlo: `repeated_action_failed` guarda
/// le ripetizioni che FALLISCONO (qui riuscivano tutte), `correction_progress`
/// misura le SCRITTURE (qui non se ne fa nessuna), e
/// `avanzamento_figura` usa questa stessa `build_signature`, quindi eredita la
/// cecita' — ogni query nuova gli conta come strada mai percorsa, cioe' come
/// avanzamento.
///
/// NON e' la regola M al contrario: l'esito e' gia' strutturato (`hits` con
/// `source_id` e `chunk_index`), quindi la firma nasce da CAMPI e non dal testo
/// della risposta. Dove quei campi non ci sono la firma non nasce: `None` dice
/// «di questo esito non so dire nulla», che non e' «esito diverso».
///
/// ORDINATA e DEDUPLICATA perche' la domanda e' «ho gia' visto questo INSIEME
/// di risultati?»: due ricerche che tornano gli stessi chunk in ordine di score
/// diverso hanno trovato la stessa cosa, e per chi decide se continuare sono la
/// stessa risposta.
pub fn firma_esito_ricerca(tool_name: &str, tool_result: &Value) -> Option<String> {
    let hits = tool_result.get("hits")?.as_array()?;
    // Zero risultati NON produce firma: «non ho trovato niente» ripetuto puo'
    // essere esplorazione legittima di query diverse, ed e' gia' coperto dal
    // rilevatore sull'input. Qui si guarda cio' che si e' TROVATO.
    if hits.is_empty() {
        return None;
    }
    let mut chiavi: Vec<String> = hits
        .iter()
        .filter_map(|h| {
            let fonte = h.get("source_id").and_then(Value::as_str)?;
            let chunk = h
                .get("chunk_index")
                .and_then(Value::as_i64)
                .map(|i| i.to_string())
                .unwrap_or_default();
            Some(format!("{fonte}#{chunk}"))
        })
        .collect();
    // Un array di hit privo dei campi attesi non e' un insieme vuoto: e' una
    // forma che non conosciamo, e su cui non si decide.
    if chiavi.is_empty() {
        return None;
    }
    chiavi.sort();
    chiavi.dedup();
    let mut hasher = Sha1::new();
    hasher.update(chiavi.join("\u{1}").as_bytes());
    let digest = hasher.finalize();
    let mut hex12 = String::with_capacity(12);
    for byte in digest.iter().take(6) {
        hex12.push_str(&format!("{byte:02x}"));
    }
    Some(format!("{tool_name}{SEP_ESITO}{hex12}"))
}

/// Data la SERIE cronologica delle firme d'esito osservate per un tool, dice se
/// le ultime `soglia` sono la stessa: N risposte identiche di fila a domande
/// diverse.
///
/// PERCHE' QUI E NON NEL RACCOGLITORE. La serie la costruisce chi legge la
/// history (un attraversamento con I/O sui messaggi); il CRITERIO su cosa quella
/// serie significhi sta accanto al vocabolario che la produce, cosi' chi cambia
/// la forma della firma vede subito la regola che la consuma.
///
/// La domanda e' sulle ultime `soglia` occorrenze CONSECUTIVE, non su un
/// conteggio nella finestra: due esiti identici separati da un terzo diverso
/// sono un'esplorazione che ha trovato qualcosa in mezzo, non uno stallo. Chi
/// costruisce la serie ha gia' interrotto la sequenza dove c'e' stato lavoro
/// (una scrittura rende legittima la ri-verifica dello stesso esito).
///
/// `soglia` 0 o 1 non e' una domanda sensata — un solo esito non e' una
/// ripetizione — e ritorna `None`.
pub fn serie_in_stallo(serie: &[String], soglia: usize) -> Option<String> {
    if soglia < 2 || serie.len() < soglia {
        return None;
    }
    let coda = &serie[serie.len() - soglia..];
    let prima = coda.first()?;
    coda.iter().all(|f| f == prima).then(|| prima.clone())
}

/// Esito della rilevazione loop: la signature eventualmente in loop e la coda
/// aggiornata delle signature (cap [`RECENT_SIGNATURES_CAP`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopDetection {
    /// `Some(sig)` se una signature appare >= [`LOOP_THRESHOLD`] volte nella
    /// finestra recente; `None` altrimenti. Replica `loop_sig` Python.
    pub loop_signature: Option<String>,
    /// `(recent + new)[-12:]`: la coda da persistere nello stato. Replica
    /// `updated_signatures` Python.
    pub updated_signatures: Vec<String>,
}

/// Rileva il loop per signature ripetuta e calcola la coda aggiornata.
///
/// Porting 1:1 di `__init__.py:3143-3287` (solo la parte di RILEVAZIONE, senza
/// l'auto-escalation I/O):
/// ```python
/// recent = list(state.get("recent_tool_signatures") or [])
/// combined = recent + new_signatures
/// loop_sig = None
/// if len(combined) >= LOOP_THRESHOLD and new_signatures:
///     for sig in new_signatures:
///         tail = [s for s in combined[-LOOP_THRESHOLD * 2:] if s == sig]
///         if len(tail) >= LOOP_THRESHOLD:
///             loop_sig = sig
///             break
/// updated_signatures = (recent + new_signatures)[-12:]
/// ```
/// La finestra di conteggio e' `combined[-(LOOP_THRESHOLD*2):]` (le ultime 6
/// signature). Ritorna la PRIMA `new_signature` in loop (ordine di emissione).
pub fn detect_signature_loop(recent: &[String], new_signatures: &[String]) -> LoopDetection {
    detect_signature_loop_with(recent, new_signatures, LoopThresholds::default())
}

/// Variante parametrica di [`detect_signature_loop`] con soglie DB-driven
/// ([`LoopThresholds`], regola G). `detect_signature_loop` la chiama coi default
/// (= costanti Python 1:1), preservando la parita' bit-per-bit.
pub fn detect_signature_loop_with(
    recent: &[String],
    new_signatures: &[String],
    thresholds: LoopThresholds,
) -> LoopDetection {
    // combined = recent + new_signatures
    let mut combined: Vec<String> = Vec::with_capacity(recent.len() + new_signatures.len());
    combined.extend_from_slice(recent);
    combined.extend_from_slice(new_signatures);

    let mut loop_signature: Option<String> = None;
    if combined.len() >= thresholds.signature && !new_signatures.is_empty() {
        // tail = combined[-(signature*2):]
        let window_len = thresholds.signature * 2;
        let start = combined.len().saturating_sub(window_len);
        let window = &combined[start..];
        for sig in new_signatures {
            let count = window.iter().filter(|s| *s == sig).count();
            if count >= thresholds.signature {
                loop_signature = Some(sig.clone());
                break;
            }
        }
    }

    // updated_signatures = (recent + new_signatures)[-cap:]
    let cap_start = combined.len().saturating_sub(thresholds.cap);
    let updated_signatures = combined[cap_start..].to_vec();

    LoopDetection {
        loop_signature,
        updated_signatures,
    }
}

/// Variante PROGRESS-AWARE di [`detect_signature_loop`] (punto unico, regola L —
/// stessa esclusione "rilettura-dopo-progresso" del detector `repeated_action`):
/// per una firma di tool READ-ONLY contano SOLO le occorrenze successive
/// all'ULTIMA azione PRODUTTIVA nella finestra. Il pattern di debugging
/// "leggi -> correggi -> builda (fallisce) -> rileggi l'errore" produce la
/// stessa firma di lettura 3 volte con edit/build in mezzo: NON e' uno stallo
/// (incidente run b833a83d: gemini-2.5-pro ucciso dal loop-detector mentre
/// stava convergendo su una build rossa). Le firme PRODUTTIVE (edit/comando
/// identico ripetuto) contano sempre: ripeterle a vuoto E' il loop.
///
/// `is_read_only(name)` e' il predicato del chiamante (l'executor passa
/// `EXPLORATION_ONLY_TOOLS`, fonte unica della classificazione read-only):
/// il modulo resta puro e senza dipendenze dal routing.
pub fn detect_signature_loop_progress_aware(
    recent: &[String],
    new_signatures: &[String],
    is_read_only: impl Fn(&str) -> bool,
) -> LoopDetection {
    detect_signature_loop_progress_aware_with(
        recent,
        new_signatures,
        is_read_only,
        LoopThresholds::default(),
    )
}

/// Variante parametrica di [`detect_signature_loop_progress_aware`] con soglie
/// DB-driven ([`LoopThresholds`], regola G). Il wrapper senza soglie usa i
/// default (= costanti Python 1:1), preservando la parita'.
pub fn detect_signature_loop_progress_aware_with(
    recent: &[String],
    new_signatures: &[String],
    is_read_only: impl Fn(&str) -> bool,
    thresholds: LoopThresholds,
) -> LoopDetection {
    let mut combined: Vec<String> = Vec::with_capacity(recent.len() + new_signatures.len());
    combined.extend_from_slice(recent);
    combined.extend_from_slice(new_signatures);

    let mut loop_signature: Option<String> = None;
    if combined.len() >= thresholds.signature && !new_signatures.is_empty() {
        let window_len = thresholds.signature * 2;
        let start = combined.len().saturating_sub(window_len);
        let window = &combined[start..];
        // Posizione dell'ULTIMA firma produttiva nella finestra: le occorrenze
        // read-only PRECEDENTI sono "scontate" dal progresso.
        let last_productive = window
            .iter()
            .rposition(|s| !is_read_only(nome_tool_da_firma(s)));
        for sig in new_signatures {
            let read_only = is_read_only(nome_tool_da_firma(sig));
            let count = if read_only {
                let from = last_productive.map(|p| p + 1).unwrap_or(0);
                window[from..].iter().filter(|s| *s == sig).count()
            } else {
                window.iter().filter(|s| *s == sig).count()
            };
            if count >= thresholds.signature {
                loop_signature = Some(sig.clone());
                break;
            }
        }
    }

    let cap_start = combined.len().saturating_sub(thresholds.cap);
    let updated_signatures = combined[cap_start..].to_vec();

    LoopDetection {
        loop_signature,
        updated_signatures,
    }
}

/// Stato del contatore di esplorazione consecutiva, prima/dopo l'aggiornamento.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplorationCounterUpdate {
    /// Nuovo valore di `consecutive_exploration_calls`.
    pub consecutive_exploration_calls: i64,
    /// Nuovo valore del flag `exploration_nudge_sent`.
    pub exploration_nudge_sent: bool,
    /// `true` se l'asse "exploration" va RIMOSSO dagli assi gia' guidati del
    /// progress_controller (reset coordinato): accade quando il turno emette
    /// almeno una call PRODUTTIVA. Replica `_progress_guided.discard("exploration")`.
    pub reset_exploration_axis: bool,
}

/// Aggiorna il contatore di chiamate esplorative consecutive in base alle tool
/// call PENDING del turno corrente.
///
/// Porting 1:1 di `__init__.py:3296-3317` (la parte di aggiornamento contatore;
/// il reset `discard("g1_descriptive")` su `pending_tool_uses` non vuoto e'
/// concern del progress_controller, non di questo punto unico):
/// ```python
/// if pending_tool_uses:
///     _pending_names = [str(tu.get("name", "")) for tu in pending_tool_uses]
///     _all_exploration = all(n in _EXPLORATION_ONLY_TOOLS for n in _pending_names)
///     if _all_exploration:
///         _updated_exploration_count += len(_pending_names)
///     else:
///         _updated_exploration_count = 0
///         _updated_exploration_nudge_sent = False
///         _progress_guided.discard("exploration")
/// ```
/// Senza tool call (turno testuale) il contatore e il flag restano INVARIATI.
/// `exploration_only_tools` e' passato come parametro (e' la lista autoritativa
/// gia' presente in `routing::signals`, regola L): cosi' la funzione resta pura.
pub fn exploration_counter_update(
    pending_tool_names: &[String],
    current_count: i64,
    current_nudge_sent: bool,
    exploration_only_tools: &[&str],
) -> ExplorationCounterUpdate {
    // Turno testuale (nessuna tool call): stato invariato.
    if pending_tool_names.is_empty() {
        return ExplorationCounterUpdate {
            consecutive_exploration_calls: current_count,
            exploration_nudge_sent: current_nudge_sent,
            reset_exploration_axis: false,
        };
    }
    let all_exploration = pending_tool_names
        .iter()
        .all(|n| exploration_only_tools.contains(&n.as_str()));
    if all_exploration {
        // Tutte esplorative: accumula (il modello sta ancora leggendo).
        ExplorationCounterUpdate {
            consecutive_exploration_calls: current_count + pending_tool_names.len() as i64,
            exploration_nudge_sent: current_nudge_sent,
            reset_exploration_axis: false,
        }
    } else {
        // Almeno una produttiva: reset coordinato.
        ExplorationCounterUpdate {
            consecutive_exploration_calls: 0,
            exploration_nudge_sent: false,
            reset_exploration_axis: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::signals::EXPLORATION_ONLY_TOOLS;

    #[test]
    fn signature_deterministica_e_ordine_chiavi_irrilevante() {
        // L'ordine d'inserimento delle chiavi NON deve cambiare la signature
        // (sort_keys=True le ordina alfabeticamente prima dell'hash).
        let s1 = build_signature("read_file", &json!({"path": "a.rs", "offset": 1}));
        let s2 = build_signature("read_file", &json!({"offset": 1, "path": "a.rs"}));
        assert_eq!(s1, s2);
        // Prefisso name|, poi 12 char esadecimali.
        let (name, hex) = s1.split_once('|').unwrap();
        assert_eq!(name, "read_file");
        assert_eq!(hex.len(), 12);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn signature_input_null_come_oggetto_vuoto() {
        // tu.get("input") or {} -> Null collassa su {}.
        let a = build_signature("t", &Value::Null);
        let b = build_signature("t", &json!({}));
        assert_eq!(a, b);
    }

    /// Esito di una ricerca nella forma che il tool produce davvero: il payload
    /// di `mcp-core::agent_tools::rag_search::payload_ricerca` (`query`, `count`,
    /// `hits` con `source_id`/`chunk_index`). Il ponte col produttore VERO e' un
    /// test in mcp-core (che dipende da questo crate, non viceversa): qui la
    /// forma e' riprodotta, li' e' quella emessa dal tool.
    fn esito_ricerca(hits: &[(&str, i64)]) -> Value {
        let h: Vec<Value> = hits
            .iter()
            .map(|(fonte, chunk)| json!({"source_id": fonte, "chunk_index": chunk, "text": "..."}))
            .collect();
        json!({"query": "qualunque", "count": h.len(), "hits": h})
    }

    #[test]
    fn firma_esito_ignora_la_query_e_l_ordine_dei_hit() {
        // IL FATTO MISURATO (run su prova-fix-10-08): query diverse, stessi hit.
        // Le due firme d'INPUT differiscono, quella d'ESITO no.
        let a = firma_esito_ricerca(
            "nexus_search_semantic",
            &esito_ricerca(&[("index.html", 0), ("index.html", 2)]),
        );
        // Stessi hit in ordine di score diverso: hanno trovato la stessa cosa.
        let b = firma_esito_ricerca(
            "nexus_search_semantic",
            &esito_ricerca(&[("index.html", 2), ("index.html", 0)]),
        );
        assert_eq!(a, b);
        assert!(a.is_some());
        assert_ne!(
            build_signature("nexus_search_semantic", &json!({"query": "card prodotto"})),
            build_signature(
                "nexus_search_semantic",
                &json!({"query": "card prodotto product card"})
            ),
            "le firme d'input DEVONO restare diverse: e' il difetto che l'esito copre"
        );
    }

    #[test]
    fn firma_esito_distingue_insiemi_diversi_e_tool_diversi() {
        let uno = firma_esito_ricerca("t", &esito_ricerca(&[("a.md", 0)]));
        let due = firma_esito_ricerca("t", &esito_ricerca(&[("a.md", 1)]));
        assert_ne!(uno, due, "chunk diverso = risultato diverso");
        let altro_tool = firma_esito_ricerca("u", &esito_ricerca(&[("a.md", 0)]));
        assert_ne!(uno, altro_tool, "il nome del tool entra nella firma");
    }

    #[test]
    fn firma_esito_assente_dove_non_c_e_nulla_da_dire() {
        // Nessun campo `hits`: non e' una ricerca -> il criterio non si applica.
        assert_eq!(
            firma_esito_ricerca("read_file", &json!({"content": "..."})),
            None
        );
        // Zero risultati: variare la query e' esplorazione legittima, gia'
        // coperta dal rilevatore sull'input.
        assert_eq!(firma_esito_ricerca("t", &esito_ricerca(&[])), None);
        // Hit in una forma che non conosciamo: l'ignoto non e' "esito diverso".
        assert_eq!(
            firma_esito_ricerca("t", &json!({"hits": [{"altro": 1}]})),
            None
        );
    }

    #[test]
    fn nome_tool_letto_da_entrambe_le_firme() {
        let input = build_signature("nexus_search_semantic", &json!({"query": "x"}));
        let esito = firma_esito_ricerca("nexus_search_semantic", &esito_ricerca(&[("a", 0)]))
            .expect("firma d'esito");
        assert_eq!(nome_tool_da_firma(&input), "nexus_search_semantic");
        // REGRESSIONE: con `split_once('|')` questa ritornava la stringa intera,
        // che nessuna lista di tool contiene -> firma classificata produttiva.
        assert_eq!(nome_tool_da_firma(&esito), "nexus_search_semantic");
        // Stringa senza separatore: e' gia' il nome.
        assert_eq!(nome_tool_da_firma("read_file"), "read_file");
    }

    #[test]
    fn serie_in_stallo_solo_su_occorrenze_consecutive() {
        let a = "t@aaa".to_string();
        let b = "t@bbb".to_string();
        assert_eq!(
            serie_in_stallo(&[a.clone(), a.clone(), a.clone()], 3),
            Some(a.clone())
        );
        // Due identiche e una diversa in mezzo: ha trovato altro, non e' stallo.
        assert_eq!(serie_in_stallo(&[a.clone(), b.clone(), a.clone()], 3), None);
        // Sotto soglia.
        assert_eq!(serie_in_stallo(&[a.clone(), a.clone()], 3), None);
        // Conta la CODA: le vecchie diverse non salvano dalla ripetizione in atto.
        assert_eq!(
            serie_in_stallo(&[b, a.clone(), a.clone(), a.clone()], 3),
            Some(a)
        );
        // Un solo esito non e' una ripetizione, a nessuna soglia.
        assert_eq!(serie_in_stallo(&["t@x".to_string()], 1), None);
    }

    #[test]
    fn loop_rilevato_a_tre_occorrenze() {
        let sig = build_signature("read_file", &json!({"path": "x"}));
        // recent ha gia' 2 occorrenze, la nuova call e' la terza -> loop.
        let recent = vec![sig.clone(), "altro|abc".into(), sig.clone()];
        let new = vec![sig.clone()];
        let out = detect_signature_loop(&recent, &new);
        assert_eq!(out.loop_signature, Some(sig));
        // Due occorrenze -> nessun loop.
        let recent2 = vec!["a|1".into()];
        let new2 = vec!["a|1".into()];
        let out2 = detect_signature_loop(&recent2, &new2);
        assert_eq!(out2.loop_signature, None);
    }

    fn read_only(name: &str) -> bool {
        EXPLORATION_ONLY_TOOLS.contains(&name)
    }

    #[test]
    fn progress_aware_rilettura_debugging_non_e_loop() {
        // REGRESSIONE run b833a83d: leggi -> correggi -> builda (fallisce) ->
        // rileggi l'errore -> rileggi. Le letture PRECEDENTI l'azione produttiva
        // sono scontate dal progresso: 2 occorrenze dopo l'edit/build -> NO loop.
        let read = build_signature("read_file", &json!({"path": "bookingService.ts"}));
        let edit = build_signature("edit_file", &json!({"path": "bookingService.ts"}));
        let build = build_signature("run_command", &json!({"command": "pnpm build"}));
        let recent = vec![read.clone(), edit, build, read.clone()];
        let out =
            detect_signature_loop_progress_aware(&recent, std::slice::from_ref(&read), read_only);
        assert_eq!(
            out.loop_signature, None,
            "rilettura post-progresso non e' stallo"
        );
        // La coda aggiornata resta completa (il filtro e' solo sul conteggio).
        assert_eq!(out.updated_signatures.len(), 5);
    }

    #[test]
    fn progress_aware_loop_vero_scatta() {
        // Tre letture identiche SENZA alcuna azione produttiva in mezzo: stallo.
        let read = build_signature("read_file", &json!({"path": "x"}));
        let recent = vec![read.clone(), "list_files|abc".into(), read.clone()];
        let out =
            detect_signature_loop_progress_aware(&recent, std::slice::from_ref(&read), read_only);
        assert_eq!(out.loop_signature, Some(read));
    }

    #[test]
    fn progress_aware_polling_servizio_tra_build_non_e_loop() {
        // REGRESSIONE run 2c41b145: build fallita -> leggi i log del dev server
        // -> correggi -> ri-builda -> rileggi i log. read_service_output e' un
        // tool di POLLING (stesso input, output che evolve): classificato
        // read-only, le letture scontate dal progresso non contano.
        let poll = build_signature("read_service_output", &json!({"process_id": "svc-1"}));
        let edit = build_signature("edit_file", &json!({"path": "x.ts"}));
        let build = build_signature("run_command", &json!({"command": "pnpm build"}));
        // Finestra reale del run: build, poll, edit, build, poll + nuovo poll.
        let recent = vec![build.clone(), poll.clone(), edit, build, poll.clone()];
        let out =
            detect_signature_loop_progress_aware(&recent, std::slice::from_ref(&poll), read_only);
        assert_eq!(
            out.loop_signature, None,
            "polling tra build/edit e' monitoraggio, non stallo"
        );
    }

    #[test]
    fn progress_aware_produttiva_ripetuta_scatta_sempre() {
        // Un edit/comando IDENTICO ripetuto 3 volte e' loop anche se in mezzo
        // ci sono altre produttive: il filtro vale solo per i read-only.
        let edit = build_signature("edit_file", &json!({"path": "x", "old_string": "a"}));
        let other = build_signature("run_command", &json!({"command": "ls"}));
        let recent = vec![edit.clone(), other, edit.clone()];
        let out =
            detect_signature_loop_progress_aware(&recent, std::slice::from_ref(&edit), read_only);
        assert_eq!(out.loop_signature, Some(edit));
    }

    #[test]
    fn loop_solo_se_nuove_signature() {
        // combined >= 3 ma new_signatures vuoto -> nessun loop.
        let recent = vec!["a|1".into(), "a|1".into(), "a|1".into()];
        let out = detect_signature_loop(&recent, &[]);
        assert_eq!(out.loop_signature, None);
        // La coda resta comunque calcolata.
        assert_eq!(out.updated_signatures.len(), 3);
    }

    #[test]
    fn coda_cap_dodici() {
        let recent: Vec<String> = (0..15).map(|i| format!("s|{i}")).collect();
        let new = vec!["s|new".to_string()];
        let out = detect_signature_loop(&recent, &new);
        assert_eq!(out.updated_signatures.len(), RECENT_SIGNATURES_CAP);
        // Tiene le ULTIME 12 (recent[4..] + new).
        assert_eq!(out.updated_signatures.last().unwrap(), "s|new");
    }

    #[test]
    fn soglie_db_driven_non_default() {
        // Proprieta' Rust (NON parita' Python: il riferimento Python usa la
        // costante fissa 3/12). Con soglia signature=2 il loop scatta a 2
        // occorrenze; con cap=4 la coda e' limitata a 4.
        let th = LoopThresholds {
            signature: 2,
            cap: 4,
        };
        let sig = build_signature("read_file", &json!({"path": "x"}));
        // Due occorrenze: con default (3) NON e' loop; con signature=2 lo e'.
        let recent = vec![sig.clone()];
        let new = vec![sig.clone()];
        assert_eq!(detect_signature_loop(&recent, &new).loop_signature, None);
        assert_eq!(
            detect_signature_loop_with(&recent, &new, th).loop_signature,
            Some(sig.clone())
        );
        // Cap coda = 4.
        let long: Vec<String> = (0..10).map(|i| format!("s|{i}")).collect();
        let out = detect_signature_loop_with(&long, std::slice::from_ref(&sig), th);
        assert_eq!(out.updated_signatures.len(), 4);
    }

    #[test]
    fn exploration_accumula_se_tutte_esplorative() {
        let names = vec!["read_file".to_string(), "grep".to_string()];
        let out = exploration_counter_update(&names, 3, false, EXPLORATION_ONLY_TOOLS);
        assert_eq!(out.consecutive_exploration_calls, 5); // 3 + 2
        assert!(!out.exploration_nudge_sent);
        assert!(!out.reset_exploration_axis);
    }

    #[test]
    fn exploration_reset_se_una_produttiva() {
        let names = vec!["read_file".to_string(), "write_file".to_string()];
        let out = exploration_counter_update(&names, 5, true, EXPLORATION_ONLY_TOOLS);
        assert_eq!(out.consecutive_exploration_calls, 0);
        assert!(!out.exploration_nudge_sent);
        assert!(out.reset_exploration_axis);
    }

    #[test]
    fn exploration_invariato_se_turno_testuale() {
        let out = exploration_counter_update(&[], 4, true, EXPLORATION_ONLY_TOOLS);
        assert_eq!(out.consecutive_exploration_calls, 4);
        assert!(out.exploration_nudge_sent);
        assert!(!out.reset_exploration_axis);
    }
}

/// Golden di parita' 1:1 vs Python per signature/exploration. Carica
/// `/tmp/golden_executor_signals.json` (vedi `gen_golden_executor_signals.py`).
#[cfg(test)]
mod golden {
    use super::*;
    use crate::routing::signals::EXPLORATION_ONLY_TOOLS;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        group: String,
        case_id: String,
        input: Value,
        output: Value,
    }

    #[derive(Debug, Deserialize)]
    struct SignatureInput {
        name: String,
        tool_input: Value,
    }

    #[derive(Debug, Deserialize)]
    struct LoopInput {
        recent: Vec<String>,
        new_signatures: Vec<String>,
    }

    #[derive(Debug, Deserialize)]
    struct ExplorationInput {
        pending_tool_names: Vec<String>,
        current_count: i64,
        current_nudge_sent: bool,
    }

    #[test]
    #[ignore = "richiede /tmp/golden_executor_signals.json generato da gen_golden_executor_signals.py"]
    fn golden_executor_signals() {
        let Some(raw) = crate::golden_util::load_golden(
            "golden_executor_signals.json",
            "gen_golden_executor_signals.py",
        ) else {
            return;
        };
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        assert!(
            cases.len() >= 20,
            "attesi >= 20 casi, trovati {}",
            cases.len()
        );

        let mut checked = 0usize;
        for c in &cases {
            let got: Value = match c.group.as_str() {
                "build_signature" => {
                    let i: SignatureInput =
                        serde_json::from_value(c.input.clone()).expect("SignatureInput");
                    Value::String(build_signature(&i.name, &i.tool_input))
                }
                "detect_signature_loop" => {
                    let i: LoopInput = serde_json::from_value(c.input.clone()).expect("LoopInput");
                    let out = detect_signature_loop(&i.recent, &i.new_signatures);
                    json!({
                        "loop_signature": out.loop_signature,
                        "updated_signatures": out.updated_signatures,
                    })
                }
                "exploration_counter_update" => {
                    let i: ExplorationInput =
                        serde_json::from_value(c.input.clone()).expect("ExplorationInput");
                    let out = exploration_counter_update(
                        &i.pending_tool_names,
                        i.current_count,
                        i.current_nudge_sent,
                        EXPLORATION_ONLY_TOOLS,
                    );
                    json!({
                        "consecutive_exploration_calls": out.consecutive_exploration_calls,
                        "exploration_nudge_sent": out.exploration_nudge_sent,
                        "reset_exploration_axis": out.reset_exploration_axis,
                    })
                }
                other => panic!("gruppo golden sconosciuto: {other} (caso {})", c.case_id),
            };
            assert_eq!(
                got, c.output,
                "PARITA' FALLITA {} / {}:\n  rust   = {}\n  python = {}",
                c.group, c.case_id, got, c.output
            );
            checked += 1;
        }
        println!("golden executor_signals: {checked} casi verificati, tutti verdi");
    }
}
