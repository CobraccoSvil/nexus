//! Gate duale sui passi CRITICI: quanto e' pericoloso un passo, e che cosa ne
//! decide l'esecuzione (mig 0677, requisito "due entita' distinte + controllo
//! avversariale" del processo standard).
//!
//! Due punti unici in UN modulo (classificazione + decisione), perche' sono le
//! due meta' della stessa domanda: «questo passo puo' partire?».
//!
//! - [`classify_step`] risponde «QUANTO e' pericoloso» ([`StepCriticality`]),
//!   consumando il vocabolario dei mutatori esistente (`is_mutator_tool_name`,
//!   mai duplicato) come livello base e le REGOLE configurate
//!   (`orchestrator.critical_step_rules`, JSON in settings — dati, non
//!   varianti inseguite a codice) per i livelli alti. Le regole sono SOLO
//!   l'innesco della convocazione: il GIUDIZIO sul passo resta agentico (i
//!   due validatori su provider distinti).
//! - [`decide_step_gate`] risponde «che cosa segue dai verdetti», con la
//!   matrice COMPLETA degli esiti: il denominatore dell'unanimita' sono i
//!   validatori CONVOCATI — un timeout o un'astensione non spariscono dal
//!   conteggio (incidente `consiglio-quorum-onesto`: il voto del morto
//!   trasformava 1/2 in consenso).
//!
//! Il matcher sui comandi opera sulle PAROLE della riga scomposta, MAI con
//! contains/regex sulla riga intera (incidente
//! `contains-non-distingue-nomina-da-esegue`: un comando che NOMINA `rm -rf`
//! non lo esegue). La scomposizione e' il punto unico
//! [`super::shell_command::comandi`] (stesso scompositore di
//! `playwright_cli`): il matcher guarda le sole `parole` di ogni comando —
//! le assegnazioni `env` in testa (`FOO=1`) e i bersagli delle redirezioni
//! (`> out.log`) non sono parole eseguite e non devono far scattare un
//! pattern.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Quanto e' pericoloso un passo (ordine = severita' crescente).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepCriticality {
    /// Non muta nulla (fuori dal vocabolario dei mutatori).
    ReadOnly,
    /// Mutatore ordinario (write/edit): coperto da HITL, review e final_gate,
    /// NON dal gate duale (decisione utente del 04/08: le write ordinarie non
    /// pagano due chiamate LLM).
    Mutating,
    /// Migrazioni DB, stop/restart di servizi, git force, kill mirati:
    /// dannoso ma rimediabile. In `enforce` passa dal gate duale.
    Critical,
    /// Distruttivo e non rimediabile (rm -rf, DROP/TRUNCATE, volumi
    /// cancellati, comandi ad ampio raggio): gate duale gia' in
    /// `enforce_irreversible`, e fail-closed sulla doppia astensione.
    Irreversible,
}

impl StepCriticality {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Mutating => "mutating",
            Self::Critical => "critical",
            Self::Irreversible => "irreversible",
        }
    }
}

/// Modalita' del gate (`orchestrator.critical_step_gate_mode`, regola N:
/// identificatori canonici, un parse solo). Il rollout previsto e' progressivo:
/// `enforce_irreversible` dal giorno 1, `enforce` sui Critical dopo la
/// taratura fatta coi meta_step di sola classificazione.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StepGateMode {
    /// Nessuna classificazione, nessun costo: dispatch bit-identico a prima.
    #[default]
    Off,
    /// Classifica e PERSISTE (meta_step) senza convocare validatori ne'
    /// bloccare: telemetria a costo zero per tarare le regole.
    Observe,
    /// Convoca il gate duale SOLO sugli Irreversible; i Critical restano in
    /// osservazione (classificati e persistiti, mai bloccati).
    EnforceIrreversible,
    /// Convoca il gate duale su Critical e Irreversible.
    Enforce,
}

impl StepGateMode {
    /// Parse dell'identificatore canonico. `None` su valore ignoto: il
    /// chiamante degrada a `Off` DICHIARANDOLO (un gate di sicurezza che si
    /// accende per typo e' peggio di uno spento visibilmente).
    pub fn try_parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "observe" => Some(Self::Observe),
            "enforce_irreversible" => Some(Self::EnforceIrreversible),
            "enforce" => Some(Self::Enforce),
            _ => None,
        }
    }

    /// Il gate CONVOCA i validatori per un passo di questo livello?
    pub fn convoca(self, level: StepCriticality) -> bool {
        match self {
            Self::Off | Self::Observe => false,
            Self::EnforceIrreversible => level >= StepCriticality::Irreversible,
            Self::Enforce => level >= StepCriticality::Critical,
        }
    }

    /// Il gate PERSISTE la classificazione di questo livello (telemetria di
    /// taratura)? Vale per ogni mode acceso, sui soli livelli alti: i
    /// ReadOnly/Mutating non producono meta_step (rumore, non taratura).
    pub fn osserva(self, level: StepCriticality) -> bool {
        self != Self::Off && level >= StepCriticality::Critical
    }
}

/// Come una regola aggancia un passo. Vocabolario CHIUSO (regola N): un
/// matcher sconosciuto nel JSON scarta la regola con WARN a monte, mai un
/// ramo inventato qui.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatcherKind {
    /// Il NOME del tool (appartenenza esatta, come il vocabolario mutatori).
    ToolName,
    /// Il comando shell, per TOKEN: `pattern` e' una sequenza di token attesi
    /// (es. "rm -rf", "docker compose down"): matcha se i token del comando
    /// la CONTENGONO come sottosequenza contigua a partire da un token
    /// qualunque, col PRIMO token del pattern che deve essere il programma o
    /// un token argomento — mai sottostringhe dentro un token.
    CommandToken,
    /// Un path nell'input del tool che inizia col prefisso dato.
    InputPathPrefix,
}

/// Una regola di criticita' (deserializzata da
/// `orchestrator.critical_step_rules`, JSON array in settings).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CriticalityRule {
    pub matcher_kind: MatcherKind,
    pub pattern: String,
    pub level: StepCriticality,
    /// Categoria canonica (inglese) dichiarata nell'esito (regola M: il
    /// PERCHE' e' un campo, non si deduce).
    pub category: String,
}

/// L'esito della classificazione: livello + la regola che l'ha deciso.
#[derive(Debug, Clone, PartialEq)]
pub struct StepClassification {
    pub level: StepCriticality,
    /// `None` per ReadOnly/Mutating (nessuna regola: e' il default del
    /// vocabolario base).
    pub matched_category: Option<String>,
}

/// I token del pattern compaiono come SOTTOSEQUENZA CONTIGUA fra i token del
/// comando (mai match di sottostringa dentro un token). Case-insensitive:
/// `TASKKILL` e `taskkill` sono lo stesso programma, e questo e' un INNESCO
/// di convocazione, non un giudizio — un innesco che si aggira cambiando il
/// case non e' un innesco.
fn pattern_nei_token(tokens: &[String], pattern: &str) -> bool {
    let attesi: Vec<&str> = pattern.split_whitespace().collect();
    if attesi.is_empty() || tokens.len() < attesi.len() {
        return false;
    }
    tokens
        .windows(attesi.len())
        .any(|w| w.iter().zip(&attesi).all(|(t, a)| t.eq_ignore_ascii_case(a)))
}

/// I tool il cui input porta una riga ESEGUITA (il matcher `command_token`
/// guarda SOLO questi: una regex che leggesse il body di un write_file
/// classificherebbe la MENZIONE, non l'esecuzione). `run_service` esegue lo
/// stesso handler di `run_in_terminal`; `nexus_db_query` porta SQL eseguito
/// direttamente (campo `sql`, non quotato: un DROP TABLE li' e' l'esecuzione).
const TOOL_CON_COMANDO: &[&str] = &[
    "run_command",
    "run_tests",
    "run_in_terminal",
    "run_service",
    "git_command",
    "nexus_db_query",
];

/// Classifica UN passo (tool + input). PURA: vocabolario mutatori e regole
/// arrivano dal chiamante (regola G), niente letture qui.
pub fn classify_step(
    tool_name: &str,
    tool_input: &Value,
    fs_mutator_tools: &[String],
    rules: &[CriticalityRule],
) -> StepClassification {
    let base_mutatore = super::hitl::is_mutator_tool_name(tool_name, fs_mutator_tools);
    let mut migliore: Option<(&CriticalityRule, StepCriticality)> = None;
    for r in rules {
        if regola_colpisce(r, tool_name, tool_input)
            && migliore.map(|(_, l)| r.level > l).unwrap_or(true)
        {
            migliore = Some((r, r.level));
        }
    }
    match migliore {
        Some((r, level)) => StepClassification {
            level,
            matched_category: Some(r.category.clone()),
        },
        None if base_mutatore => StepClassification {
            level: StepCriticality::Mutating,
            matched_category: None,
        },
        None => StepClassification {
            level: StepCriticality::ReadOnly,
            matched_category: None,
        },
    }
}

/// UNA regola contro UN passo (il ramo per matcher_kind, fuori dal ciclo).
fn regola_colpisce(r: &CriticalityRule, tool_name: &str, tool_input: &Value) -> bool {
    match r.matcher_kind {
        MatcherKind::ToolName => r.pattern == tool_name,
        MatcherKind::CommandToken => {
            TOOL_CON_COMANDO.contains(&tool_name)
                && comando_del_passo(tool_input)
                    .map(|riga| comando_matcha(&riga, &r.pattern))
                    .unwrap_or(false)
        }
        MatcherKind::InputPathPrefix => path_del_passo(tool_input)
            .map(|p| p.replace('\\', "/").starts_with(&r.pattern))
            .unwrap_or(false),
    }
}

/// La riga scomposta (punto unico [`super::shell_command::comandi`]) contiene
/// il pattern nelle PAROLE di almeno uno dei suoi comandi. Il match e' sulle
/// sole `parole`: le assegnazioni `env` in testa e i bersagli delle
/// redirezioni non sono parole eseguite e non devono far scattare un pattern.
fn comando_matcha(riga: &str, pattern: &str) -> bool {
    super::shell_command::comandi(riga)
        .iter()
        .any(|c| pattern_nei_token(&c.parole, pattern))
}

fn comando_del_passo(input: &Value) -> Option<String> {
    input
        .get("command")
        .or_else(|| input.get("cmd"))
        .or_else(|| input.get("sql"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn path_del_passo(input: &Value) -> Option<String> {
    input
        .get("path")
        .or_else(|| input.get("file_path"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Il verdetto di UN validatore, come arriva dal wire (campo, mai prosa —
/// regola M/Q). `Abstained` copre timeout, risposta fuori schema e JoinError:
/// il validatore era CONVOCATO e non ha risposto — resta nel denominatore.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepVerdict {
    Approve,
    Reject,
    NeedsHuman,
    Abstained,
}

/// La decisione del gate sul batch, dai verdetti dei CONVOCATI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepGateDecision {
    /// Unanimita' dei convocati: si procede (i verdetti restano allegati).
    Approved,
    /// Almeno un reject: il batch NON si esegue, il modello riceve i motivi
    /// (rimando, non chiusura).
    Rejected,
    /// Disaccordo, needs_human esplicito, o Irreversible senza unanimita'
    /// piena: sospensione HITL (anche in Automatic: e' il punto del
    /// requisito).
    NeedsHuman,
    /// Nessun verdetto utilizzabile su un Critical: si procede DICHIARANDOLO
    /// (una barriera che appende il run e' peggio del problema — ADR 0040);
    /// per Irreversible non esiste: quella via porta a NeedsHuman.
    UnavailableDeclared,
}

/// La matrice degli esiti (review del piano, GAP-2): unanimita' per
/// approvare, UN reject ferma, l'astensione NON e' un si'.
///
/// | verdetti                  | Critical              | Irreversible |
/// |---------------------------|-----------------------|--------------|
/// | tutti Approve             | Approved              | Approved     |
/// | almeno un Reject          | Rejected              | Rejected     |
/// | almeno un NeedsHuman      | NeedsHuman            | NeedsHuman   |
/// | Approve + Abstained       | NeedsHuman            | NeedsHuman   |
/// | tutti Abstained           | UnavailableDeclared   | NeedsHuman   |
pub fn decide_step_gate(verdicts: &[StepVerdict], level: StepCriticality) -> StepGateDecision {
    if verdicts.contains(&StepVerdict::Reject) {
        return StepGateDecision::Rejected;
    }
    if verdicts.contains(&StepVerdict::NeedsHuman) {
        return StepGateDecision::NeedsHuman;
    }
    if !verdicts.is_empty() && verdicts.iter().all(|v| *v == StepVerdict::Approve) {
        // «Due entita' distinte» e' il requisito, non un auspicio: su un
        // Irreversible un SOLO giudice (selezione degradata) non fa
        // unanimita' — decide l'umano. Sui Critical il degrado dichiarato
        // ammette il giudice singolo (review avversaria del 05/08).
        if level == StepCriticality::Irreversible && verdicts.len() < 2 {
            return StepGateDecision::NeedsHuman;
        }
        return StepGateDecision::Approved;
    }
    // Astensioni in mezzo (o nessun convocato): mai un si' implicito.
    let tutti_astenuti = verdicts.iter().all(|v| *v == StepVerdict::Abstained);
    if tutti_astenuti && level == StepCriticality::Critical {
        return StepGateDecision::UnavailableDeclared;
    }
    StepGateDecision::NeedsHuman
}

/// Chiave `extra` del contatore dei rimandi del gate duale nel run (cap anti
/// ping-pong: oltre `critical_step_max_rejections` si degrada a NeedsHuman).
pub const STEP_GATE_REJECTIONS_EXTRA_KEY: &str = "step_gate_rejections";

/// Chiave `extra` dei verdetti allegati a una sospensione HITL nata dal gate
/// duale: l'umano decide VEDENDO cosa hanno detto i validatori.
pub const STEP_GATE_VERDICTS_EXTRA_KEY: &str = "step_gate_verdicts";

// Il permesso di eseguire un batch dopo una sospensione NON e' una chiave
// `extra`: e' il campo tipizzato `AgentState::step_gate_human_ok`, scritto dal
// RESUME (la risposta dell'umano) e consumato dal dispatch in un solo giro.
// Qui c'era un marker con gli id del batch, scritto alla SOSPENSIONE: dichiarava
// deliberato un batch mentre se ne chiedeva la decisione, e al rientro nel
// dispatch lo faceva passare senza rivalidazione (run 77fcff4a del 05/08/2026:
// `rm -rf` eseguito 482ms dopo il proprio `NeedsHuman`).

/// Kind del meta_step di ogni convocazione/osservazione del gate (payload
/// slim coi validatori: provider, modello, verdetto|astensione+causa, costo).
pub const STEP_VALIDATION_META_KIND: &str = "step_validation";

/// Deserializza le regole dal JSON di `orchestrator.critical_step_rules`:
/// le voci malformate vengono scartate UNA a una con WARN (una regola rotta
/// non spegne il vocabolario intero).
pub fn parse_rules(raw: &str) -> Vec<CriticalityRule> {
    let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(raw) else {
        tracing::warn!("critical_step_rules: JSON non-array, vocabolario vuoto");
        return Vec::new();
    };
    arr.into_iter()
        .filter_map(|v| match serde_json::from_value::<CriticalityRule>(v.clone()) {
            Ok(r) => Some(r),
            Err(err) => {
                tracing::warn!(voce = %v, error = %err, "critical_step_rules: regola scartata");
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mutatori() -> Vec<String> {
        vec!["write_file".into(), "run_command".into(), "stop_service".into()]
    }

    /// La matrice mode x livello che decide chi paga i validatori. Mutazione:
    /// invertire il `>=` di `convoca` in `EnforceIrreversible` fa convocare i
    /// Critical (costo non deciso dall'admin) -> rosso qui.
    #[test]
    fn mode_convoca_solo_dal_livello_dichiarato() {
        use StepCriticality::*;
        use StepGateMode::*;
        assert!(!Off.convoca(Irreversible));
        assert!(!Observe.convoca(Irreversible));
        assert!(!EnforceIrreversible.convoca(Critical));
        assert!(EnforceIrreversible.convoca(Irreversible));
        assert!(Enforce.convoca(Critical));
        assert!(Enforce.convoca(Irreversible));
        assert!(!Enforce.convoca(Mutating));
        // Telemetria: ogni mode acceso osserva i livelli alti, mai i bassi.
        assert!(Observe.osserva(Critical));
        assert!(EnforceIrreversible.osserva(Critical));
        assert!(!Off.osserva(Irreversible));
        assert!(!Enforce.osserva(Mutating));
    }

    /// Identificatori canonici (regola N): un typo NON accende il gate.
    #[test]
    fn mode_parse_canonico_e_nessun_sinonimo() {
        assert_eq!(StepGateMode::try_parse(" Enforce "), Some(StepGateMode::Enforce));
        assert_eq!(
            StepGateMode::try_parse("enforce_irreversible"),
            Some(StepGateMode::EnforceIrreversible)
        );
        assert_eq!(StepGateMode::try_parse("observe"), Some(StepGateMode::Observe));
        assert_eq!(StepGateMode::try_parse("off"), Some(StepGateMode::Off));
        assert_eq!(StepGateMode::try_parse("attivo"), None);
        assert_eq!(StepGateMode::try_parse("enforce-irreversible"), None);
    }

    fn regole() -> Vec<CriticalityRule> {
        parse_rules(
            r#"[
              {"matcher_kind":"command_token","pattern":"rm -rf","level":"irreversible","category":"recursive_delete"},
              {"matcher_kind":"command_token","pattern":"docker compose down","level":"critical","category":"stack_down"},
              {"matcher_kind":"tool_name","pattern":"stop_service","level":"critical","category":"service_stop"}
            ]"#,
        )
    }

    /// IL principio del matcher (GAP-1): un comando che NOMINA `rm -rf` non lo
    /// esegue. `echo` e `cat` con la stringa dentro non matchano; il comando
    /// vero si', anche dentro una catena.
    ///
    /// MUTAZIONE: sostituire il match per token con un contains sulla riga fa
    /// matchare `echo "rm -rf"` e la prima asserzione cade.
    #[test]
    fn la_menzione_non_e_esecuzione() {
        let classify = |cmd: &str| {
            classify_step(
                "run_command",
                &json!({ "command": cmd }),
                &mutatori(),
                &regole(),
            )
        };
        assert_eq!(classify("echo 'rm -rf tutto'").level, StepCriticality::Mutating);
        assert_eq!(classify("cat cleanup.sh").level, StepCriticality::Mutating);
        assert_eq!(classify("rm -rf build/").level, StepCriticality::Irreversible);
        assert_eq!(
            classify("npm ci && rm -rf dist && npm run build").level,
            StepCriticality::Irreversible
        );
        // Il pattern multi-token e' una sottosequenza CONTIGUA di token.
        assert_eq!(
            classify("docker compose down -v").level,
            StepCriticality::Critical
        );
        assert_eq!(classify("docker compose up -d").level, StepCriticality::Mutating);

        // Delega allo scompositore unico (consolidamento): un'assegnazione env
        // in testa non spezza il match sulle PAROLE del comando.
        assert_eq!(
            classify("FORCE=1 rm -rf build/").level,
            StepCriticality::Irreversible
        );
        // La redirezione non produce piu' token spuri (`2>&1` non lascia un
        // comando fantasma): un `rm` come BERSAGLIO di redirezione, non
        // eseguito, non matcha. MUTAZIONE: far vedere al matcher env+parole o
        // ripristinare l'ex tokenizzatore -> uno di questi due casi cade.
        assert_eq!(
            classify("node build.js 2>&1").level,
            StepCriticality::Mutating,
            "run_command e' mutatore; il pattern rm -rf non c'e' fra le parole"
        );
        assert_eq!(
            classify("echo done > rm -rf").level,
            StepCriticality::Mutating,
            "rm/-rf sono bersaglio di redirezione e argomento, non un rm eseguito"
        );
    }

    /// Il livello base viene dal vocabolario mutatori ESISTENTE (consumato,
    /// mai duplicato): tool non mutatore -> ReadOnly, mutatore senza regole
    /// -> Mutating, tool_name in regola -> il suo livello.
    #[test]
    fn base_dal_vocabolario_mutatori_e_tool_name() {
        let c = classify_step("read_file", &json!({"path": "a"}), &mutatori(), &regole());
        assert_eq!(c.level, StepCriticality::ReadOnly);
        let c = classify_step("write_file", &json!({"path": "a"}), &mutatori(), &regole());
        assert_eq!(c.level, StepCriticality::Mutating);
        assert_eq!(c.matched_category, None);
        let c = classify_step("stop_service", &json!({}), &mutatori(), &regole());
        assert_eq!(c.level, StepCriticality::Critical);
        assert_eq!(c.matched_category.as_deref(), Some("service_stop"));
    }

    /// La matrice degli esiti (GAP-2): il denominatore sono i CONVOCATI —
    /// approve+astensione NON e' approvazione; la doppia astensione degrada
    /// dichiarata sui Critical e fail-closed sugli Irreversible.
    ///
    /// MUTAZIONE: far contare solo i verdetti "arrivati" (filtrare gli
    /// Abstained prima del tutti-Approve) trasforma approve+timeout in
    /// Approved e la terza asserzione cade — e' l'incidente del quorum sul
    /// sopravvissuto.
    #[test]
    fn matrice_esiti_su_convocati() {
        use StepCriticality::{Critical, Irreversible};
        use StepGateDecision as D;
        use StepVerdict::{Abstained, Approve, NeedsHuman, Reject};
        assert_eq!(decide_step_gate(&[Approve, Approve], Critical), D::Approved);
        assert_eq!(decide_step_gate(&[Approve, Reject], Critical), D::Rejected);
        assert_eq!(decide_step_gate(&[Approve, Abstained], Critical), D::NeedsHuman);
        assert_eq!(decide_step_gate(&[Approve, NeedsHuman], Critical), D::NeedsHuman);
        assert_eq!(
            decide_step_gate(&[Abstained, Abstained], Critical),
            D::UnavailableDeclared
        );
        assert_eq!(
            decide_step_gate(&[Abstained, Abstained], Irreversible),
            D::NeedsHuman
        );
        assert_eq!(decide_step_gate(&[], Critical), D::UnavailableDeclared);
        // «Due entita' distinte»: un SOLO giudice non approva un Irreversible
        // (selezione degradata -> decide l'umano); su un Critical il degrado
        // dichiarato lo ammette. Mutazione: togliere il minimo di convocati
        // dall'all-Approve -> il primo assert va ad Approved -> rosso.
        assert_eq!(decide_step_gate(&[Approve], Irreversible), D::NeedsHuman);
        assert_eq!(decide_step_gate(&[Approve], Critical), D::Approved);
    }

    /// Le regole malformate cadono UNA a una, mai il vocabolario intero.
    #[test]
    fn regola_rotta_non_spegne_il_vocabolario() {
        let rules = parse_rules(
            r#"[
              {"matcher_kind":"command_token","pattern":"rm -rf","level":"irreversible","category":"recursive_delete"},
              {"matcher_kind":"boh","pattern":"x","level":"critical","category":"y"}
            ]"#,
        );
        assert_eq!(rules.len(), 1);
        assert_eq!(parse_rules("non-json").len(), 0);
    }
}
