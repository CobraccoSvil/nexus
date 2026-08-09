//! Tool agente di ricerca semantica: codebase (Qdrant), recall contesto,
//! ricerca TF-IDF in-file.
//!
//! Estratto da mod.rs (refactor god-file), MIGRATI alla regola Q: l'esito sta
//! nel campo `esito` di [`RispostaTool`] e il testo resta testo.
//!
//! Il criterio che li accomuna e' quello che una ricerca sbaglia piu' spesso:
//! **non aver trovato nulla e' un SUCCESSO** — la ricerca e' stata fatta e la
//! risposta e' "niente", che per l'agente e' un'informazione su cui decidere.
//! Il fallimento e' l'altra cosa, e non le assomiglia: non aver POTUTO cercare.
//! Finche' i due casi uscivano dalla stessa porta (una stringa senza marker),
//! un indice irraggiungibile diceva all'agente "quel codice non esiste".

use nexus_agent_tools::input_contract::InputTool;
use nexus_agent_tools::tool_inputs::{
    RecallContextInput, SearchCodebaseSemanticInput, SearchFileSemanticInput, Source,
};
use nexus_types::tool_outcome::{NaturaFallimento, RispostaTool};
use nexus_types::vector_dto::VectorPointHit;
use serde_json::Value;

use super::AgentToolContext;
use crate::projects::resolve_relative_path;
use crate::vector_memory;

/// La strada alternativa quando l'indice vettoriale non e' interrogabile.
/// Fa parte del messaggio, non e' decorazione: `DelSistema` dice all'agente
/// «cambia strada», e una direttiva che non nomina la strada e' una promessa
/// non mantenuta.
const ALTERNATIVA_INDICE: &str =
    "Usa 'search_in_files' o 'find_files' per cercare nel codice con una ricerca testuale.";

/// Alternativa per il richiamo del contesto: li' non si cerca codice, si cerca
/// cio' che e' gia' stato detto o scritto, e i tool che ci arrivano sono altri.
const ALTERNATIVA_RICHIAMO: &str =
    "Rileggi i file di progetto con 'read_file' o cerca con 'search_in_files'.";

/// Soglia di pertinenza del richiamo conversazionale (invariata).
const SOGLIA_CONVERSAZIONE: f64 = 0.55;

/// Soglia di pertinenza del contesto di progetto (invariata).
const SOGLIA_PROGETTO: f64 = 0.60;

/// Byte massimi di anteprima per un hit richiamato.
const ANTEPRIMA_MAX_BYTE: usize = 1500;

/// Le chiavi da cui leggere l'ETICHETTA di un hit conversazionale.
const ETICHETTA_CONVERSAZIONE: &[&str] = &["role"];

/// Le chiavi da cui leggere il TESTO di un hit conversazionale, in ordine di
/// preferenza.
///
/// MISURATO il 09/08/2026 sulla collection viva `conversation_context`: 1194
/// punti su 1194 SENZA `text_preview`. Il produttore
/// ([`crate::vector_memory::conversation_point_payload`]) scrive il testo in
/// `content` e `text_preview` non lo ha scritto mai — quindi `content` non e' un
/// "ripiego storico", e' la sola chiave che porti il testo; `text_preview` resta
/// in coda come tolleranza verso punti di forma diversa, non come chiave attesa.
const TESTO_CONVERSAZIONE: &[&str] = &["content", "text_preview"];

/// Le chiavi da cui leggere l'ETICHETTA di un hit del contesto di progetto.
const ETICHETTA_PROGETTO: &[&str] = &["title", "section_title"];

/// Le chiavi da cui leggere il TESTO di un hit del contesto di progetto.
///
/// MISURATO lo stesso giorno sulla collection viva `project_context`: 3 punti su
/// 3 senza `text_preview`, senza `content` e senza `section_title`. L'unico
/// produttore ([`crate::projects::indexing::bootstrap_point_payload`]) scrive
/// `title` e `text`.
///
/// `section_title`/`text_preview` sono le chiavi di `project_docs`, che e'
/// un'ALTRA collection interrogata da un'altra ricerca (`search_doc_points`):
/// leggendo quelle, la sezione «Contesto progetto» usciva con l'etichetta di
/// default e l'anteprima VUOTA su OGNI hit — un richiamo dichiarato riuscito che
/// non richiamava niente. L'ordine e' lo stesso che l'altro lettore di questa
/// collection usa gia' (`nexus_builtin::docs::append_kb_hits`: `text`, poi
/// `text_preview`), perche' due letture della stessa collection non possono
/// dare due risposte diverse (regola L).
const TESTO_PROGETTO: &[&str] = &["text", "text_preview", "content"];

// ── Guardie e fallimenti condivisi ─────────────────────────────────────────

/// Le due dipendenze che una ricerca vettoriale PRETENDE. `None` = si puo'
/// cercare.
///
/// Punto unico dei due tool vettoriali di questo modulo (regola L): la domanda
/// e' la stessa, e con due copie una delle due avrebbe finito per dichiarare un
/// esito diverso dall'altra sullo stesso guasto — che e' esattamente cio' che
/// succedeva, visto che `recall_context` non riportava nemmeno QUALE delle due
/// dipendenze fosse giu'.
fn guardia_dipendenze_vettoriali(
    ctx: &AgentToolContext,
    alternativa: &str,
) -> Option<RispostaTool> {
    use std::sync::atomic::Ordering;

    let qdrant_ok = ctx.dependency_status.qdrant.load(Ordering::Relaxed);
    let embedder_ok = ctx.dependency_status.embedder.load(Ordering::Relaxed);
    if qdrant_ok && embedder_ok {
        return None;
    }
    // DEL SISTEMA: l'agente non ha nessun parametro da correggere e nessuna
    // attesa da fare — una dipendenza infrastrutturale offline non torna su
    // perche' lui ripete la chiamata.
    Some(RispostaTool::fallito_di_sistema(format!(
        "Ricerca semantica non disponibile (qdrant={}, embedder={}). {alternativa}",
        if qdrant_ok { "ok" } else { "down" },
        if embedder_ok { "ok" } else { "down" },
    )))
}

/// L'embedder non ha prodotto il vettore: senza vettore non c'e' ricerca.
///
/// DEL SISTEMA e non transitorio: l'embedder e' in-process (bridge ONNX) e le
/// sue cause — bridge non inizializzato, modello non caricato — non cambiano
/// perche' si ripete la stessa chiamata. L'errore arriva gia' appiattito in
/// `anyhow::Error`, quindi il tipo con cui distinguerle non c'e' piu' (regola
/// M): fra le due letture sbagliate possibili, questa manda a cercare un'altra
/// strada invece di far ripetere una chiamata che rifallira' identica.
fn risposta_embedder_muto(e: &anyhow::Error, alternativa: &str) -> RispostaTool {
    RispostaTool::fallito_di_sistema(format!(
        "Embedding della query non riuscito: {e}. {alternativa}"
    ))
}

/// Il punteggio di un hit come percentuale intera, per il testo.
fn percentuale(score: f64) -> u64 {
    (score * 100.0).round() as u64
}

/// Taglia il testo a `max` BYTE senza spezzare un carattere.
///
/// `&s[..max]` su uno `str` PANICA quando l'indice cade in mezzo a un carattere
/// multi-byte, e le anteprime richiamate sono prosa italiana: una lettera
/// accentata a cavallo del taglio non faceva fallire il tool, faceva morire il
/// processo. Stessa famiglia del panic gia' chiuso sulle query di ricerca
/// accentate (commit 1860dac0).
fn taglia_sicuro(testo: &str, max: usize) -> &str {
    if testo.len() <= max {
        return testo;
    }
    let mut fine = max;
    while fine > 0 && !testo.is_char_boundary(fine) {
        fine -= 1;
    }
    &testo[..fine]
}

// ── search_codebase_semantic ───────────────────────────────────────────────

/// Ricerca semantica nell'indice del codice (Qdrant).
///
/// MIGRATO al contratto d'ingresso e a `RispostaTool`. Lo zero hit e' un
/// SUCCESSO: l'indice ha risposto, e la risposta e' che non c'e' niente di
/// pertinente — cosa ben diversa dal non aver potuto interrogare l'indice, che
/// il testo raccontava con la stessa faccia.
pub(super) async fn tool_search_codebase_semantic(
    ctx: &AgentToolContext,
    input: &Value,
) -> RispostaTool {
    let params = match SearchCodebaseSemanticInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let query = params.query.trim();
    if query.is_empty() {
        return RispostaTool::fallito_rimediabile(
            "Il campo 'query' e' vuoto: scrivi in linguaggio naturale cosa cerchi nel \
             codebase (es. 'dove viene validato il token di sessione').",
        );
    }
    if let Some(risposta) = guardia_dipendenze_vettoriali(ctx, ALTERNATIVA_INDICE) {
        return risposta;
    }
    // `clamp` e non `min`, e l'estremo che cambia qualcosa e' quello BASSO. Il
    // lettore precedente era `as_u64`, che su un intero NEGATIVO ritorna `None`:
    // un `limit: -5` ricadeva nel default 8, cioe' il modello credeva di aver
    // ristretto e riceveva altro, senza che nulla glielo dicesse. Il contratto
    // porta ora un `i64`, e un `as usize` senza estremo inferiore lo
    // trasformerebbe in un limite enorme — la stessa svista dall'altra parte.
    // Lo ZERO invece era gia' innocuo: `vector_memory::search_code_index` lo
    // rialza con `limit.max(1)` prima di comporre la richiesta a Qdrant, quindi
    // l'estremo basso qui e' una difesa nel punto in cui il valore ENTRA, non la
    // correzione di un difetto osservato.
    let limit = params.limit.unwrap_or(8).clamp(1, 20) as usize;

    let embedding = match ctx.neural.embed_text("", query).await {
        Ok(v) => v,
        Err(e) => return risposta_embedder_muto(&e, ALTERNATIVA_INDICE),
    };
    let hits =
        match vector_memory::search_code_index(&ctx.db, &embedding, ctx.project_id, limit).await {
            Ok(h) => h,
            // La guardia sopra dichiarava Qdrant su: se la ricerca fallisce
            // ugualmente, il guasto e' dell'indice e non della richiesta.
            Err(e) => {
                return RispostaTool::fallito_di_sistema(format!(
                    "Ricerca nell'indice del codice fallita: {e}. {ALTERNATIVA_INDICE}"
                ))
            }
        };
    if hits.is_empty() {
        return RispostaTool::riuscito(format!(
            "Nessun risultato per '{query}'. Il codebase potrebbe non essere ancora \
             indicizzato: {ALTERNATIVA_INDICE}"
        ));
    }
    let risultati: Vec<String> = hits
        .iter()
        .enumerate()
        .map(|(i, hit)| riga_risultato_codice(i, hit))
        .collect();
    RispostaTool::riuscito(format!(
        "Risultati per '{query}':\n\n{}",
        risultati.join("\n\n")
    ))
}

/// Una riga di risultato dell'indice codice, composta DAI campi del payload.
fn riga_risultato_codice(indice: usize, hit: &VectorPointHit) -> String {
    let file = hit
        .payload
        .get("file_path")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let chunk = hit
        .payload
        .get("chunk_index")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let labels = hit
        .payload
        .get("ui_labels")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let mut parti = vec![format!(
        "{}. {} (score: {}%)",
        indice + 1,
        file,
        percentuale(hit.score)
    )];
    if !labels.is_empty() {
        parti.push(format!("   Label UI: {labels}"));
    }
    if chunk > 0 {
        parti.push(format!("   Chunk: {chunk}"));
    }
    parti.join("\n")
}

// ── recall_context ─────────────────────────────────────────────────────────

/// Cio' che UNA delle due ricerche di `recall_context` ha prodotto.
///
/// Quattro casi e non due. «Non ho trovato niente», «non ho potuto cercare» e
/// «non c'era niente da interrogare» portano a decisioni opposte, e collassarli
/// e' precisamente il difetto che questa migrazione chiude: l'errore della
/// ricerca finiva in un `tracing::warn` e all'agente arrivava «nessun contesto
/// rilevante trovato», cioe' una risposta sulla PERTINENZA dove non c'era stata
/// nessuna ricerca.
enum EsitoRicerca {
    /// La ricerca ha prodotto una sezione di testo gia' composta.
    Trovato(String),
    /// La ricerca e' stata fatta e non ha trovato nulla: e' un successo.
    Vuoto,
    /// Non c'era niente da interrogare (nessuna sessione: fuori da una chat non
    /// esiste una conversazione da richiamare). Non e' un fallimento e non e'
    /// un vuoto: e' una domanda che non si poteva porre. Il messaggio dice
    /// quale, perche' una fonte muta senza nome non e' dichiarabile.
    NonInterrogabile(String),
    /// La ricerca non ha potuto rispondere. Il messaggio dice quale.
    Fallita(String),
}

/// Richiama il contesto pertinente da conversazione e/o documentazione di
/// progetto.
///
/// MIGRATO al contratto d'ingresso e a `RispostaTool`. `source` e' un ENUM del
/// contratto: prima era una stringa libera confrontata con tre letterali, e un
/// valore fuori vocabolario (`"chat"`, `"tutto"`) non cercava da nessuna parte
/// e usciva come «nessun contesto rilevante».
pub(super) async fn tool_recall_context(ctx: &AgentToolContext, input: &Value) -> RispostaTool {
    let params = match RecallContextInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let query = params.query.trim();
    if query.is_empty() {
        return RispostaTool::fallito_rimediabile(
            "Il campo 'query' e' vuoto: descrivi cosa vuoi richiamare (es. 'errore di \
             autenticazione discusso prima').",
        );
    }
    if let Some(risposta) = guardia_dipendenze_vettoriali(ctx, ALTERNATIVA_RICHIAMO) {
        return risposta;
    }
    let limit = params.limit.unwrap_or(5).clamp(1, 10) as u64;
    let embedding = match ctx.neural.embed_text("", query).await {
        Ok(v) => v,
        Err(e) => return risposta_embedder_muto(&e, ALTERNATIVA_RICHIAMO),
    };

    let source = params.source.unwrap_or(Source::All);
    let mut esiti: Vec<EsitoRicerca> = Vec::new();
    if matches!(source, Source::Conversation | Source::All) {
        esiti.push(richiama_conversazione(ctx, &embedding, limit).await);
    }
    if matches!(source, Source::Project | Source::All) {
        esiti.push(richiama_progetto(ctx, &embedding, limit).await);
    }
    componi_richiamo(query, esiti)
}

/// Il contesto conversazionale della sessione corrente.
async fn richiama_conversazione(
    ctx: &AgentToolContext,
    embedding: &[f32],
    limit: u64,
) -> EsitoRicerca {
    let Some(sid) = ctx.session_id else {
        return EsitoRicerca::NonInterrogabile(
            "contesto conversazionale (nessuna sessione attiva)".to_string(),
        );
    };
    match vector_memory::search_conversation_context(
        &ctx.db,
        embedding,
        sid,
        limit,
        SOGLIA_CONVERSAZIONE,
    )
    .await
    {
        Ok(hits) if hits.is_empty() => EsitoRicerca::Vuoto,
        Ok(hits) => EsitoRicerca::Trovato(componi_sezione(
            "--- Contesto conversazionale ---",
            &hits,
            ETICHETTA_CONVERSAZIONE,
            "?",
            TESTO_CONVERSAZIONE,
        )),
        Err(e) => EsitoRicerca::Fallita(format!("contesto conversazionale ({e})")),
    }
}

/// Il contesto e la documentazione del progetto.
async fn richiama_progetto(ctx: &AgentToolContext, embedding: &[f32], limit: u64) -> EsitoRicerca {
    match vector_memory::search_project_context_points(
        &ctx.db,
        embedding,
        ctx.project_id,
        limit,
        SOGLIA_PROGETTO,
    )
    .await
    {
        Ok(hits) if hits.is_empty() => EsitoRicerca::Vuoto,
        Ok(hits) => EsitoRicerca::Trovato(componi_sezione(
            "--- Contesto progetto ---",
            &hits,
            ETICHETTA_PROGETTO,
            "Contesto progetto",
            TESTO_PROGETTO,
        )),
        Err(e) => EsitoRicerca::Fallita(format!("contesto progetto ({e})")),
    }
}

/// Il primo dei campi indicati che esista e porti testo.
///
/// Le chiavi sono ORDINATE e dichiarate dal chiamante perche' le due collection
/// non hanno lo stesso payload: leggerne una a caso e' il modo in cui la sezione
/// «Contesto progetto» e' rimasta muta.
fn primo_campo<'a>(payload: &'a Value, chiavi: &[&str]) -> Option<&'a str> {
    // Il filtro sul VUOTO sta dentro la scansione, non dopo: una chiave presente
    // ma vuota deve far proseguire all'alternativa, non fermare la ricerca —
    // altrimenti un `text` vuoto nasconderebbe il `text_preview` che c'e'.
    chiavi.iter().find_map(|chiave| {
        payload
            .get(*chiave)
            .and_then(Value::as_str)
            .filter(|testo| !testo.is_empty())
    })
}

/// Compone una sezione di hit richiamati. Le chiavi di payload da cui leggere
/// etichetta e testo le porta la fonte (vedi [`TESTO_CONVERSAZIONE`] e
/// [`TESTO_PROGETTO`]): il resto della resa e' identico, e con due copie sarebbe
/// divergito.
fn componi_sezione(
    titolo: &str,
    hits: &[VectorPointHit],
    chiavi_etichetta: &[&str],
    etichetta_default: &str,
    chiavi_testo: &[&str],
) -> String {
    let righe: Vec<String> = hits
        .iter()
        .enumerate()
        .map(|(i, hit)| {
            let etichetta =
                primo_campo(&hit.payload, chiavi_etichetta).unwrap_or(etichetta_default);
            let anteprima = primo_campo(&hit.payload, chiavi_testo).unwrap_or("");
            format!(
                "{}. {} (pertinenza: {}%)\n{}",
                i + 1,
                etichetta,
                percentuale(hit.score),
                taglia_sicuro(anteprima, ANTEPRIMA_MAX_BYTE)
            )
        })
        .collect();
    format!("{titolo}\n{}", righe.join("\n\n"))
}

/// Il verdetto sul richiamo, dagli esiti delle ricerche interrogate.
///
/// Il caso che cambia e' quello di mezzo: se NESSUNA ricerca ha risposto e
/// almeno una e' fallita, il tool non sa se il contesto esista — e dirlo e'
/// l'unica risposta onesta. Con un hit trovato, invece, una fonte muta resta un
/// successo (i risultati ci sono) e il testo dichiara che il richiamo e'
/// parziale.
///
/// Il vuoto vero e' l'ULTIMO caso, e ci si arriva solo se qualcosa e' stato
/// davvero interrogato: se tutte le fonti richieste erano non interrogabili, non
/// c'e' stata nessuna ricerca, e rispondere «nessun contesto rilevante» sarebbe
/// di nuovo un'affermazione sulla PERTINENZA al posto di una sulla RICERCA — il
/// difetto che questo modulo esiste per chiudere, nell'unica forma che era
/// rimasta aperta.
fn componi_richiamo(query: &str, esiti: Vec<EsitoRicerca>) -> RispostaTool {
    let interrogate = esiti.len();
    let mut sezioni: Vec<String> = Vec::new();
    let mut fallite: Vec<String> = Vec::new();
    let mut mute: Vec<String> = Vec::new();
    for esito in esiti {
        match esito {
            EsitoRicerca::Trovato(sezione) => sezioni.push(sezione),
            EsitoRicerca::Vuoto => {}
            EsitoRicerca::NonInterrogabile(motivo) => mute.push(motivo),
            EsitoRicerca::Fallita(motivo) => fallite.push(motivo),
        }
    }
    let non_risposte: Vec<&str> = fallite
        .iter()
        .chain(mute.iter())
        .map(String::as_str)
        .collect();
    if !sezioni.is_empty() {
        let mut testo = format!(
            "Contesto recuperato per '{query}':\n\n{}",
            sezioni.join("\n\n")
        );
        if !non_risposte.is_empty() {
            testo.push_str(&format!(
                "\n\n(richiamo PARZIALE: non interrogabile {})",
                non_risposte.join("; ")
            ));
        }
        return RispostaTool::riuscito(testo);
    }
    if !fallite.is_empty() {
        return RispostaTool::fallito_di_sistema(format!(
            "Richiamo del contesto non riuscito: non interrogabile {}. Questo NON \
             significa che il contesto non esista, significa che non e' stato possibile \
             cercarlo: {ALTERNATIVA_RICHIAMO}",
            fallite.join("; ")
        ));
    }
    if mute.len() == interrogate && interrogate > 0 {
        // Nessuna ricerca fatta: l'agente lo puo' correggere scegliendo una
        // fonte che esista, e il messaggio nomina quella che non esisteva.
        return RispostaTool::fallito_rimediabile(format!(
            "Nessuna delle fonti richieste era interrogabile: {}. Non e' un'assenza di \
             contesto — nessuna ricerca e' stata eseguita: richiama 'recall_context' con \
             source='project', oppure {ALTERNATIVA_RICHIAMO}",
            mute.join("; ")
        ));
    }
    let mut testo = format!(
        "Nessun contesto rilevante trovato per '{query}'. La conversazione potrebbe non \
         essere ancora indicizzata o la query potrebbe essere troppo specifica."
    );
    if !mute.is_empty() {
        testo.push_str(&format!(
            "\n\n(ricerca PARZIALE: non interrogabile {})",
            mute.join("; ")
        ));
    }
    RispostaTool::riuscito(testo)
}

// ── search_file_semantic ───────────────────────────────────────────────────

/// Un blocco di righe del file col suo punteggio rispetto alla query.
struct ChunkScorato {
    /// Prima riga del blocco, 1-based.
    start_line: usize,
    /// Ultima riga del blocco, 1-based.
    end_line: usize,
    score: f32,
    text: String,
}

/// Ricerca semantica TF-IDF in-process all'interno di un singolo file.
/// Divide il file in chunk sovrapposti, scorea ogni chunk vs query e
/// restituisce le sezioni piu' rilevanti con i numeri di riga.
///
/// MIGRATO al contratto d'ingresso e a `RispostaTool`. Un file VUOTO e' un
/// successo con zero sezioni (il file esiste, non ha righe da cercare); un file
/// che non si riesce a leggere e' un fallimento, e la sua natura viene dal
/// `ErrorKind` (regola M) invece che dal messaggio del sistema operativo, che e'
/// localizzato e diverso fra Windows e Linux.
pub(super) async fn tool_search_file_semantic(
    ctx: &AgentToolContext,
    input: &Value,
) -> RispostaTool {
    let params = match SearchFileSemanticInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let path_str = params.path.trim();
    let query = params.query.trim();
    if path_str.is_empty() {
        return RispostaTool::fallito_rimediabile(
            "Il campo 'path' e' vuoto: indica il file da analizzare, relativo alla root \
             del progetto o assoluto.",
        );
    }
    if query.is_empty() {
        return RispostaTool::fallito_rimediabile(
            "Il campo 'query' e' vuoto: descrivi cosa cerchi dentro il file.",
        );
    }
    let top_k = params.top_k.unwrap_or(5).clamp(1, 10) as usize;
    let chunk_lines = params.chunk_lines.unwrap_or(50).clamp(10, 200) as usize;

    let target = match risolvi_percorso(ctx, path_str) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let content = match tokio::fs::read_to_string(&target).await {
        Ok(c) => c,
        Err(e) => {
            return RispostaTool::fallito(format!("Lettura di '{path_str}' non riuscita: {e}"))
                .con_natura(NaturaFallimento::da_errore_io(&e))
        }
    };
    cerca_nel_contenuto(path_str, query, &content, top_k, chunk_lines)
}

/// Risolve il percorso del file: assoluto com'e', relativo dentro la root.
fn risolvi_percorso(
    ctx: &AgentToolContext,
    path_str: &str,
) -> Result<std::path::PathBuf, RispostaTool> {
    if std::path::Path::new(path_str).is_absolute() {
        return Ok(std::path::PathBuf::from(path_str));
    }
    resolve_relative_path(&ctx.root_path, path_str).map_err(|e| {
        // Un percorso sbagliato l'agente lo puo' correggere, e il messaggio
        // porta la causa che `resolve_relative_path` ha gia' composto (fuori
        // root, non esiste, caratteri invalidi).
        RispostaTool::fallito_rimediabile(format!(
            "Percorso '{path_str}' non risolvibile: {}",
            e.1["error"].as_str().unwrap_or("path error")
        ))
    })
}

/// La ricerca vera e propria, separata dall'I/O perche' e' pura: stesso
/// contenuto, stesso esito, provabile senza toccare il filesystem.
fn cerca_nel_contenuto(
    path_str: &str,
    query: &str,
    content: &str,
    top_k: usize,
    chunk_lines: usize,
) -> RispostaTool {
    let all_lines: Vec<&str> = content.lines().collect();
    if all_lines.is_empty() {
        // Il file esiste e non ha righe: la ricerca e' RIUSCITA con zero
        // sezioni, come una directory vuota elencata.
        return RispostaTool::riuscito(format!(
            "Il file '{path_str}' e' vuoto: nessuna sezione da cercare."
        ));
    }
    let query_tokens = tokenizza_query(query);
    if query_tokens.is_empty() {
        return RispostaTool::fallito_rimediabile(format!(
            "La query '{query}' non contiene termini di ricerca utilizzabili: servono \
             parole di almeno 2 caratteri alfanumerici."
        ));
    }
    let chunks = costruisci_chunk(&all_lines, chunk_lines, &query_tokens);
    if chunks.is_empty() {
        // Ramo che il file non-vuoto non dovrebbe raggiungere: se lo raggiunge,
        // e' il chunker a non aver prodotto nulla, e non c'e' parametro che
        // l'agente possa cambiare per rimediare.
        return RispostaTool::fallito_di_sistema(format!(
            "Nessun blocco prodotto per '{path_str}' ({} righe): ricerca non eseguita.",
            all_lines.len()
        ));
    }
    let scelti = seleziona_non_sovrapposti(chunks, top_k);
    RispostaTool::riuscito(componi_sezioni_file(
        path_str,
        query,
        all_lines.len(),
        &scelti,
    ))
}

/// Tokenizza la query: lowercase, split su non-alfanumerici, filtra token brevi.
fn tokenizza_query(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.len() >= 2)
        .map(str::to_string)
        .collect()
}

/// Costruisce i chunk sovrapposti e ne calcola il punteggio.
fn costruisci_chunk(
    all_lines: &[&str],
    chunk_lines: usize,
    query_tokens: &[String],
) -> Vec<ChunkScorato> {
    let total_lines = all_lines.len();
    // Overlap: 20% del chunk_lines per non perdere contesto ai bordi.
    let overlap = (chunk_lines / 5).max(5);
    let step = chunk_lines.saturating_sub(overlap).max(1);

    let mut chunks: Vec<ChunkScorato> = Vec::new();
    let mut inizio = 0usize;
    while inizio < total_lines {
        let fine = (inizio + chunk_lines).min(total_lines);
        let testo = all_lines[inizio..fine].join("\n");
        let score = punteggio_chunk(
            &testo,
            query_tokens,
            total_lines,
            chunks.len(),
            fine - inizio,
        );
        chunks.push(ChunkScorato {
            start_line: inizio + 1,
            end_line: fine,
            score,
            text: testo,
        });
        inizio += step;
    }
    chunks
}

/// Score = somma pesata delle occorrenze dei token della query, normalizzata
/// per densita' (token utili per riga) per penalizzare i chunk quasi vuoti.
fn punteggio_chunk(
    testo: &str,
    query_tokens: &[String],
    total_lines: usize,
    gia_prodotti: usize,
    righe: usize,
) -> f32 {
    let minuscolo = testo.to_lowercase();
    let parole = minuscolo
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| !t.is_empty())
        .count()
        .max(1) as f32;

    let mut grezzo = 0.0f32;
    for token in query_tokens {
        let occorrenze = minuscolo.matches(token.as_str()).count() as f32;
        if occorrenze > 0.0 {
            // TF puro, log-normalizzato per ridurre l'influenza dei token ripetuti.
            grezzo += (1.0 + occorrenze.ln())
                * (total_lines as f32 / (gia_prodotti + 1).max(1) as f32)
                    .ln()
                    .max(1.0);
        }
    }
    let densita = (parole / righe.max(1) as f32).min(2.0);
    grezzo * densita
}

/// Ordina per punteggio, scarta i chunk che si sovrappongono troppo a uno gia'
/// scelto, e restituisce i primi `top_k` nell'ordine naturale del file.
fn seleziona_non_sovrapposti(mut chunks: Vec<ChunkScorato>, top_k: usize) -> Vec<ChunkScorato> {
    chunks.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut scelti: Vec<ChunkScorato> = Vec::new();
    for chunk in chunks {
        if scelti.len() >= top_k {
            break;
        }
        if scelti.iter().any(|gia| sovrappone_troppo(&chunk, gia)) {
            continue;
        }
        scelti.push(chunk);
    }
    // Ri-ordina i selezionati per numero di riga (ordine naturale del file).
    scelti.sort_by_key(|c| c.start_line);
    scelti
}

/// Due chunk si sovrappongono "troppo" quando l'intersezione copre piu' della
/// meta' del piu' corto: sotto quella soglia le due sezioni portano ancora
/// informazione distinta.
fn sovrappone_troppo(a: &ChunkScorato, b: &ChunkScorato) -> bool {
    let inizio = a.start_line.max(b.start_line);
    let fine = a.end_line.min(b.end_line);
    if inizio > fine {
        return false;
    }
    let lunghezza = fine - inizio + 1;
    let minima = (a.end_line - a.start_line + 1).min(b.end_line - b.start_line + 1);
    lunghezza * 2 > minima
}

/// Compone il testo finale: intestazione col totale righe, poi le sezioni.
fn componi_sezioni_file(
    path_str: &str,
    query: &str,
    total_lines: usize,
    scelti: &[ChunkScorato],
) -> String {
    let intestazione = format!(
        "File: {} ({} righe totali) — {} sezioni rilevanti per '{}'\n",
        path_str,
        total_lines,
        scelti.len(),
        query
    );
    let sezioni: Vec<String> = scelti
        .iter()
        .map(|c| {
            format!(
                "── Righe {}-{} (score: {:.0}) ──\n{}",
                c.start_line, c.end_line, c.score, c.text
            )
        })
        .collect();
    format!("{}\n{}", intestazione, sezioni.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_types::tool_outcome::EsitoTool;

    // I test attraversano `cerca_nel_contenuto`, che e' il punto in cui la
    // ricerca in-file DECIDE (regola O): l'unica cosa che resta fuori e' la
    // lettura del file, che non e' l'oggetto della misura.

    #[test]
    fn file_vuoto_e_un_successo_non_un_errore() {
        let out = cerca_nel_contenuto("vuoto.txt", "qualcosa", "", 5, 50);
        assert_eq!(out.esito, EsitoTool::Riuscito, "{out:?}");
        assert!(out.testo.contains("vuoto"), "{out:?}");
    }

    #[test]
    fn nessuna_corrispondenza_e_un_successo() {
        // Il file ha righe e la query ha token validi: la ricerca viene fatta e
        // non trova nulla di pertinente. E' un esito, non un guasto.
        let contenuto = "alpha\nbeta\ngamma\n";
        let out = cerca_nel_contenuto("f.txt", "zeta omega", contenuto, 5, 50);
        assert_eq!(out.esito, EsitoTool::Riuscito, "{out:?}");
    }

    #[test]
    fn query_senza_termini_utili_e_rimediabile() {
        let out = cerca_nel_contenuto("f.txt", "a !", "riga\n", 5, 50);
        assert_eq!(out.esito, EsitoTool::Fallito, "{out:?}");
        assert_eq!(out.natura, Some(NaturaFallimento::Rimediabile), "{out:?}");
        // Rimediabile obbliga il testo a dire COME: qui il criterio e' la
        // lunghezza minima del termine.
        assert!(out.testo.contains("2 caratteri"), "{out:?}");
    }

    #[test]
    fn le_sezioni_escono_in_ordine_di_riga() {
        let contenuto: String = (1..=200)
            .map(|i| {
                if i == 10 || i == 150 {
                    format!("bersaglio riga {i}\n")
                } else {
                    format!("riempitivo {i}\n")
                }
            })
            .collect();
        let out = cerca_nel_contenuto("f.txt", "bersaglio", &contenuto, 5, 20);
        assert_eq!(out.esito, EsitoTool::Riuscito, "{out:?}");
        let prima = out.testo.find("riga 10").expect("prima sezione");
        let dopo = out.testo.find("riga 150").expect("seconda sezione");
        assert!(prima < dopo, "{out:?}");
    }

    #[test]
    fn anteprima_non_spezza_un_carattere_accentato() {
        // Il carattere combinante occupa i byte 10 e 11: tagliare a 11 cade nel
        // MEZZO, ed e' li' che `&s[..max]` panicava. Rimettendo lo slice nudo,
        // questa riga non fallisce — abortisce il processo di test.
        let testo = format!("{}e\u{300}coda", "x".repeat(9));
        assert_eq!(taglia_sicuro(&testo, 11), "xxxxxxxxxe");
        assert_eq!(taglia_sicuro(&testo, 10), "xxxxxxxxxe");
        assert_eq!(taglia_sicuro("corto", 10), "corto");
    }

    #[test]
    fn una_fonte_muta_senza_risultati_e_un_fallimento() {
        let out = componi_richiamo(
            "q",
            vec![
                EsitoRicerca::Fallita("contesto progetto (qdrant giu')".to_string()),
                EsitoRicerca::NonInterrogabile("conversazione (nessuna sessione)".to_string()),
            ],
        );
        assert_eq!(out.esito, EsitoTool::Fallito, "{out:?}");
        assert_eq!(out.natura, Some(NaturaFallimento::DelSistema), "{out:?}");
    }

    #[test]
    fn nessuna_fonte_interrogabile_non_e_un_vuoto() {
        // `source='conversation'` fuori da una chat: non e' stata fatta nessuna
        // ricerca, quindi rispondere «nessun contesto rilevante» affermerebbe
        // qualcosa sulla PERTINENZA che nessuno ha misurato.
        let out = componi_richiamo(
            "q",
            vec![EsitoRicerca::NonInterrogabile(
                "contesto conversazionale (nessuna sessione attiva)".to_string(),
            )],
        );
        assert_eq!(out.esito, EsitoTool::Fallito, "{out:?}");
        assert_eq!(out.natura, Some(NaturaFallimento::Rimediabile), "{out:?}");
        // Rimediabile obbliga il testo a dire COME.
        assert!(out.testo.contains("source='project'"), "{out:?}");
        assert!(!out.testo.contains("Nessun contesto rilevante"), "{out:?}");
    }

    #[test]
    fn una_fonte_muta_accanto_a_un_vuoto_lo_dichiara() {
        let out = componi_richiamo(
            "q",
            vec![
                EsitoRicerca::Vuoto,
                EsitoRicerca::NonInterrogabile("contesto conversazionale (x)".to_string()),
            ],
        );
        assert_eq!(out.esito, EsitoTool::Riuscito, "{out:?}");
        assert!(out.testo.contains("PARZIALE"), "{out:?}");
    }

    /// Le anteprime del richiamo si provano contro i PRODUTTORI dei payload
    /// (regola O): riscrivere qui un payload a mano fisserebbe l'assunto da
    /// verificare, ed e' precisamente cosi' che le chiavi sbagliate sono
    /// sopravvissute — il lettore chiedeva `section_title`/`text_preview`, che
    /// sono di `project_docs`, mentre `search_project_context_points` interroga
    /// `project_context`.
    ///
    /// MUTAZIONE: riportando [`TESTO_PROGETTO`] a `["text_preview", "content"]`
    /// o [`ETICHETTA_PROGETTO`] a `["section_title"]`, questo test rosseggia col
    /// valore del difetto reale — anteprima vuota ed etichetta di default.
    #[test]
    fn la_sezione_progetto_mostra_il_testo_che_il_produttore_ha_scritto() {
        let payload = crate::projects::indexing::bootstrap_point_payload(
            uuid::Uuid::nil(),
            "summary",
            "Project Summary",
            "Il backend espone /api/corsi",
        );
        let hit = VectorPointHit {
            point_id: "p1".to_string(),
            score: 0.8,
            payload,
        };
        let sezione = componi_sezione(
            "--- Contesto progetto ---",
            std::slice::from_ref(&hit),
            ETICHETTA_PROGETTO,
            "Contesto progetto",
            TESTO_PROGETTO,
        );
        assert!(sezione.contains("Il backend espone /api/corsi"), "{sezione}");
        assert!(sezione.contains("Project Summary"), "{sezione}");
    }

    /// Gemello del precedente sull'altra collection.
    ///
    /// MUTAZIONE: togliendo `"content"` da [`TESTO_CONVERSAZIONE`] la riga esce
    /// senza testo — che e' cio' che accadeva al contesto di progetto, dove la
    /// chiave giusta non era in elenco affatto.
    #[test]
    fn la_sezione_conversazione_mostra_il_testo_che_il_produttore_ha_scritto() {
        let payload = crate::vector_memory::conversation_point_payload(
            uuid::Uuid::nil(),
            "assistant",
            "Ho corretto il proxy del frontend",
            "2026-08-09T10:00:00Z",
        );
        let hit = VectorPointHit {
            point_id: "p2".to_string(),
            score: 0.9,
            payload,
        };
        let sezione = componi_sezione(
            "--- Contesto conversazionale ---",
            std::slice::from_ref(&hit),
            ETICHETTA_CONVERSAZIONE,
            "?",
            TESTO_CONVERSAZIONE,
        );
        assert!(
            sezione.contains("Ho corretto il proxy del frontend"),
            "{sezione}"
        );
        assert!(sezione.contains("assistant"), "{sezione}");
    }

    #[test]
    fn una_fonte_muta_con_risultati_resta_un_successo_dichiarato_parziale() {
        let out = componi_richiamo(
            "q",
            vec![
                EsitoRicerca::Trovato("--- Contesto progetto ---\n1. x".to_string()),
                EsitoRicerca::Fallita("contesto conversazionale (timeout)".to_string()),
            ],
        );
        assert_eq!(out.esito, EsitoTool::Riuscito, "{out:?}");
        assert!(out.testo.contains("PARZIALE"), "{out:?}");
    }

    #[test]
    fn nessun_contesto_trovato_senza_guasti_e_un_successo() {
        let out = componi_richiamo("q", vec![EsitoRicerca::Vuoto, EsitoRicerca::Vuoto]);
        assert_eq!(out.esito, EsitoTool::Riuscito, "{out:?}");
        assert!(out.testo.contains("Nessun contesto rilevante"), "{out:?}");
    }
}
