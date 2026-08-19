//! Un mandato non e' un blob: e' una **sequenza ordinata di segmenti**. Punto
//! unico (regola L) di «quali blocchi contiene questo prompt», «togline uno»,
//! «aggiungine uno» — poste sulla STRUTTURA e mai su una sottostringa.
//!
//! ## IL DIFETTO, misurato
//!
//! Un prompt e' un `TEXT` in `nexus_prompt_templates.content`, e ogni domanda su
//! di esso era lessicale. Il 18/08/2026 il costo si e' visto per intero:
//!
//! ```text
//! ILIKE '%prove eseguibili%'   -> 0 su 8   (falso)
//! LIKE  '%<prove_eseguibili>%' -> 8 su 8   (vero)
//! ```
//!
//! In `LIKE` l'underscore e' un jolly e lo spazio no. Lo zero non e'
//! distinguibile da «non c'e'», e su quello zero e' stato costruito — e fatto
//! implementare — un mandato di correzione per un difetto inesistente. Il
//! difetto non era la query: era che quella fosse l'unica forma in cui la
//! domanda si potesse porre.
//!
//! La stessa fragilita' viveva in ogni consumatore: `.contains("<tag>")` nei
//! test, `content LIKE '%<tag>%'` nei guard di migrazione,
//! `system_text.contains("<reflection>")` nel gate del nodo di riflessione, e
//! soprattutto [`crate::blocchi::strip_block_between`], che e' la sola primitiva
//! di manipolazione esistente e lavora per delimitatori testuali.
//!
//! ## Che cos'e' un blocco, e perche' l'identita' NON sta in un catalogo
//!
//! Un blocco e' `<nome> ... </nome>` con `nome` in `[a-zA-Z][a-zA-Z0-9_-]*`. La
//! sua IDENTITA' e' il nome del tag, e basta: non c'e' una tabella di catalogo
//! che lo dichiari, e la scelta e' misurata, non estetica.
//!
//! Un catalogo sarebbe una SECONDA risposta alla domanda «questo e' un blocco?»,
//! accanto a quella che la mig 0744 ha gia' dichiarato punto unico
//! (`nexus_prompt_blocchi`, riconoscimento per tag di chiusura). E sarebbe
//! **incompleto per costruzione**: le figure nascono a RUNTIME dal FigureWizard,
//! che scrive `subagent.<kind>.base` con un contenuto arbitrario — un blocco
//! introdotto li' non starebbe in nessun catalogo, la copertura direbbe
//! «assente» su un blocco presente, e sarebbe il difetto d'origine con un nome
//! nuovo. Il vocabolario dei blocchi non e' configurazione: e' cio' che il testo
//! CONTIENE, e si misura.
//!
//! ## Perche' una funzione pura e non righe in tabella
//!
//! La scomposizione non si PERSISTE. Persisterla darebbe due case allo stesso
//! dato — le righe segmento e la colonna `content` che ~40 lettori interrogano —
//! da tenere allineate da un trigger, su una tabella che ha gia' un trigger
//! fail-closed (la 0744) e quattro scrittori di produzione. La regola G e'
//! soddisfatta meglio cosi': `content` resta l'unica fonte, la struttura e' una
//! DERIVAZIONE calcolata quando serve, e non c'e' nulla da allineare.
//!
//! ## L'invariante che regge tutto
//!
//! ```text
//! rendi(scomponi(x)) == x     per ogni x, byte per byte
//! ```
//!
//! Senza di esso «togli un blocco» non e' un'operazione sul prompt: e' una
//! riscrittura del prompt che per caso somiglia all'originale. MISURATO il
//! 19/08/2026 sul META vivo, 174 righe attive: **0 su 174 cambiano un byte**
//! attraversando la scomposizione. La proprieta' resta sorvegliata da
//! `il_giro_dalla_scomposizione_non_cambia_un_byte`, che gira sul corpus del DB
//! migrato a ogni gate.
//!
//! ## Tre fatti del corpus reale che hanno deciso il criterio
//!
//! (misura del 19/08/2026 sul META vivo: 180 righe, 174 attive, 58 con almeno un
//! blocco, 65 tag distinti)
//!
//! 1. **L'annidamento esiste, ed e' uno.** `<suggested_actions>` sta dentro
//!    `<next_actions>` in `system.nexus_base`. Uno scompositore a scansione
//!    piatta emetterebbe il blocco esterno intero e direbbe «assente» sul
//!    blocco interno, che c'e'. Per questo la scomposizione e' RICORSIVA: il
//!    corpo di un blocco e' a sua volta una [`Composizione`]. Costo nullo
//!    sull'invariante (un blocco si rende come apertura + corpo + chiusura), e
//!    la classe di difetto sparisce invece di essere censita.
//! 2. **Un'apertura senza chiusura NON e' un'anomalia**: e' prosa. Sul corpus
//!    reale sono 20 occorrenze su 6 template, e sono TUTTE segnaposto descrittivi
//!    (`punteggio: <float>`, `<stringa>`, `<nome>`) dentro istruzioni di formato.
//!    Registrarle come anomalie significherebbe gridare al lupo su sei template
//!    sani. Il criterio e' quindi: apertura senza la propria chiusura => non e'
//!    un blocco, il testo resta nell'interstizio e nessuno se ne lamenta.
//! 3. **Nessun template ripete lo stesso blocco** (0 occorrenze multiple su 174).
//!    Percio' [`Composizione::ha`] risponde con un booleano e non deve
//!    distinguere «una volta» da «due»; se un giorno accadesse, `senza` le
//!    toglie tutte e `blocchi_dichiarati` le conta una volta — nessuna delle due
//!    inventa un caso.
//! 4. **Il tag di apertura puo' portare ATTRIBUTI.** Tre template attivi aprono
//!    con `<todo_list version="{{plan_version}}">`, `<subagent_result kind="..."
//!    ...>`, `<verification_failed cycle="..." ...>` — e sono i template che il
//!    runtime riempie. Il primo giro di questo modulo li ignorava, e il ponte
//!    con la funzione SQL della 0744 (che conta il tag di CHIUSURA, quindi li
//!    dichiara) e' rosseggiato su esattamente quelle tre righe: due idee di
//!    «blocco» sulla stessa tabella. Il criterio le accetta, e i byte esatti
//!    dell'apertura viaggiano nel segmento perche' la resa non li perda.
//!
//! ## L'interstizio e' una variante, non un `Option` dimenticato
//!
//! Non ogni byte appartiene a un blocco: il paragrafo di apertura di
//! `agent.coder.base` non e' un blocco e battezzarlo `intro_1` sarebbe fabbricare
//! vocabolario per far quadrare uno schema. [`Segmento::Interstizio`] e' la
//! forma dichiarata di «questo pezzo di testo non ha un nome» (regola Q), ed e'
//! cio' che rende la scomposizione senza perdita.

use std::collections::BTreeSet;

/// Quanto in profondita' si scompone il corpo di un blocco.
///
/// Il corpus reale ha profondita' 2 (un solo annidamento). Il tetto esiste
/// perche' un testo patologico non faccia ricorsione illimitata; raggiunto il
/// tetto, il corpo resta un interstizio — **niente si perde**, si perde solo la
/// capacita' di NOMINARE cio' che vi sta dentro, e la resa e' identica.
const PROFONDITA_MASSIMA: usize = 8;

/// L'identita' di un blocco: il nome del suo tag.
///
/// Newtype e non `String` perche' «prove eseguibili» e «prove_eseguibili» sono
/// due cose diverse e solo la seconda e' una chiave: il costruttore rifiuta cio'
/// che non puo' essere un tag, e da quel punto in poi la distinzione non si puo'
/// piu' dimenticare (regola Q).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChiaveBlocco(String);

impl ChiaveBlocco {
    /// Dalla forma grezza. `None` se non e' un nome di tag ammissibile: il
    /// criterio e' lo STESSO della funzione SQL `nexus_prompt_blocchi` (mig
    /// 0744), e le due implementazioni esistono solo perche' SQL e Rust non si
    /// chiamano — il test `rust_e_sql_riconoscono_gli_stessi_blocchi` le
    /// confronta sul corpus vero.
    pub fn nuova(nome: &str) -> Option<Self> {
        let mut c = nome.chars();
        let primo = c.next()?;
        if !primo.is_ascii_alphabetic() {
            return None;
        }
        if !c.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-') {
            return None;
        }
        Some(Self(nome.to_string()))
    }

    /// Il nome nudo del tag, senza delimitatori.
    pub fn come_str(&self) -> &str {
        &self.0
    }

    /// `<nome>`. La FORMA si compone qui e in nessun altro posto: un chiamante
    /// che la scrivesse a mano potrebbe scriverla diversa da come la si cerca.
    pub fn apertura(&self) -> String {
        format!("<{}>", self.0)
    }

    /// `</nome>`.
    pub fn chiusura(&self) -> String {
        format!("</{}>", self.0)
    }
}

impl std::fmt::Display for ChiaveBlocco {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Un pezzo del prompt: o ha un nome, o non ce l'ha.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segmento {
    /// `<chiave ...> ... </chiave>`. Il corpo e' a sua volta scomposto: e' il
    /// caso misurato di `<suggested_actions>` dentro `<next_actions>`.
    Blocco {
        chiave: ChiaveBlocco,
        /// I byte ESATTI del tag di apertura, attributi compresi.
        ///
        /// Non si ricompone da [`ChiaveBlocco::apertura`], che rende la sola
        /// forma canonica: sul corpus reale tre template aprono con attributi
        /// (`<todo_list version="{{plan_version}}">`), e ricomporre la forma
        /// canonica al posto loro perderebbe quegli attributi — cioe' romperebbe
        /// l'invariante byte proprio sui template che il runtime riempie.
        apertura: String,
        corpo: Composizione,
    },
    /// Prosa che nessun blocco rivendica.
    Interstizio(String),
}

impl Segmento {
    /// I byte di questo segmento nel prompt originale.
    fn rendi_in(&self, out: &mut String) {
        match self {
            Self::Interstizio(t) => out.push_str(t),
            Self::Blocco { chiave, apertura, corpo } => {
                out.push_str(apertura);
                corpo.rendi_in(out);
                out.push_str(&chiave.chiusura());
            }
        }
    }
}

/// Il prompt come sequenza di segmenti.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Composizione {
    segmenti: Vec<Segmento>,
}

/// Esito di una rimozione. Il caso «non c'era» e' una variante e non un
/// silenzio: per [`crate::ambiente`] «ho tolto la direttiva su apt» e «il
/// delimitatore non c'era, la direttiva e' rimasta» hanno conseguenze opposte,
/// e finche' entrambi producevano lo stesso `String` la seconda era invisibile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EsitoRimozione {
    /// Il blocco c'era ed e' stato tolto (quante occorrenze).
    Rimosso { occorrenze: usize },
    /// Il blocco non era nel prompt. Non e' un errore, ed e' un FATTO che il
    /// chiamante puo' dichiarare.
    NonPresente,
}

/// Esito di un innesto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EsitoInnesto {
    Innestato,
    /// Idempotenza: il blocco c'era gia' e non si duplica.
    GiaPresente,
}

impl Composizione {
    /// Scompone un prompt. Non fallisce mai: cio' che non e' un blocco e'
    /// interstizio, e l'invariante `rendi(scomponi(x)) == x` vale su qualunque
    /// stringa, anche vuota, anche malformata.
    pub fn scomponi(testo: &str) -> Self {
        Self::scomponi_a(testo, PROFONDITA_MASSIMA)
    }

    fn scomponi_a(testo: &str, profondita: usize) -> Self {
        let mut segmenti = Vec::new();
        if profondita == 0 {
            if !testo.is_empty() {
                segmenti.push(Segmento::Interstizio(testo.to_string()));
            }
            return Self { segmenti };
        }
        let bytes = testo.as_bytes();
        // Inizio dell'interstizio corrente.
        let mut inizio = 0usize;
        // Posizione di scansione.
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] != b'<' {
                i += 1;
                continue;
            }
            // `<` e' ASCII: in UTF-8 nessun byte di continuazione vale 0x3C,
            // quindi questa posizione e' sempre un confine di carattere e non
            // serve interrogarlo.
            let Some((chiave, dopo_apertura)) = tag_di_apertura(testo, i) else {
                i += 1;
                continue;
            };
            let chiusura = chiave.chiusura();
            let Some(rel) = testo[dopo_apertura..].find(&chiusura) else {
                // Apertura senza la propria chiusura: NON e' un blocco, e' prosa
                // (i segnaposto `<float>`/`<stringa>` dei template di formato).
                // Si riparte dopo l'apertura, che resta nell'interstizio.
                i = dopo_apertura;
                continue;
            };
            let inizio_chiusura = dopo_apertura + rel;
            let fine = inizio_chiusura + chiusura.len();
            if inizio < i {
                segmenti.push(Segmento::Interstizio(testo[inizio..i].to_string()));
            }
            segmenti.push(Segmento::Blocco {
                chiave,
                apertura: testo[i..dopo_apertura].to_string(),
                corpo: Self::scomponi_a(&testo[dopo_apertura..inizio_chiusura], profondita - 1),
            });
            inizio = fine;
            i = fine;
        }
        if inizio < testo.len() {
            segmenti.push(Segmento::Interstizio(testo[inizio..].to_string()));
        }
        Self { segmenti }
    }

    /// I byte del prompt. Inversa esatta di [`Self::scomponi`].
    pub fn rendi(&self) -> String {
        let mut out = String::new();
        self.rendi_in(&mut out);
        out
    }

    fn rendi_in(&self, out: &mut String) {
        for s in &self.segmenti {
            s.rendi_in(out);
        }
    }

    /// Il prompt contiene questo blocco? A qualunque profondita'.
    ///
    /// E' la domanda che l'`ILIKE` non sapeva porre. Nessuna sottostringa:
    /// uguaglianza su un nome di tag, quindi l'underscore non e' un jolly e una
    /// MENZIONE in prosa (`vedi il blocco <safety_progetto>`, che non porta la
    /// chiusura) non conta come presenza.
    pub fn ha(&self, chiave: &ChiaveBlocco) -> bool {
        self.segmenti.iter().any(|s| match s {
            Segmento::Interstizio(_) => false,
            Segmento::Blocco { chiave: k, corpo, .. } => k == chiave || corpo.ha(chiave),
        })
    }

    /// Tutti i blocchi dichiarati dal prompt, a ogni profondita'.
    ///
    /// Ordinato e deduplicato: e' l'insieme che il test di ponte confronta con
    /// la funzione SQL `nexus_prompt_blocchi`.
    pub fn blocchi_dichiarati(&self) -> BTreeSet<ChiaveBlocco> {
        let mut acc = BTreeSet::new();
        self.raccogli_in(&mut acc);
        acc
    }

    fn raccogli_in(&self, acc: &mut BTreeSet<ChiaveBlocco>) {
        for s in &self.segmenti {
            if let Segmento::Blocco { chiave, corpo, .. } = s {
                acc.insert(chiave.clone());
                corpo.raccogli_in(acc);
            }
        }
    }

    /// Toglie il blocco (tutte le occorrenze, a qualunque profondita').
    ///
    /// Gli a capo che PRECEDONO il blocco rimosso vengono assorbiti: chi legge
    /// il prompt e' un modello, e una riga vuota al posto di un blocco non e'
    /// gratis. E' lo stesso comportamento di
    /// [`crate::blocchi::strip_block_between`], che questo metodo sostituisce —
    /// l'equivalenza e' asserita da `senza_si_comporta_come_lo_strip_testuale`.
    pub fn senza(&mut self, chiave: &ChiaveBlocco) -> EsitoRimozione {
        let occorrenze = self.togli(chiave);
        if occorrenze == 0 {
            EsitoRimozione::NonPresente
        } else {
            EsitoRimozione::Rimosso { occorrenze }
        }
    }

    /// Quante occorrenze del blocco sono state tolte, a questo livello e sotto.
    ///
    /// L'indice si gestisce a mano invece che con un `retain` perche' la
    /// rimozione ha un effetto sul segmento PRECEDENTE (gli a capo assorbiti),
    /// e perche' un blocco non bersaglio va comunque attraversato: il caso reale
    /// e' `<suggested_actions>` dentro `<next_actions>`.
    fn togli(&mut self, chiave: &ChiaveBlocco) -> usize {
        let mut tolti = 0usize;
        let mut i = 0usize;
        while i < self.segmenti.len() {
            let e_bersaglio = matches!(
                &self.segmenti[i],
                Segmento::Blocco { chiave: k, .. } if k == chiave
            );
            if e_bersaglio {
                self.segmenti.remove(i);
                tolti += 1;
                i = self.assorbi_a_capo_prima_di(i);
                continue;
            }
            if let Segmento::Blocco { corpo, .. } = &mut self.segmenti[i] {
                tolti += corpo.togli(chiave);
            }
            i += 1;
        }
        tolti
    }

    /// Toglie gli a capo finali dell'interstizio che precede la posizione `i`,
    /// e l'interstizio stesso se non resta altro. Ritorna la nuova `i`.
    ///
    /// Chi legge il prompt e' un modello: una riga vuota al posto di un blocco
    /// rimosso non e' gratis, ed e' la stessa cura che
    /// [`crate::blocchi::strip_block_between`] applica sulla testa.
    fn assorbi_a_capo_prima_di(&mut self, i: usize) -> usize {
        if i == 0 {
            return i;
        }
        let Some(Segmento::Interstizio(t)) = self.segmenti.get_mut(i - 1) else {
            return i;
        };
        let tagliato = t.trim_end_matches('\n').to_string();
        if tagliato.is_empty() {
            self.segmenti.remove(i - 1);
            return i - 1;
        }
        *t = tagliato;
        i
    }

    /// Appende un blocco in coda, se non c'e' gia'.
    ///
    /// `testo` e' il blocco COMPLETO, delimitatori inclusi, cosi' come vive nel
    /// DB: il chiamante non lo ricompone da un corpo, perche' comporre la forma
    /// e' compito della sola [`ChiaveBlocco`]. Se il testo non contiene il
    /// blocco dichiarato, l'innesto non avviene — appendere qualcosa che non e'
    /// il blocco promesso e' peggio del silenzio.
    pub fn appendi(&mut self, chiave: &ChiaveBlocco, testo: &str) -> EsitoInnesto {
        if self.ha(chiave) {
            return EsitoInnesto::GiaPresente;
        }
        let da_innestare = Self::scomponi(testo);
        if !da_innestare.ha(chiave) {
            return EsitoInnesto::GiaPresente;
        }
        self.segmenti.extend(da_innestare.segmenti);
        EsitoInnesto::Innestato
    }

    /// I segmenti, per chi deve ispezionarli (diagnostica, test).
    pub fn segmenti(&self) -> &[Segmento] {
        &self.segmenti
    }
}

/// Se a `pos` comincia un tag di apertura, la sua chiave e la posizione del
/// primo byte dopo `>`.
///
/// Gli ATTRIBUTI sono ammessi (`<todo_list version="{{plan_version}}">`) e non
/// e' una concessione: sul META vivo tre template attivi aprono cosi'
/// (`agent.todo_reminder.tpl`, `subagent.result_block`,
/// `verification.failed_block`, misurati il 19/08/2026), e sono proprio i
/// template che il runtime riempie. Rifiutarli renderebbe questo criterio
/// DIVERSO da quello della mig 0744 — che conta il tag di CHIUSURA e quei tre
/// li dichiara — cioe' due idee di «blocco» sulla stessa tabella.
///
/// La scansione degli attributi si ferma al primo `<`: senza quel limite una
/// prosa come `a < b e c > d` diventerebbe un tag lungo un paragrafo.
fn tag_di_apertura(testo: &str, pos: usize) -> Option<(ChiaveBlocco, usize)> {
    let bytes = testo.as_bytes();
    debug_assert_eq!(bytes[pos], b'<');
    let fine_nome = fine_del_nome(bytes, pos + 1);
    let chiave = ChiaveBlocco::nuova(testo.get(pos + 1..fine_nome)?)?;
    let fine_tag = fine_del_tag(bytes, fine_nome)?;
    Some((chiave, fine_tag))
}

/// Il primo byte dopo il nome che comincia a `da`.
fn fine_del_nome(bytes: &[u8], da: usize) -> usize {
    let mut j = da;
    while j < bytes.len() {
        let c = bytes[j];
        if !(c.is_ascii_alphanumeric() || c == b'_' || c == b'-') {
            break;
        }
        j += 1;
    }
    j
}

/// Il primo byte dopo il `>` che chiude il tag il cui nome finisce a
/// `fine_nome`. `None` se li' il tag non si chiude.
fn fine_del_tag(bytes: &[u8], fine_nome: usize) -> Option<usize> {
    match bytes.get(fine_nome)? {
        b'>' => Some(fine_nome + 1),
        c if c.is_ascii_whitespace() => {
            let mut k = fine_nome;
            while k < bytes.len() && bytes[k] != b'>' && bytes[k] != b'<' {
                k += 1;
            }
            (bytes.get(k) == Some(&b'>')).then_some(k + 1)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> ChiaveBlocco {
        ChiaveBlocco::nuova(s).expect("chiave valida")
    }

    /// L'INVARIANTE. Tutto il resto del modulo poggia qui.
    #[test]
    fn il_giro_e_senza_perdita_su_forme_note() {
        for testo in [
            "",
            "solo prosa",
            "<a>corpo</a>",
            "testa\n<a>corpo</a>\ncoda",
            // annidamento, il caso reale di system.nexus_base
            "<next_actions>x<suggested_actions>y</suggested_actions>z</next_actions>",
            // segnaposto: apertura senza chiusura, resta prosa
            "punteggio: <float> e nome: <stringa>",
            // chiusura senza apertura
            "coda </orfano> fine",
            // tag non ammissibile
            "<1nope>x</1nope>",
            "<a b=\"c\">x</a>",
            "accenti a\u{e8}\u{f2} <a>\u{e9}</a> fine",
            "<a></a>",
            "<a><a>doppio</a></a>",
            // Le tre forme reali con attributi (mig 0532/0563/0565).
            "<todo_list version=\"{{plan_version}}\">\n{{todos}}\n</todo_list>\ncoda",
            "prosa con a < b e c > d, nessun blocco",
        ] {
            assert_eq!(Composizione::scomponi(testo).rendi(), testo, "perdita su {testo:?}");
        }
    }

    /// MUTAZIONE: sostituire l'emissione dell'interstizio con `t.trim()` fa
    /// cadere questo test col testo originale a fianco del reso.
    #[test]
    fn il_giro_e_senza_perdita_su_testi_generati() {
        // Combinatoria brutale: ogni permutazione di pezzi ambigui.
        let pezzi = ["a", "<x>", "</x>", "\n", "<y>", "</y>", " ", "<1>", "</"];
        for i in 0..pezzi.len() {
            for j in 0..pezzi.len() {
                for l in 0..pezzi.len() {
                    for m in 0..pezzi.len() {
                        let t = format!("{}{}{}{}", pezzi[i], pezzi[j], pezzi[l], pezzi[m]);
                        assert_eq!(Composizione::scomponi(&t).rendi(), t, "perdita su {t:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn una_menzione_non_e_un_blocco() {
        // Il caso che il gate del processo operativo documenta dalla 0674.
        let c = Composizione::scomponi("vedi il blocco <safety_progetto> per le regole");
        assert!(!c.ha(&k("safety_progetto")));
        assert!(c.blocchi_dichiarati().is_empty());
    }

    /// L'underscore non e' un jolly, e questo e' l'intero punto del modulo.
    #[test]
    fn l_identita_e_il_nome_del_tag_non_una_sottostringa() {
        let c = Composizione::scomponi("<prove_eseguibili>x</prove_eseguibili>");
        assert!(c.ha(&k("prove_eseguibili")));
        // Cio' che l'ILIKE del 18/08 cercava non e' una chiave: il costruttore
        // lo rifiuta invece di rispondere «zero».
        assert_eq!(ChiaveBlocco::nuova("prove eseguibili"), None);
        // E un nome che ne e' PREFISSO non risponde di si'.
        assert!(!c.ha(&k("prove")));
    }

    /// Il caso misurato: `<suggested_actions>` dentro `<next_actions>`.
    #[test]
    fn un_blocco_annidato_e_visibile() {
        let t = "<next_actions>testo<suggested_actions>x</suggested_actions></next_actions>";
        let c = Composizione::scomponi(t);
        assert!(c.ha(&k("next_actions")));
        assert!(c.ha(&k("suggested_actions")), "l'annidato deve essere visibile");
        assert_eq!(
            c.blocchi_dichiarati(),
            [k("next_actions"), k("suggested_actions")].into_iter().collect()
        );
    }

    /// L'equivalenza col comportamento che sostituisce, sul testo REALE del
    /// blocco privilegi (regola O: non una forma inventata per l'occasione).
    #[test]
    fn senza_si_comporta_come_lo_strip_testuale() {
        for t in [
            "testa\n<a>corpo</a>\ncoda",
            "prima\n<x>y</x>\ndopo",
            "<a>corpo</a>coda",
            "testa<a>corpo</a>",
            "<role>r</role>\n\n<privilegi_sistema>\nsudo apt-get\n</privilegi_sistema>\n\n<final>f</final>",
        ] {
            let nome = Composizione::scomponi(t)
                .blocchi_dichiarati()
                .into_iter()
                .next()
                .expect("almeno un blocco");
            let mut c = Composizione::scomponi(t);
            assert_eq!(c.senza(&nome), EsitoRimozione::Rimosso { occorrenze: 1 });
            assert_eq!(
                c.rendi(),
                crate::blocchi::strip_block_between(t, &nome.apertura(), &nome.chiusura()),
                "divergenza dallo strip testuale su {t:?}"
            );
        }
    }

    /// «Non c'era» e' un esito, non un silenzio: e' la meta' che mancava a
    /// `strip_block_between` e che rende invisibile una direttiva rimasta.
    #[test]
    fn una_rimozione_a_vuoto_lo_dichiara() {
        let mut c = Composizione::scomponi("testa\n<a>x</a>");
        assert_eq!(c.senza(&k("mai_visto")), EsitoRimozione::NonPresente);
        assert_eq!(c.rendi(), "testa\n<a>x</a>", "un no-op non tocca il testo");
    }

    #[test]
    fn l_innesto_e_idempotente() {
        let mut c = Composizione::scomponi("<role>r</role>");
        assert_eq!(c.appendi(&k("nuovo"), "\n<nuovo>x</nuovo>"), EsitoInnesto::Innestato);
        assert_eq!(c.rendi(), "<role>r</role>\n<nuovo>x</nuovo>");
        assert_eq!(c.appendi(&k("nuovo"), "\n<nuovo>x</nuovo>"), EsitoInnesto::GiaPresente);
        assert_eq!(c.rendi(), "<role>r</role>\n<nuovo>x</nuovo>", "nessuna copia");
    }

    /// Un testo che non porta il blocco promesso non viene innestato: appendere
    /// qualcosa che non e' il blocco dichiarato e' peggio del silenzio.
    #[test]
    fn un_innesto_che_non_porta_il_blocco_non_avviene() {
        let mut c = Composizione::scomponi("<role>r</role>");
        assert_eq!(c.appendi(&k("nuovo"), "testo qualunque"), EsitoInnesto::GiaPresente);
        assert_eq!(c.rendi(), "<role>r</role>");
    }

    /// Il caso che ha fatto rosseggiare il ponte con la 0744: un'apertura con
    /// attributi E' un blocco, e la sua resa conserva gli attributi.
    ///
    /// MUTAZIONE: far tornare `None` a `tag_di_apertura` quando dopo il nome
    /// non c'e' `>` fa cadere sia `ha` sia il ponte SQL su tre template reali.
    #[test]
    fn un_apertura_con_attributi_e_un_blocco() {
        let t = "<todo_list version=\"{{plan_version}}\">\nx\n</todo_list>\ncoda";
        let c = Composizione::scomponi(t);
        assert!(c.ha(&k("todo_list")), "{c:?}");
        assert_eq!(c.rendi(), t, "gli attributi devono sopravvivere alla resa");
        let mut c2 = Composizione::scomponi(t);
        assert_eq!(c2.senza(&k("todo_list")), EsitoRimozione::Rimosso { occorrenze: 1 });
        assert_eq!(c2.rendi(), "\ncoda");
    }

    /// La scansione degli attributi non attraversa un `<`: senza quel limite
    /// una disequazione in prosa diventerebbe un tag lungo un paragrafo.
    #[test]
    fn la_prosa_con_maggiore_e_minore_non_diventa_un_blocco() {
        let c = Composizione::scomponi("se a < b allora <x>y</x> vale");
        assert_eq!(c.blocchi_dichiarati(), [k("x")].into_iter().collect());
    }

    #[test]
    fn la_chiave_rifiuta_cio_che_non_e_un_tag() {
        assert!(ChiaveBlocco::nuova("safety_progetto").is_some());
        assert!(ChiaveBlocco::nuova("a-b").is_some());
        assert_eq!(ChiaveBlocco::nuova(""), None);
        assert_eq!(ChiaveBlocco::nuova("1a"), None);
        assert_eq!(ChiaveBlocco::nuova("a b"), None);
        assert_eq!(ChiaveBlocco::nuova("a>b"), None);
        assert_eq!(ChiaveBlocco::nuova("a%"), None);
    }

    /// Il tetto di profondita' non perde byte: e' l'unica cosa che deve reggere.
    #[test]
    fn oltre_il_tetto_di_profondita_non_si_perde_nulla() {
        let mut t = "fondo".to_string();
        for i in 0..PROFONDITA_MASSIMA + 4 {
            t = format!("<l{i}>{t}</l{i}>");
        }
        assert_eq!(Composizione::scomponi(&t).rendi(), t);
    }
}

#[cfg(test)]
mod tests_corpus {
    use super::*;
    use sqlx::PgPool;

    /// Tutti i prompt del DB migrato, nella forma in cui il runtime li serve.
    async fn corpus(db: &PgPool) -> Vec<(String, String)> {
        sqlx::query_as::<_, (String, String)>(
            "SELECT key, content FROM nexus_prompt_templates WHERE is_active = TRUE ORDER BY key",
        )
        .fetch_all(db)
        .await
        .expect("lettura dei template")
    }

    /// IL VINCOLO. Ogni prompt che il runtime puo' servire attraversa la
    /// scomposizione e ne esce IDENTICO, byte per byte.
    ///
    /// Non e' una proprieta' astratta: e' la sola cosa che rende «togli un
    /// blocco» un'operazione sul prompt invece di una riscrittura che gli
    /// somiglia. Il corpus e' quello che le migrazioni producono (regola O:
    /// nessuna fixture scritta a mano, e una migrazione che domani aggiunga un
    /// template dalla forma insolita fa fallire QUI).
    ///
    /// MISURATO anche sul META vivo il 19/08/2026 (174 righe attive, di cui 27
    /// `subagent.*` che nascono a runtime dal FigureWizard e nessuna migrazione
    /// produce): 0 righe cambiate.
    ///
    /// MUTAZIONE: in `scomponi_a`, emettere l'interstizio con `.trim()` invece
    /// che integro fa rosseggiare questo test nominando i template mutilati.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_giro_dalla_scomposizione_non_cambia_un_byte(db: PgPool) {
        let righe = corpus(&db).await;
        assert!(righe.len() > 100, "corpus troppo piccolo: {} righe", righe.len());
        let mut mutilati = Vec::new();
        for (key, content) in &righe {
            if Composizione::scomponi(content).rendi() != *content {
                mutilati.push(key.clone());
            }
        }
        assert!(
            mutilati.is_empty(),
            "la scomposizione ha cambiato {} prompt su {}: {:?}",
            mutilati.len(),
            righe.len(),
            mutilati
        );
    }

    /// PONTE fra le due implementazioni dello stesso criterio (regola O).
    ///
    /// «Questo testo dichiara il blocco X» ha una risposta in SQL
    /// (`nexus_prompt_blocchi`, mig 0744, punto unico del trigger) e una in Rust
    /// ([`Composizione::blocchi_dichiarati`]). Due implementazioni perche' SQL e
    /// Rust non si chiamano — MAI due criteri: se divergessero, il trigger
    /// rifiuterebbe scritture che il compositore considera innocue, o peggio ne
    /// lascerebbe passare di lossy.
    ///
    /// Il confronto e' sul corpus VERO, non su casi scelti: e' li' che una
    /// divergenza si manifesterebbe.
    ///
    /// MUTAZIONE: ammettere il punto nel nome di un tag in `ChiaveBlocco::nuova`
    /// (che la regexp SQL non ammette) fa rosseggiare questo test.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn rust_e_sql_riconoscono_gli_stessi_blocchi(db: PgPool) {
        let mut divergenti = Vec::new();
        for (key, content) in corpus(&db).await {
            let da_sql: std::collections::BTreeSet<ChiaveBlocco> =
                sqlx::query_scalar::<_, String>("SELECT unnest(nexus_prompt_blocchi($1))")
                    .bind(&content)
                    .fetch_all(&db)
                    .await
                    .expect("nexus_prompt_blocchi")
                    .into_iter()
                    .filter_map(|n| ChiaveBlocco::nuova(&n))
                    .collect();
            let da_rust = Composizione::scomponi(&content).blocchi_dichiarati();
            if da_sql != da_rust {
                let solo_sql: Vec<_> = da_sql.difference(&da_rust).map(|c| c.to_string()).collect();
                let solo_rust: Vec<_> =
                    da_rust.difference(&da_sql).map(|c| c.to_string()).collect();
                divergenti.push(format!("{key}: solo_sql={solo_sql:?} solo_rust={solo_rust:?}"));
            }
        }
        assert!(divergenti.is_empty(), "criteri divergenti:\n{}", divergenti.join("\n"));
    }

    /// La domanda del 18/08, posta bene, sul corpus vero.
    ///
    /// `ILIKE '%prove eseguibili%'` rispondeva **0 su 8** e quello zero era
    /// indistinguibile da «non c'e'»; su quello zero e' stato fatto
    /// implementare un mandato di correzione per un difetto inesistente. Qui la
    /// risposta e' strutturale — uguaglianza su un nome di tag — e il perimetro
    /// e' quello della mig 0742: le figure che emettono `advisory_verdict`, piu'
    /// le loro varianti dal punto unico [`nexus_types::chiavi_servibili`].
    ///
    /// Il denominatore NON e' un numero scritto a mano: il `8` di ieri sarebbe
    /// falso alla prima figura aggiunta, e una figura nuova entra qui da sola.
    ///
    /// MUTAZIONE: togliere il blocco a una riga servibile nella 0742 fa cadere
    /// questa asserzione nominando la chiave scoperta.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_domanda_del_18_08_ha_una_risposta_strutturale(db: PgPool) {
        let prove = ChiaveBlocco::nuova("prove_eseguibili").expect("chiave");
        let corpus = corpus(&db).await;
        // Le figure advisory: chi nel proprio protocollo nomina il tool con cui
        // le prove si emettono. E' il criterio della 0742, non uno nuovo.
        let figure: Vec<&String> = corpus
            .iter()
            .filter(|(k, c)| k.starts_with("subagent.") && c.contains("advisory_verdict"))
            .map(|(k, _)| k)
            .collect();
        assert!(!figure.is_empty(), "perimetro vuoto: il guard sarebbe vacuo");

        let mut servibili = 0usize;
        let mut scoperte = Vec::new();
        for base in &figure {
            for chiave in nexus_types::chiavi_servibili(base) {
                let Some((_, content)) = corpus.iter().find(|(k, _)| *k == chiave) else {
                    continue;
                };
                servibili += 1;
                if !Composizione::scomponi(content).ha(&prove) {
                    scoperte.push(chiave);
                }
            }
        }
        assert!(servibili >= figure.len(), "meno righe servibili delle figure");
        assert!(
            scoperte.is_empty(),
            "{} righe servibili su {servibili} non chiedono le prove eseguibili: {scoperte:?}",
            scoperte.len()
        );
    }

    /// La forma esatta dell'errore del 18/08, provata su questo corpus.
    ///
    /// Non e' una curiosita': dimostra che il criterio nuovo non e' «lo stesso
    /// `LIKE` scritto meglio». Con lo SPAZIO la ricerca lessicale risponde zero
    /// su righe che il blocco ce l'hanno; con l'underscore risponde bene ma per
    /// coincidenza, perche' in `LIKE` quell'underscore e' un jolly che matcha
    /// qualunque carattere.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_criterio_strutturale_non_e_un_like_scritto_meglio(db: PgPool) {
        let prove = ChiaveBlocco::nuova("prove_eseguibili").expect("chiave");
        let corpus = corpus(&db).await;
        let strutturale =
            corpus.iter().filter(|(_, c)| Composizione::scomponi(c).ha(&prove)).count();
        assert!(strutturale > 0, "il corpus deve contenere il blocco");

        let con_spazio: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM nexus_prompt_templates \
             WHERE is_active AND content ILIKE '%prove eseguibili%'",
        )
        .fetch_one(&db)
        .await
        .expect("conteggio lessicale");
        assert_eq!(
            con_spazio, 0,
            "la ricerca con lo SPAZIO deve rispondere zero: e' il falso negativo del 18/08"
        );
        // Lo zero non e' «non c'e'»: il criterio strutturale le trova.
        assert!(strutturale >= 8, "attese almeno 8 righe, trovate {strutturale}");
    }
}
