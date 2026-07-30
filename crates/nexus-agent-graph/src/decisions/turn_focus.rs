//! `turn_focus`: PUNTO UNICO (regola L) della direttiva "focus del turno
//! corrente" (anti-contaminazione della history pregressa).
//!
//! Causa radice che risolve: con una history grande su un certo task, i modelli
//! (specie gli small) seguono il "peso" del contesto storico invece dell'ultima
//! istruzione. Questa direttiva ancora il turno corrente alla richiesta
//! dell'utente a prescindere dalla similarita' semantica. La useranno SIA il
//! planner SIA l'executor: un solo punto autoritativo qui, i due nodi delegano.
//!
//! ## Da dove viene la richiesta
//!
//! Da [`crate::decisions::turn_task::current_turn_task`], cioe' da dove il run
//! l'ha FISSATA all'origine, e non piu' dall'ultimo `Message::Human` della
//! cronologia.
//!
//! L'euristica precedente era falsa gia' dal secondo turno di un run agentico:
//! `tool_dispatch` consegna i risultati dei tool come `Message::Human` a blocchi
//! e vi appende i promemoria della barriera advisory e dei todo come blocchi
//! `{"type":"text","text":"<system-reminder>...</system-reminder>"}`; l'executor
//! inietta i nudge anti-stallo come `Message::Human` di testo; il resume HITL ne
//! aggiunge uno di conferma. `flatten_text` concatena i blocchi di testo, e
//! [`user_text_only`] toglie solo i blocchi `<allegati>`/`<allegati_sessione>`/
//! `<task_playbook>`: il risultato era una directive che dichiarava al modello,
//! con la massima autorita' del system prompt, che la richiesta dell'utente
//! "ADESSO" era un promemoria di sistema o l'output di un tool.
//!
//! Nota su cosa NON era: il difetto non e' che il tag `<system-reminder>`
//! sfuggisse al filtro. Aggiungerlo alla regex avrebbe zittito il caso peggiore
//! lasciando in piedi gli altri (i nudge non hanno tag, i tool_result sono
//! blocchi tipizzati) e avrebbe confermato la premessa sbagliata: che la
//! richiesta dell'utente si possa riconoscere guardando il contenuto dei
//! messaggi (regola M). La richiesta e' un dato, e ha gia' un posto suo.
//!
//! ## Perche' senza il dato non si scrive nulla
//!
//! Se `current_turn_task` non sa rispondere, la funzione ritorna `None` e il
//! blocco non viene iniettato. Nessun ripiego sulla cronologia: una directive
//! che AFFERMA "la richiesta e' X" quando X non e' la richiesta e' peggio della
//! sua assenza, perche' sposta il lavoro sull'oggetto sbagliato invece di
//! lasciare che il modello legga la conversazione da se'. Il ripiego del
//! supervisore (che una stringa la vuole comunque, per riempire un campo del
//! prompt) resta esplicito e locale a lui.
//!
//! Regola G (no hardcode/no lettura DB nella primitiva): il flag che la governa
//! (`agent.context.turn_focus_enabled`, default `true`) NON e' letto qui dentro.
//! La funzione e' PURA — legge solo lo stato in memoria; il chiamante decide se
//! invocarla in base al flag e passa il parametro `new_topic`.
//!
//! Parita' col Python: divergenza VOLUTA e non piu' misurabile (il brain e' stato
//! rimosso). Il golden `gen_golden_turn_focus.py` esercitava proprio l'euristica
//! "ultimo messaggio umano" — cioe' il difetto — quindi e' stato tolto invece di
//! essere adattato: avrebbe misurato l'errore, con la faccia seria del verde
//! (regola O). Stessa scelta gia' fatta per `inject_turn_focus` quando il blocco
//! e' passato in coda alla parte stabile del system.

use std::sync::LazyLock;

use regex::Regex;

use crate::decisions::turn_task::current_turn_task;
use crate::state::AgentState;

/// Marcatore di idempotenza dell'iniezione nel system_text. Esposto perche' i
/// chiamanti (planner/executor) lo usano per l'iniezione idempotente.
pub const TURN_FOCUS_MARKER: &str = "[[NEXUS_TURN_FOCUS]]";

/// Soglia di troncamento dell'estratto (in CARATTERI Unicode).
const EXCERPT_MAX_CHARS: usize = 600;

// Blocchi di SISTEMA che mcp-core impagina DENTRO il messaggio del turno prima
// di consegnarlo al motore (allegati, playbook): vanno RIMOSSI prima di
// mostrarne l'estratto. Replica di `_SYSTEM_BLOCK_RE` (`task_playbook.py`):
// `<(allegati|allegati_sessione|task_playbook)...>...</tag>` con DOTALL +
// IGNORECASE.
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
/// `<allegati_sessione>`, `<task_playbook>`) impaginati nel messaggio del turno.
/// Gemello Rust di `task_playbook._user_text_only` (punto unico, regola L): se in
/// futuro serve a piu' nodi, e' qui che si estende.
pub fn user_text_only(text: &str) -> String {
    SYSTEM_BLOCK_RE.replace_all(text, "").into_owned()
}

/// Costruisce il blocco "focus del turno corrente" (anti-contaminazione della
/// history). Funzione PURA e idempotente.
///
/// Cosa estrae: la richiesta dell'utente del turno, letta dal punto unico
/// [`current_turn_task`] e ripulita dai blocchi di sistema via
/// [`user_text_only`]. NON scorre `state.messages` (vedi doc del modulo).
///
/// Casi limite:
/// - task del turno non fissato nello stato -> `None`;
/// - testo vuoto dopo la rimozione dei blocchi di sistema e il `trim` -> `None`;
/// - richiesta > 600 caratteri -> primi 600 char + `rstrip` + `" [...]"`.
///
/// `new_topic`: se `true` (il continuity gate ha rilevato un cambio
/// d'argomento) aggiunge la riga di rinforzo finale.
///
/// Regola G: il flag `turn_focus_enabled` NON e' letto qui; il chiamante decide
/// se invocare e passa `new_topic`.
pub fn build_turn_focus_directive(state: &AgentState, new_topic: bool) -> Option<String> {
    let task = current_turn_task(state)?;
    let pulito = user_text_only(task);
    let pulito = pulito.trim();
    if pulito.is_empty() {
        return None;
    }
    // Estratto compatto: la directive deve restare leggera e cacheabile. Conteggio
    // e slice su CARATTERI Unicode.
    let excerpt: String = if pulito.chars().count() <= EXCERPT_MAX_CHARS {
        pulito.to_string()
    } else {
        let head: String = pulito.chars().take(EXCERPT_MAX_CHARS).collect();
        format!("{} [...]", head.trim_end())
    };

    let mut lines: Vec<String> = vec![
        "### FOCUS DEL TURNO CORRENTE ###".to_string(),
        // NON "l'ultimo messaggio dell'utente": cio' che segue e' la richiesta
        // con cui il turno e' partito, e l'ultimo messaggio della conversazione
        // in un run agentico e' un risultato di tool. La riga descriveva la
        // vecchia euristica, e mandava il modello a cercare la richiesta nel
        // posto sbagliato.
        "La richiesta da portare a termine ADESSO e':".to_string(),
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
    use crate::decisions::turn_task::ORIGINAL_TASK_KEY;
    use crate::state::{ContentBlock, Message, MessageContent};
    use serde_json::Value;

    /// Stato con il task del turno FISSATO come lo fissa `build_initial_state`.
    fn stato_con_task(task: &str) -> AgentState {
        let mut extra = serde_json::Map::new();
        extra.insert(
            ORIGINAL_TASK_KEY.to_string(),
            Value::String(task.to_string()),
        );
        AgentState {
            extra,
            ..Default::default()
        }
    }

    fn human(text: &str) -> Message {
        Message::Human {
            content: MessageContent::text(text),
        }
    }

    #[test]
    fn vuoto_se_il_task_del_turno_non_e_fissato() {
        assert_eq!(
            build_turn_focus_directive(&AgentState::default(), false),
            None
        );
    }

    #[test]
    fn vuoto_se_solo_blocchi_di_sistema() {
        // Dopo la rimozione dei blocchi di sistema resta solo whitespace -> None.
        let st = stato_con_task("<allegati_sessione>foo.txt</allegati_sessione>");
        assert_eq!(build_turn_focus_directive(&st, false), None);
    }

    #[test]
    fn estrae_il_task_del_turno_non_l_ultimo_messaggio() {
        // La cronologia contiene, in coda, cio' che il motore agentico vi mette:
        // il risultato di un tool e un promemoria di sistema. Nessuno dei due e'
        // la richiesta dell'utente, e il focus non deve nominarli.
        let mut st = stato_con_task("crea index.html");
        st.messages = vec![
            human("vecchia richiesta su bookingService.ts"),
            Message::Ai {
                content: MessageContent::text("ho lavorato sul booking"),
                tool_calls: vec![],
                reasoning: None,
                thinking_signature: None,
            },
            Message::Human {
                content: MessageContent::Blocks(vec![
                    ContentBlock::ToolResult {
                        tool_use_id: "t1".to_string(),
                        content: Value::String("contenuto del file letto".to_string()),
                        is_error: false,
                        exit_code: None,
                    },
                    ContentBlock::Text {
                        text: "<system-reminder>\nCHECKLIST: 1) fai X\n</system-reminder>"
                            .to_string(),
                    },
                ]),
            },
            human("Prosegui con l'analisi richiesta."),
        ];
        let out = build_turn_focus_directive(&st, false).expect("directive");
        assert!(out.starts_with("### FOCUS DEL TURNO CORRENTE ###"));
        assert!(out.contains("\"crea index.html\""));
        assert!(!out.contains("system-reminder"), "focus contaminato: {out}");
        assert!(!out.contains("CHECKLIST"), "focus contaminato: {out}");
        assert!(!out.contains("contenuto del file letto"));
        assert!(!out.contains("bookingService"));
        assert!(!out.contains("Prosegui con l'analisi"));
    }

    #[test]
    fn rimuove_blocco_allegati_sessione() {
        let st = stato_con_task(
            "<allegati_sessione>\nPL.make\n</allegati_sessione>quante tabelle nel db?",
        );
        let out = build_turn_focus_directive(&st, false).expect("directive");
        assert!(out.contains("\"quante tabelle nel db?\""));
        assert!(!out.contains("PL.make"));
    }

    #[test]
    fn troncamento_oltre_600_char() {
        let lunga = "x".repeat(700);
        let st = stato_con_task(&lunga);
        let out = build_turn_focus_directive(&st, false).expect("directive");
        // 600 char + suffisso " [...]" dentro le virgolette.
        let attesa = format!("\"{} [...]\"", "x".repeat(600));
        assert!(out.contains(&attesa));
    }

    #[test]
    fn new_topic_aggiunge_riga() {
        let st = stato_con_task("nuovo task");
        let con = build_turn_focus_directive(&st, true).expect("directive");
        let senza = build_turn_focus_directive(&st, false).expect("directive");
        assert!(con.contains("rilevato un cambio di argomento"));
        assert!(!senza.contains("rilevato un cambio di argomento"));
    }

    #[test]
    fn marker_costante() {
        assert_eq!(TURN_FOCUS_MARKER, "[[NEXUS_TURN_FOCUS]]");
    }
}
