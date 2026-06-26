//! `turn_focus`: PUNTO UNICO (regola L) della direttiva "focus del turno
//! corrente" (anti-contaminazione della history pregressa).
//!
//! Porting 1:1 di `build_turn_focus_directive`
//! (`brain/agents/nodes/helpers.py:866-927`). Funzione PURA e idempotente: stesso
//! input -> stesso output, nessun side effect, nessuna lettura DB.
//!
//! Causa radice che risolve: con una history grande su un certo task, i modelli
//! (specie gli small) seguono il "peso" del contesto storico invece dell'ultima
//! istruzione. Questa direttiva ancora il turno corrente all'ultima richiesta
//! utente a prescindere dalla similarita' semantica. La useranno SIA il planner
//! SIA l'executor (entrambi la antepongono al system): un solo punto autoritativo
//! qui, i due nodi delegano.
//!
//! Regola G (no hardcode/no lettura DB nella primitiva): il flag che la governa
//! (`agent.context.turn_focus_enabled`, default `true`, letto dal brain in
//! `_load_continuity_config`) NON e' letto qui dentro. La funzione e' pura; il
//! chiamante (planner/executor) decide se invocarla in base al flag e passa il
//! parametro `new_topic`.
//!
//! Regola L (riuso): l'estrazione del testo dai messaggi delega a
//! [`crate::state::MessageContent::flatten_text`] (NON re-implementata) e la
//! rimozione dei blocchi di sistema delega a [`user_text_only`] (gemello Rust di
//! `task_playbook._user_text_only`, qui consolidato come punto unico locale).

use std::sync::LazyLock;

use regex::Regex;

use crate::state::Message;

/// Marcatore di idempotenza dell'iniezione nel system_text. Identico a
/// `_TURN_FOCUS_MARKER` Python (`helpers.py:650`). Esposto perche' i chiamanti
/// (planner/executor) lo useranno per l'iniezione idempotente nel system.
pub const TURN_FOCUS_MARKER: &str = "[[NEXUS_TURN_FOCUS]]";

/// Soglia di troncamento dell'estratto (in CARATTERI Unicode, come il Python che
/// usa `len()`/slice su `str`). Identico a `helpers.py:907`.
const EXCERPT_MAX_CHARS: usize = 600;

// Blocchi di SISTEMA iniettati da mcp-core nel messaggio utente: vanno RIMOSSI
// prima di estrarre la richiesta corrente. Replica di `_SYSTEM_BLOCK_RE`
// (`task_playbook.py:142-145`): `<(allegati|allegati_sessione|task_playbook)...>
// ...</tag>` con DOTALL + IGNORECASE.
//
// Il Python usa il backreference `\1` per far combaciare il tag di chiusura con
// quello di apertura; la crate `regex` di Rust NON supporta i backreference.
// Poiche' i tag possibili sono enumerati (3 alternative del gruppo), l'espansione
// in 3 alternanze tag-fisso (`<allegati...>...</allegati>` | ... | ...) e'
// EQUIVALENTE: ogni alternativa vincola apertura e chiusura allo stesso tag, che
// e' esattamente cio' che il backreference garantiva. `(?is)` = IGNORECASE +
// DOTALL (`.` matcha anche `\n`), `.*?` = non-greedy come il Python.
static SYSTEM_BLOCK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?is)<allegati[^>]*>.*?</allegati>|<allegati_sessione[^>]*>.*?</allegati_sessione>|<task_playbook[^>]*>.*?</task_playbook>",
    )
    .expect("regex system block valida")
});

/// Testo utente PULITO: rimuove i blocchi di sistema (`<allegati>`,
/// `<allegati_sessione>`, `<task_playbook>`) dal contenuto. Gemello Rust di
/// `task_playbook._user_text_only` (punto unico, regola L): se in futuro serve a
/// piu' nodi, e' qui che si estende.
pub fn user_text_only(text: &str) -> String {
    SYSTEM_BLOCK_RE.replace_all(text, "").into_owned()
}

/// Costruisce il blocco "focus del turno corrente" (anti-contaminazione della
/// history). Funzione PURA e idempotente. Porting 1:1 di
/// `build_turn_focus_directive` (`helpers.py:866-927`).
///
/// Cosa estrae: l'ULTIMO `Message::Human` (iterazione dal fondo), ripulito dai
/// blocchi di sistema via [`user_text_only`]. Per i contenuti a blocchi usa
/// [`MessageContent::flatten_text`] (regola L: NON re-implementa l'estrazione);
/// nota di parita': il Python fa `str(content)` sui contenuti non-stringa, forma
/// non riproducibile 1:1 e fuori contratto per l'utente — qui, coerentemente con
/// gli altri nodi del crate (`clarify_or_expand::last_user_message`), si
/// concatenano i blocchi Text.
///
/// Casi limite (identici al Python):
/// - `messages` vuoto -> `None` (Python: `""`).
/// - nessun `Human` valido / testo vuoto dopo `trim` -> `None` (Python: `""`).
/// - richiesta > 600 caratteri -> primi 600 char + `rstrip` + `" [...]"`.
///
/// Ritorna `Option<String>`: `None` mappa la stringa vuota del Python (no-op a
/// monte), `Some(directive)` il blocco pronto da anteporre al system.
///
/// `new_topic`: se `true` (il continuity gate ha rilevato un cambio
/// d'argomento) aggiunge la riga di rinforzo finale.
///
/// Regola G: il flag `turn_focus_enabled` NON e' letto qui (funzione pura); il
/// chiamante decide se invocare e passa `new_topic`.
pub fn build_turn_focus_directive(messages: &[Message], new_topic: bool) -> Option<String> {
    if messages.is_empty() {
        return None;
    }
    // Ultimo messaggio umano, ripulito dai blocchi di sistema (punto unico del
    // parser, regola L), come `apply_continuity_trim`.
    let mut last_user = String::new();
    for m in messages.iter().rev() {
        if let Message::Human { content } = m {
            // `flatten_text()` rende la forma stringa (Text) cosi' com'e' e
            // concatena i blocchi Text (surrogato di `str(content)` Python, vedi
            // doc). Poi rimuoviamo i blocchi di sistema come `_user_text_only`.
            let raw = content.flatten_text();
            last_user = user_text_only(&raw);
            break;
        }
    }
    let last_user = last_user.trim();
    if last_user.is_empty() {
        return None;
    }
    // Estratto compatto: la directive deve restare leggera e cacheabile. Conteggio
    // e slice su CARATTERI Unicode (come Python `len()`/`[:600]` su `str`).
    let excerpt: String = if last_user.chars().count() <= EXCERPT_MAX_CHARS {
        last_user.to_string()
    } else {
        let head: String = last_user.chars().take(EXCERPT_MAX_CHARS).collect();
        format!("{} [...]", head.trim_end())
    };

    let mut lines: Vec<String> = vec![
        "### FOCUS DEL TURNO CORRENTE ###".to_string(),
        "La richiesta da portare a termine ADESSO e' l'ultimo messaggio dell'utente:"
            .to_string(),
        format!("\"{excerpt}\""),
        String::new(),
        "La cronologia precedente e' CONTESTO DI SUPPORTO, non l'oggetto di questa \
richiesta. Se il turno corrente riguarda un task diverso da quello discusso prima, \
segui il turno corrente e NON proseguire il lavoro precedente. Non dare per scontato \
che file, componenti o obiettivi citati nella cronologia siano l'oggetto di QUESTA \
richiesta, a meno che il turno corrente non li nomini esplicitamente."
            .to_string(),
    ];
    if new_topic {
        lines.push(
            "NOTA: rilevato un cambio di argomento rispetto alla cronologia. \
Concentrati esclusivamente sulla richiesta corrente; ignora il lavoro precedente \
salvo quanto serve a soddisfarla."
                .to_string(),
        );
    }
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ContentBlock, MessageContent};
    use serde_json::Value;

    fn human(text: &str) -> Message {
        Message::Human {
            content: MessageContent::text(text),
        }
    }

    fn ai(text: &str) -> Message {
        Message::Ai {
            content: MessageContent::text(text),
            tool_calls: vec![],
        }
    }

    #[test]
    fn vuoto_se_nessun_messaggio() {
        assert_eq!(build_turn_focus_directive(&[], false), None);
    }

    #[test]
    fn vuoto_se_nessun_human() {
        let msgs = vec![ai("ciao")];
        assert_eq!(build_turn_focus_directive(&msgs, false), None);
    }

    #[test]
    fn vuoto_se_solo_blocchi_di_sistema() {
        // Dopo la rimozione dei blocchi di sistema resta solo whitespace -> None.
        let msgs = vec![human("<allegati_sessione>foo.txt</allegati_sessione>")];
        assert_eq!(build_turn_focus_directive(&msgs, false), None);
    }

    #[test]
    fn estrae_ultimo_human() {
        let msgs = vec![
            human("vecchia richiesta su bookingService.ts"),
            ai("ho lavorato sul booking"),
            human("crea index.html"),
        ];
        let out = build_turn_focus_directive(&msgs, false).expect("directive");
        assert!(out.starts_with("### FOCUS DEL TURNO CORRENTE ###"));
        assert!(out.contains("\"crea index.html\""));
        // NON deve agganciare la richiesta vecchia.
        assert!(!out.contains("bookingService"));
    }

    #[test]
    fn rimuove_blocco_allegati_sessione() {
        let msgs = vec![human(
            "<allegati_sessione>\nPL.make\n</allegati_sessione>quante tabelle nel db?",
        )];
        let out = build_turn_focus_directive(&msgs, false).expect("directive");
        assert!(out.contains("\"quante tabelle nel db?\""));
        assert!(!out.contains("PL.make"));
    }

    #[test]
    fn troncamento_oltre_600_char() {
        let lunga = "x".repeat(700);
        let msgs = vec![human(&lunga)];
        let out = build_turn_focus_directive(&msgs, false).expect("directive");
        // 600 char + suffisso " [...]" dentro le virgolette.
        let attesa = format!("\"{} [...]\"", "x".repeat(600));
        assert!(out.contains(&attesa));
    }

    #[test]
    fn new_topic_aggiunge_riga() {
        let msgs = vec![human("nuovo task")];
        let con = build_turn_focus_directive(&msgs, true).expect("directive");
        let senza = build_turn_focus_directive(&msgs, false).expect("directive");
        assert!(con.contains("rilevato un cambio di argomento"));
        assert!(!senza.contains("rilevato un cambio di argomento"));
    }

    #[test]
    fn blocchi_text_concatenati() {
        // Contenuto a blocchi: flatten_text concatena i Text con spazio.
        let msgs = vec![Message::Human {
            content: MessageContent::Blocks(vec![
                ContentBlock::Text {
                    text: "prima parte".to_string(),
                },
                ContentBlock::ToolResult {
                    tool_use_id: "t1".to_string(),
                    content: Value::Null,
                    is_error: false,
                },
                ContentBlock::Text {
                    text: "seconda parte".to_string(),
                },
            ]),
        }];
        let out = build_turn_focus_directive(&msgs, false).expect("directive");
        assert!(out.contains("\"prima parte seconda parte\""));
    }

    #[test]
    fn marker_costante() {
        assert_eq!(TURN_FOCUS_MARKER, "[[NEXUS_TURN_FOCUS]]");
    }
}

#[cfg(test)]
mod golden {
    //! Golden-test di PARITA' 1:1 vs Python per `build_turn_focus_directive`.
    //!
    //! Lo script `scripts/gen_golden_turn_focus.py` importa la funzione REALE dal
    //! brain (`brain.agents.nodes.helpers.build_turn_focus_directive`), la esercita
    //! su N casi rappresentativi e salva `{case_id, messages, new_topic, output}`
    //! in `/tmp/golden_turn_focus.json`. Qui ricostruiamo l'input come `Message`,
    //! chiamiamo la funzione Rust e verifichiamo `output == golden Python`.
    //!
    //! Il test e' `#[ignore]` perche' dipende dal file generato. Comando:
    //!   python3 crates/nexus-agent-graph/scripts/gen_golden_turn_focus.py
    //!   cargo test -p nexus-agent-graph --lib golden_turn_focus_parita -- --ignored
    //!
    //! Nota di parita': i casi golden usano SOLO contenuti `Text` (stringa) sui
    //! messaggi umani, dove la forma Python (`m.content`) e la forma Rust
    //! (`flatten_text`) coincidono byte-per-byte. La forma `str(content)` Python
    //! sui contenuti a blocchi NON e' riproducibile 1:1 ed e' fuori contratto per
    //! l'utente (vedi doc della funzione), quindi NON e' confrontata col golden;
    //! e' coperta dal test unitario `blocchi_text_concatenati`.

    use super::build_turn_focus_directive;
    use crate::state::{Message, MessageContent};
    use serde::Deserialize;

    /// Un messaggio dell'input golden: ruolo + testo (solo `Text`).
    #[derive(Debug, Deserialize)]
    struct GoldenMsg {
        role: String,
        text: String,
    }

    /// Un caso golden: id + history + new_topic + output atteso (`null` == None).
    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        case_id: String,
        messages: Vec<GoldenMsg>,
        #[serde(default)]
        new_topic: bool,
        output: Option<String>,
    }

    fn to_messages(raw: &[GoldenMsg]) -> Vec<Message> {
        raw.iter()
            .map(|m| {
                let content = MessageContent::text(m.text.clone());
                match m.role.as_str() {
                    "user" | "human" => Message::Human { content },
                    "assistant" | "ai" => Message::Ai {
                        content,
                        tool_calls: vec![],
                    },
                    "tool" => Message::Tool {
                        tool_call_id: "golden".to_string(),
                        content,
                    },
                    other => panic!("ruolo golden sconosciuto: {other}"),
                }
            })
            .collect()
    }

    #[test]
    #[ignore = "richiede /tmp/golden_turn_focus.json generato da gen_golden_turn_focus.py"]
    fn golden_turn_focus_parita() {
        let path = "/tmp/golden_turn_focus.json";
        let raw = std::fs::read_to_string(path).unwrap_or_else(|e| {
            panic!(
                "impossibile leggere {path}: {e}; genera con \
                 python3 crates/nexus-agent-graph/scripts/gen_golden_turn_focus.py"
            )
        });
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        assert!(cases.len() >= 15, "attesi >=15 casi golden, trovati {}", cases.len());

        let mut checked = 0usize;
        for c in &cases {
            let msgs = to_messages(&c.messages);
            let got = build_turn_focus_directive(&msgs, c.new_topic);
            assert_eq!(
                got, c.output,
                "PARITA' FALLITA caso {} (new_topic={}):\n  rust   = {:?}\n  python = {:?}",
                c.case_id, c.new_topic, got, c.output
            );
            checked += 1;
        }
        println!("golden turn_focus: {checked} casi verificati, tutti verdi");
    }
}
