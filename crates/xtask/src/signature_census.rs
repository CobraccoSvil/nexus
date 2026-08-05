//! Censimento delle firme: **funzioni diverse che rispondono alla stessa
//! domanda**.
//!
//! # Perche' esiste
//!
//! `jscpd` (gate `scripts/dup-report.sh`) misura la duplicazione TESTUALE, e su
//! quella il repo e' gia' a 9 cloni su 1234 iniziali. Ma il repo stesso ha
//! documentato il limite: Wave 2 consolida `TemplateCache`/`TtlCache` e annota
//! *"conteggio jscpd invariato: le due copie differivano nel formato, quindi non
//! erano exact-clone; la duplicazione era STRUTTURALE"*.
//!
//! Nove cloni non significa "niente duplicazione": significa "niente
//! duplicazione testuale". Due funzioni che fanno la stessa cosa scritte
//! diversamente sono invisibili al gate attuale — e sono precisamente quelle che
//! la regola L vuole far convergere.
//!
//! # Cosa NON fa
//!
//! **La firma non prova la semantica.** Due `fn f(&PgPool, Uuid) -> Result<String>`
//! possono fare cose opposte. Questo strumento produce **candidati**, mai
//! verdetti: e' la regola M applicata a se' stesso — la classificazione non si
//! deduce dalla forma, la da' una lettura.
//!
//! Il verdetto lo scrive una persona in `scripts/signature-census-verdicts.json`
//! e **resta**: e' cio' che impedisce a un censimento di riproporre gli stessi
//! falsi positivi a ogni giro, che e' il motivo per cui i censimenti manuali si
//! esauriscono dopo due esecuzioni.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Il verdetto su un gruppo, dato da una lettura e registrato perche' non si
/// debba rifare. Enum chiuso e non un `bool` (regola Q): "li ho guardati e sono
/// diversi" e "non li ho ancora guardati" sono stati diversi, e collassarli
/// costringerebbe a riguardare tutto ogni volta.
/// Tag esplicito (`"verdetto": "..."`) e non la forma esterna di serde: questo
/// file lo scrive una PERSONA, e `{"verdetto": "distinto", "motivo": "..."}` si
/// legge, mentre `{"distinto": {"motivo": "..."}}` si decifra.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "verdetto", rename_all = "snake_case")]
pub enum Verdetto {
    /// Rispondono alla stessa domanda: da consolidare in un punto unico.
    Duplicato { nota: String },
    /// Guardati, e sono cose diverse. Il `motivo` non e' burocrazia: e' cio'
    /// che evita alla prossima persona di rifare la stessa lettura.
    Distinto { motivo: String },
    /// Da consolidare, ma non ora (serve un refactor piu' grande).
    DaConsolidare { nota: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VoceRegistro {
    firma: String,
    membri: Vec<String>,
    #[serde(flatten)]
    verdetto: Verdetto,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Registro {
    #[serde(default)]
    voci: Vec<VoceRegistro>,
}

/// Una funzione censita.
#[derive(Debug, Clone)]
struct Funzione {
    file: String,
    riga: usize,
    nome: String,
    firma: String,
    /// I nomi chiamati dentro la funzione, dal call-graph di `mcp-ast`. Sono un
    /// segnale INDIPENDENTE dalla firma: due funzioni con la stessa firma che
    /// chiamano anche le stesse cose sono molto piu' sospette di due che non
    /// condividono nulla.
    chiama: Vec<String>,
}

/// Il crate di appartenenza, dal percorso. Serve al ranking: due funzioni
/// gemelle in crate DIVERSI sono il caso che la regola L vuole prevenire, e
/// pesano piu' di due gemelle nello stesso modulo.
fn crate_di(file: &str) -> &str {
    file.split(['/', '\\'])
        .skip_while(|p| *p != "crates")
        .nth(1)
        .unwrap_or("?")
}

/// La firma normalizzata: la forma su cui due funzioni si confrontano.
///
/// L'equivalenza e' DICHIARATA qui e testata sotto, invece di essere implicita
/// nel confronto: "stessa firma o simile" non significa niente finche' non si
/// dice quali differenze non contano.
fn normalizza_tipo(t: &str) -> String {
    let mut s = t.trim().to_string();
    // Riferimenti e mutabilita' non cambiano la domanda a cui la funzione
    // risponde: `&PgPool` e `PgPool` sono lo stesso ingrediente.
    for p in ["&mut ", "&mut", "&'a ", "&"] {
        if let Some(r) = s.strip_prefix(p) {
            s = r.trim().to_string();
        }
    }
    s = s.replace("'a ", "").replace("'_ ", "");
    // Le tre forme della stringa sono la stessa cosa per chi legge la firma.
    if matches!(s.as_str(), "String" | "str" | "&str" | "String>" | "Cow<str>") {
        return "str".into();
    }
    // Il tipo d'ERRORE concreto varia legittimamente fra moduli (anyhow, un
    // thiserror locale, un Box<dyn Error>): cio' che conta e' che la funzione
    // possa fallire. `Result<T, E>` e `Result<T>` collassano su `Result<T>`.
    if let Some(interno) = s.strip_prefix("Result<").and_then(|r| r.strip_suffix('>')) {
        let primo = taglia_al_primo_livello(interno);
        return format!("Result<{}>", normalizza_tipo(primo));
    }
    if let Some(interno) = s.strip_prefix("Option<").and_then(|r| r.strip_suffix('>')) {
        return format!("Option<{}>", normalizza_tipo(interno));
    }
    if let Some(interno) = s.strip_prefix("Vec<").and_then(|r| r.strip_suffix('>')) {
        return format!("Vec<{}>", normalizza_tipo(interno));
    }
    s
}

/// Il primo argomento generico, rispettando l'annidamento: in
/// `Result<HashMap<A, B>, E>` il primo e' `HashMap<A, B>`, non `HashMap<A`.
fn taglia_al_primo_livello(s: &str) -> &str {
    let mut prof = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '<' | '(' | '[' => prof += 1,
            '>' | ')' | ']' => prof -= 1,
            ',' if prof == 0 => return s[..i].trim_end(),
            _ => {}
        }
    }
    s
}

fn firma_normalizzata(params: &[String], ret: Option<&str>) -> String {
    let p: Vec<String> = params.iter().map(|t| normalizza_tipo(t)).collect();
    let r = ret.map(normalizza_tipo).unwrap_or_else(|| "()".into());
    format!("({}) -> {}", p.join(", "), r)
}

/// Quanto e' sospetto un gruppo. Piu' segnali INDIPENDENTI valgono piu' di uno:
/// la firma da sola e' debole, ed e' il motivo per cui questo non e' un gate ma
/// un elenco di candidati.
/// Oltre questa taglia un gruppo non e' una duplicazione: e' un IDIOMA.
///
/// Misurato al primo giro: `() -> 'static Regex` raccoglieva 13 accessori di
/// `LazyLock` in tre crate, `() -> (StatusCode, Json<Value>)` tredici handler
/// axum di 410. Sono firme imposte da un pattern o da un framework, e nessuna
/// delle due e' codice da consolidare. Cio' che si consolida arriva in due o
/// tre copie, non in tredici.
const MEMBRI_MAX: usize = 6;

fn sospetto(gruppo: &[Funzione]) -> (u32, Vec<String>) {
    let mut punti = 0;
    let mut perche = Vec::new();

    // Una firma con piu' ingredienti dice molto di piu' di `() -> X`: quella
    // senza parametri e' quasi sempre un costruttore o un accessore, e il fatto
    // che due ne condividano il tipo di ritorno non e' un indizio.
    let params = gruppo[0].firma.split(')').next().map(|p| p.matches(',').count() + usize::from(p.len() > 1)).unwrap_or(0);
    if params >= 2 {
        punti += 3;
        perche.push(format!("firma ricca ({params} parametri)"));
    } else if params == 0 {
        // Non si scarta: si toglie il peso. Un `() -> T` puo' essere un vero
        // duplicato, ma deve guadagnarsi i punti dagli altri segnali.
        return (0, vec!["firma senza parametri: poco informativa".into()]);
    }

    let crates: std::collections::BTreeSet<&str> = gruppo.iter().map(|f| crate_di(&f.file)).collect();
    if crates.len() > 1 {
        punti += 3;
        perche.push(format!("crate diversi ({})", crates.into_iter().collect::<Vec<_>>().join(", ")));
    }

    // Nomi affini: token in comune fra i nomi (classify_x / classifica_x).
    let token = |n: &str| -> std::collections::BTreeSet<String> {
        n.split('_').filter(|t| t.len() > 3).map(|t| t.to_lowercase()).collect()
    };
    let primo = token(&gruppo[0].nome);
    if gruppo.iter().skip(1).any(|f| !primo.is_disjoint(&token(&f.nome))) {
        punti += 2;
        perche.push("nomi affini".into());
    }

    // Callee in comune: segnale indipendente dalla firma.
    let ch0: std::collections::BTreeSet<&String> = gruppo[0].chiama.iter().collect();
    if !ch0.is_empty()
        && gruppo.iter().skip(1).any(|f| {
            let c: std::collections::BTreeSet<&String> = f.chiama.iter().collect();
            ch0.intersection(&c).count() >= 2
        })
    {
        punti += 2;
        perche.push("chiamano le stesse cose".into());
    }

    (punti, perche)
}

fn file_rust(radice: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut pile = vec![radice.join("crates")];
    while let Some(d) = pile.pop() {
        let Ok(letti) = std::fs::read_dir(&d) else { continue };
        for e in letti.flatten() {
            let p = e.path();
            if p.is_dir() {
                let nome = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                // `target` non e' sorgente; `tests` e' codice di test, e la
                // duplicazione fra test e' un'altra domanda.
                if !matches!(nome, "target" | "tests" | ".git") {
                    pile.push(p);
                }
            } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Cosa si e' chiesto di censire.
struct Opzioni {
    solo_sospetti: bool,
    /// Il bersaglio della regola L e' la duplicazione FRA CRATE: due gemelle
    /// nello stesso modulo sono spesso fratelli legittimi
    /// (`route_after_x`/`route_after_y`), mentre due in crate diversi sono il
    /// caso che la regola vuole prevenire.
    solo_cross: bool,
    minimo: u32,
}

impl Opzioni {
    fn da_args(args: &[String]) -> Self {
        Self {
            solo_sospetti: !args.iter().any(|a| a == "--tutti"),
            solo_cross: args.iter().any(|a| a == "--crate-diversi"),
            minimo: args
                .iter()
                .position(|a| a == "--min-punti")
                .and_then(|i| args.get(i + 1))
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(5),
        }
    }
}

/// Quante fonti si sono potute misurare, e quante no. Non un totale unico
/// (regola Q): "letti 977" e "di cui 12 non misurabili" sono due fatti, e
/// fonderli nasconderebbe proprio quello che il lettore deve sapere per fidarsi
/// del resto.
struct Copertura {
    letti: usize,
    imprecisi: usize,
}

fn censisci(radice: &Path) -> Result<(Vec<Funzione>, Copertura)> {
    let mut funzioni = Vec::new();
    let mut cop = Copertura { letti: 0, imprecisi: 0 };

    for f in file_rust(radice)? {
        let Ok(src) = std::fs::read_to_string(&f) else { continue };
        let rel = f.strip_prefix(radice).unwrap_or(&f).to_string_lossy().replace('\\', "/");
        let idx = mcp_ast::index_source(&rel, &src);
        cop.letti += 1;
        // Senza AST la firma non e' misurata (il fallback regex legge una riga
        // per volta): includerli mescolerebbe "nessun parametro" e "non
        // misurato", che e' esattamente l'ambiguita' che il campo dichiara di
        // non voler avere.
        if !idx.precise {
            cop.imprecisi += 1;
            continue;
        }
        for s in idx.symbols {
            if !matches!(s.kind, mcp_ast::SymbolKind::Function | mcp_ast::SymbolKind::Method) {
                continue;
            }
            if s.params.is_empty() && s.ret.is_none() {
                continue; // niente da confrontare
            }
            let chiama = idx
                .calls
                .iter()
                .filter(|c| c.caller.as_deref() == Some(s.name.as_str()))
                .map(|c| c.callee.clone())
                .collect();
            funzioni.push(Funzione {
                file: rel.clone(),
                riga: s.line,
                nome: s.name,
                firma: firma_normalizzata(&s.params, s.ret.as_deref()),
                chiama,
            });
        }
    }
    Ok((funzioni, cop))
}

fn raggruppa(funzioni: Vec<Funzione>) -> BTreeMap<String, Vec<Funzione>> {
    let mut per_firma: BTreeMap<String, Vec<Funzione>> = BTreeMap::new();
    for f in funzioni {
        per_firma.entry(f.firma.clone()).or_default().push(f);
    }
    per_firma
}

/// Il gruppo merita di essere mostrato? Separata dalla stampa perche' i criteri
/// sono cinque e vederli in fila e' l'unico modo di accorgersi quando uno
/// contraddice un altro.
fn da_mostrare(firma: &str, gruppo: &[Funzione], opt: &Opzioni, giudicate: &std::collections::BTreeSet<&str>) -> bool {
    if gruppo.len() < 2 || gruppo.len() > MEMBRI_MAX {
        return false;
    }
    if giudicate.contains(firma) {
        return false;
    }
    if opt.solo_cross {
        let crates: std::collections::BTreeSet<&str> = gruppo.iter().map(|f| crate_di(&f.file)).collect();
        if crates.len() < 2 {
            return false;
        }
    }
    !opt.solo_sospetti || sospetto(gruppo).0 >= opt.minimo
}

pub fn run(args: &[String]) -> Result<i32> {
    let radice = std::env::current_dir()?;
    let opt = Opzioni::da_args(args);

    let percorso_registro = radice.join("scripts/signature-census-verdicts.json");
    let registro: Registro = match std::fs::read_to_string(&percorso_registro) {
        Ok(t) => serde_json::from_str(&t).context("registro verdetti illeggibile")?,
        Err(_) => Registro::default(),
    };
    let giudicate: std::collections::BTreeSet<&str> =
        registro.voci.iter().map(|v| v.firma.as_str()).collect();

    let (funzioni, cop) = censisci(&radice)?;
    let per_firma = raggruppa(funzioni);

    // La premessa accanto ai numeri (regola O).
    println!(
        "signature-census: {} file .rs letti ({} scartati: firma non misurabile senza AST)",
        cop.letti, cop.imprecisi
    );
    println!("  registro: {} gruppi gia' giudicati", registro.voci.len());
    println!("  soglia sospetto: {} punti\n", opt.minimo);

    let (candidati, saltati) = stampa_candidati(&per_firma, &opt, &giudicate);

    println!("signature-census: {candidati} gruppi da guardare, {saltati} gia' giudicati in precedenza.");
    if candidati > 0 {
        println!("Ogni gruppo va letto UNA volta; il verdetto si registra in");
        println!("scripts/signature-census-verdicts.json e non si ripropone.");
    }
    Ok(0)
}

/// Stampa i gruppi che superano i filtri e ritorna `(mostrati, saltati)`.
fn stampa_candidati(
    per_firma: &BTreeMap<String, Vec<Funzione>>,
    opt: &Opzioni,
    giudicate: &std::collections::BTreeSet<&str>,
) -> (usize, usize) {
    let mut candidati = 0usize;
    let mut saltati = 0usize;
    for (firma, gruppo) in per_firma {
        if gruppo.len() >= 2 && giudicate.contains(firma.as_str()) {
            saltati += 1;
            continue;
        }
        if !da_mostrare(firma, gruppo, opt, giudicate) {
            continue;
        }
        let (punti, perche) = sospetto(gruppo);
        candidati += 1;
        println!("[{punti} punti] {firma}   {}", perche.join(" + "));
        for f in gruppo {
            println!("    {}:{}  {}", f.file, f.riga, f.nome);
        }
        println!();
    }
    (candidati, saltati)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i_riferimenti_non_cambiano_la_domanda() {
        assert_eq!(normalizza_tipo("&PgPool"), "PgPool");
        assert_eq!(normalizza_tipo("&mut PgPool"), "PgPool");
        assert_eq!(normalizza_tipo("PgPool"), "PgPool");
    }

    #[test]
    fn le_tre_forme_della_stringa_collassano() {
        for t in ["String", "&str", "str"] {
            assert_eq!(normalizza_tipo(t), "str", "{t}");
        }
    }

    #[test]
    fn il_tipo_di_errore_concreto_non_conta() {
        // Due moduli che falliscono con errori diversi rispondono alla stessa
        // domanda: "questo puo' fallire".
        assert_eq!(normalizza_tipo("Result<String, anyhow::Error>"), "Result<str>");
        assert_eq!(normalizza_tipo("Result<String>"), "Result<str>");
        assert_eq!(normalizza_tipo("Result<String, MioErrore>"), "Result<str>");
    }

    #[test]
    fn l_annidamento_non_si_taglia_a_meta() {
        // In `Result<HashMap<A, B>, E>` il primo argomento e' `HashMap<A, B>`.
        assert_eq!(taglia_al_primo_livello("HashMap<A, B>, E"), "HashMap<A, B>");
        assert_eq!(taglia_al_primo_livello("String, E"), "String");
        assert_eq!(taglia_al_primo_livello("String"), "String");
    }

    #[test]
    fn due_funzioni_gemelle_hanno_la_stessa_firma_normalizzata() {
        let a = firma_normalizzata(
            &["&PgPool".into(), "Uuid".into()],
            Some("Result<String, anyhow::Error>"),
        );
        let b = firma_normalizzata(&["PgPool".into(), "Uuid".into()], Some("Result<String>"));
        assert_eq!(a, b);
        assert_eq!(a, "(PgPool, Uuid) -> Result<str>");
    }

    #[test]
    fn il_crate_si_legge_dal_percorso() {
        assert_eq!(crate_di("crates/mcp-core/src/lib.rs"), "mcp-core");
        assert_eq!(crate_di("crates/nexus-prompt/src/learned.rs"), "nexus-prompt");
    }
}
