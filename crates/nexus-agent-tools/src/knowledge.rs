//! MCP tools per la Knowledge Base per-progetto.
//!
//! ADR 0017 v2 TODO 2 — Reimplementazione 1:1 dei 9 tool `knowledge_*` sul nuovo
//! schema unificato (`wiki_docs` + `wiki_links` + `wiki_concept_triples`).
//!
//! Le firme pubbliche restano stabili: i tool sono esposti via
//! `NexusToolCatalog` e gli agenti AI in produzione si aspettano i campi
//! documentati in `agent_tools::tool_schema` (es. `note_id`, `intent`,
//! `rel_type`, `outgoing`, `incoming`). Solo le implementazioni interne sono
//! state riscritte: le query SQL puntano alle nuove tabelle, gli embedding
//! usano la collection Qdrant `wiki_content`, e gli scope sono sempre
//! `WikiScope::Project` con `project_id = ctx.project_id`.
//!
//! Mapping concettuale vecchio -> nuovo:
//!   - `project_knowledge_notes`       -> `wiki_docs` (scope='project')
//!   - `project_knowledge_links`       -> `wiki_links`
//!   - `note.off_topic = true`         -> `wiki_docs.edit_lock = 'frozen'`
//!   - `note.intent`                   -> `wiki_docs.intent` (campo legacy preservato)
//!   - `note.kind = 'code_doc'`        -> `wiki_docs.kind = 'code_doc'`
//!   - `rel_type` mapping (note->link):
//!       followup     -> followup
//!       correction   -> correction_of
//!       refinement   -> refines
//!       duplicate    -> duplicate_of
//!       blocks       -> blocks
//!       blocked_by   -> blocked_by
//!       relates      -> relates

use nexus_types::tool_outcome::{NaturaFallimento, RispostaTool};
use crate::context_core::ToolContextCore;
// Il solo contratto che compare in una FIRMA e non dentro il corpo di un
// handler: il sottografo legge i propri parametri in due passi (dimensioni e
// seed), e passarli avanti come `Value` significherebbe rileggere l'input due
// volte con due letture diverse.
use crate::tool_inputs::KnowledgeGetSubgraphInput;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

/// rel_type esposti agli agenti (schema stabile). Lo stesso set della vecchia
/// `knowledge.rel_type`: gli agenti gia' deployati ne dipendono.
pub const KNOWLEDGE_REL_TYPES: [&str; 7] = [
    "followup",
    "correction",
    "refinement",
    "duplicate",
    "blocks",
    "blocked_by",
    "relates",
];

/// Traduce il rel_type "agente-facing" verso il vocabolario di `wiki_links`.
fn map_rel_to_wiki(rel: &str) -> &'static str {
    match rel {
        "followup" => "followup",
        "correction" => "correction_of",
        "refinement" => "refines",
        "duplicate" => "duplicate_of",
        "blocks" => "blocks",
        "blocked_by" => "blocked_by",
        "relates" => "relates",
        _ => "relates",
    }
}

/// Traduzione inversa (storage -> esposizione agente). Sconosciuti restano
/// inalterati (es. `mentions`, `implements`, `tests` — emessi dai worker auto)
/// in modo che l'agente vede anche le relazioni nuove dell'ADR 0017 v2.
fn map_rel_from_wiki(rel: &str) -> String {
    match rel {
        "correction_of" => "correction".to_string(),
        "refines" => "refinement".to_string(),
        "duplicate_of" => "duplicate".to_string(),
        other => other.to_string(),
    }
}

/// Filtro Qdrant standard per i doc del progetto corrente (scope + project_id).
/// Punto unico del payload di filtro riusato da search e subgraph.
///
/// La CHIAVE e il VALORE dello scope vengono dal wiki, non da due letterali:
/// `wiki_content` porta meta e progetto insieme, e un terzo lettore che
/// scrivesse `"scope"`/`"project"` per conto proprio sarebbe la divergenza di
/// domani. Il RAG di mcp-core costruisce lo stesso filtro dagli stessi due
/// punti (`rag::collezioni`).
fn project_qdrant_filter(project_id: Uuid) -> Value {
    json!({
        "must": [
            {
                "key": nexus_wiki::content_points::CHIAVE_SCOPE,
                "match": { "value": nexus_wiki::model::WikiScope::Project.as_str() }
            },
            { "key": "project_id", "match": { "value": project_id.to_string() } }
        ]
    })
}

/// Il fallimento da dichiarare quando il meta-DB non ha risposto.
///
/// PUNTO UNICO (regola L) del messaggio per gli handler di questo file: la
/// domanda «il DB non ha risposto» ha una sola risposta possibile per l'agente,
/// che e' nessuna — non c'e' parametro da correggere e ripetere la chiamata non
/// cambia l'esito, quindi la natura e' sempre [`NaturaFallimento::DelSistema`].
///
/// Esiste perche' fino a ieri quell'evento non produceva un fallimento affatto:
/// nove helper di questo modulo lo appiattivano su un `Vec` vuoto, un `false` o
/// un `None`, e l'handler consegnava all'agente un'assenza INVENTATA — un
/// sottografo senza nodi, una nota «non trovata nel progetto», un import con
/// zero archi creati. Un'assenza ha l'aria di un dato, e su un dato si decide.
fn errore_db(operazione: &str, e: sqlx::Error) -> RispostaTool {
    crate::errore_tool(format!("{operazione}: {e}"), NaturaFallimento::DelSistema)
}

/// Il fallimento da dichiarare quando l'indice vettoriale non ha risposto.
///
/// Distinto da [`errore_db`] nel messaggio e non nella natura: sono due sistemi
/// diversi, e chi legge il resoconto deve sapere QUALE dei due e' muto.
fn errore_indice(e: impl std::fmt::Display) -> RispostaTool {
    crate::errore_tool(
        format!("ricerca sull'indice vettoriale fallita: {e}"),
        NaturaFallimento::DelSistema,
    )
}

/// Il fallimento da dichiarare quando l'embedder non ha prodotto il vettore.
///
/// TRANSITORIO: l'embedder e' un servizio esterno raggiunto via gateway, e le
/// sue cause tipiche (fornitore saturo, timeout di rete) si risolvono da sole —
/// ripetere la STESSA chiamata e' qui la strategia corretta.
fn errore_embedding(cosa: &str, e: impl std::fmt::Display) -> RispostaTool {
    crate::errore_tool(
        format!("embedding {cosa} fallito: {e}"),
        NaturaFallimento::Transitorio,
    )
}

/// Byte massimi di testo consegnati all'embedder.
const EMBED_MAX_BYTES: usize = 2000;

/// Tronca il testo da embeddare al limite senza copiarlo se corto.
///
/// PANIC CHIUSO. Era `&text[..2000]`, e in Rust affettare uno `&str` a un indice
/// che non e' un confine di carattere PANICA. Il doc diceva «2000 char» ma
/// `text.len()` conta BYTE, e i due coincidono solo in ASCII: bastava una query
/// di ricerca piu' lunga di 2000 byte con un accento o un'emoji a cavallo di
/// quel confine per far cadere il processo. Non e' un caso di laboratorio — la
/// query la scrive il MODELLO, e questo helper la riceve da tre chiamanti:
/// `knowledge_search`, `knowledge_create_note` (dove il testo e' il corpo di una
/// nota) e `knowledge_get_subgraph`.
///
/// Il limite resta in BYTE, che e' quello che l'embedder vede davvero; a
/// cambiare e' solo il punto di taglio, portato indietro fino al confine di
/// carattere piu' vicino. Passare a 2000 CARATTERI avrebbe fatto crescere il
/// testo inviato per ogni lingua non-ASCII, che e' una decisione sull'embedder e
/// non la correzione di un panic.
fn embed_slice(text: &str) -> &str {
    if text.len() <= EMBED_MAX_BYTES {
        return text;
    }
    // L'ultimo confine di carattere che non supera il limite. `is_char_boundary`
    // e' vero per definizione su 0, quindi il ciclo termina sempre.
    let mut fine = EMBED_MAX_BYTES;
    while !text.is_char_boundary(fine) {
        fine -= 1;
    }
    &text[..fine]
}

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_search
// ═══════════════════════════════════════════════════════════════════════════

/// Parametri validati di `knowledge_search`.
#[derive(Debug)]
struct SearchParams {
    query: String,
    top_k: usize,
    min_score: f32,
}

/// Estrae e valida gli input di `knowledge_search` dal contratto d'ingresso.
fn parse_search_params(input: &Value) -> Result<SearchParams, RispostaTool> {
    use crate::{input_contract::InputTool, tool_inputs::KnowledgeSearchInput};

    let params = KnowledgeSearchInput::leggi(input)?;
    // Il contratto pretende che il campo CI SIA; che sia non vuoto dopo la
    // normalizzazione lo puo' dire solo qui.
    let query = params.query.trim().to_string();
    if query.is_empty() {
        return Err(crate::errore_tool(
            "query vuota: passa il testo da cercare (es. 'autenticazione OAuth Google')",
            NaturaFallimento::Rimediabile,
        ));
    }
    // Il tetto resta 100 e non 20 come dice la descrizione del catalogo: sopra
    // la soglia di `search_summary_threshold` il tool cambia MODO e risponde coi
    // cluster, e restringere qui renderebbe irraggiungibile quel modo.
    let top_k = params.top_k.unwrap_or(5).clamp(1, 100) as usize;
    let min_score = params.min_score.unwrap_or(0.4) as f32;
    Ok(SearchParams {
        query,
        top_k,
        min_score,
    })
}

/// Soglia summary-mode (DB-driven, regola G — niente fallback hardcoded sopra il
/// safe default 20).
async fn search_summary_threshold(ctx: &ToolContextCore) -> usize {
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'agent.kb.graph_summary_threshold_topk'",
    )
    .fetch_optional(&*ctx.db)
    .await
    .ok()
    .flatten()
    .and_then(|v| v.trim().parse().ok())
    .unwrap_or(20)
}

/// Serializza le righe di cluster (theme/count/sample_titles) e ne somma i
/// count. Ritorna `(clusters_json, total)`.
fn cluster_rows_to_json(rows: &[sqlx::postgres::PgRow]) -> (Vec<Value>, i32) {
    let clusters: Vec<Value> = rows
        .iter()
        .map(|r| {
            let theme: Option<String> = r.try_get("theme").ok();
            let count: i32 = r.try_get("count").unwrap_or(0);
            let titles: Vec<String> = r.try_get("sample_titles").unwrap_or_default();
            json!({
                "theme": theme.unwrap_or_else(|| "other".to_string()),
                "count": count,
                "sample_titles": titles,
            })
        })
        .collect();
    let total: i32 = clusters
        .iter()
        .filter_map(|c| c.get("count").and_then(|v| v.as_i64()))
        .sum::<i64>() as i32;
    (clusters, total)
}

/// Summary-mode: cluster per `intent` (o `kind` se intent assente) sui doc del
/// progetto. Esclude i doc 'frozen' (semantica equivalente al vecchio off_topic).
async fn knowledge_search_summary(
    ctx: &ToolContextCore,
    top_k: usize,
) -> Result<String, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        WITH ranked AS (
            SELECT COALESCE(intent, kind) AS theme, title,
                   row_number() OVER (PARTITION BY COALESCE(intent, kind)
                                      ORDER BY updated_at DESC) AS rk
            FROM wiki_docs
            WHERE scope = 'project' AND project_id = $1
              AND edit_lock <> 'frozen'
        )
        SELECT theme,
               COUNT(*)::int AS count,
               array_agg(title ORDER BY rk) FILTER (WHERE rk <= 3) AS sample_titles
        FROM ranked
        GROUP BY theme
        ORDER BY count DESC
        LIMIT $2
        "#,
    )
    .bind(ctx.project_id)
    .bind(top_k as i32)
    .fetch_all(&*ctx.db)
    .await?;
    let (clusters, total) = cluster_rows_to_json(&rows);
    Ok(json!({
        "mode": "summary",
        "clusters": clusters,
        "total": total,
        "hint": "Per body completo di un cluster: knowledge_search(query, top_k<=20)."
    })
    .to_string())
}

/// Ricerca semantica via embedding Qdrant: ritorna gli hit (doc_id, score)
/// sopra soglia gia' filtrati a `top_k`.
async fn knowledge_search_hits(
    ctx: &ToolContextCore,
    p: &SearchParams,
) -> Result<Vec<(Uuid, f32)>, RispostaTool> {
    let vector = match ctx.embedder.embed_text("", embed_slice(&p.query)).await {
        Ok(v) => v,
        Err(e) => return Err(errore_embedding("della query", e)),
    };
    let hits = match nexus_wiki::content_points::search_wiki_content_points_filtered(
        &ctx.db,
        vector,
        (p.top_k * 2).max(10),
        p.min_score as f64,
        Some(project_qdrant_filter(ctx.project_id)),
    )
    .await
    {
        Ok(h) => h,
        Err(e) => return Err(errore_indice(e)),
    };
    Ok(hits
        .iter()
        .filter(|h| (h.score as f32) >= p.min_score)
        .filter_map(|h| {
            h.point_id
                .parse::<Uuid>()
                .ok()
                .map(|id| (id, h.score as f32))
        })
        .take(p.top_k)
        .collect())
}

/// Serializza una riga di risultato ricerca nel formato agente-facing. Lo
/// `status` e' sempre "active" (i frozen sono gia' filtrati a monte); il campo
/// resta per non rompere il contratto del tool.
fn search_row_to_json(id: Uuid, r: &sqlx::postgres::PgRow) -> Value {
    let body: String = r.try_get("body_md").unwrap_or_default();
    let snippet = body.chars().take(300).collect::<String>();
    json!({
        "note_id": id.to_string(),
        "title": r.try_get::<String, _>("title").unwrap_or_default(),
        "intent": r.try_get::<Option<String>, _>("intent").ok().flatten()
            .or_else(|| r.try_get::<String, _>("kind").ok()),
        "status": "active",
        "tags": r.try_get::<Vec<String>, _>("tags").unwrap_or_default(),
        "snippet": snippet,
        "truncated": body.len() > 300,
    })
}

/// Idrata i doc-hit con i metadati da `wiki_docs`, escludendo i frozen, e
/// preserva l'ordine per score.
async fn knowledge_search_render(
    ctx: &ToolContextCore,
    doc_hits: &[(Uuid, f32)],
) -> Result<Vec<Value>, sqlx::Error> {
    let ids: Vec<Uuid> = doc_hits.iter().map(|(id, _)| *id).collect();
    let rows = sqlx::query(
        r#"
        SELECT id, title, body_md, tags, intent, kind, edit_lock
        FROM wiki_docs
        WHERE id = ANY($1) AND scope = 'project' AND project_id = $2
          AND edit_lock <> 'frozen'
        "#,
    )
    .bind(&ids)
    .bind(ctx.project_id)
    .fetch_all(&*ctx.db)
    .await?;

    let mut by_id: std::collections::HashMap<Uuid, Value> = std::collections::HashMap::new();
    for r in &rows {
        let id: Uuid = match r.try_get("id") {
            Ok(v) => v,
            Err(_) => continue,
        };
        by_id.insert(id, search_row_to_json(id, r));
    }

    Ok(doc_hits
        .iter()
        .filter_map(|(id, score)| {
            by_id.get(id).map(|note| {
                let mut n = note.clone();
                n["score"] = json!(*score);
                n
            })
        })
        .collect())
}

/// `knowledge_search` — top-K doc rilevanti via embedding Qdrant.
///
/// Input: { query, top_k?=5 (1..=100), min_score?=0.4 }.
/// Output: { results: [{note_id,title,intent,status,tags,score,snippet}], count }
/// oppure (top_k > soglia) { mode:"summary", clusters:[{theme,count,sample_titles}] }.
/// MIGRATO al contratto d'ingresso e a `RispostaTool` (regola Q).
pub async fn tool_knowledge_search(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    let params = match parse_search_params(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };

    if params.top_k > search_summary_threshold(ctx).await {
        return match knowledge_search_summary(ctx, params.top_k).await {
            Ok(testo) => RispostaTool::riuscito(testo),
            Err(e) => errore_db("lettura dei cluster della knowledge base fallita", e),
        };
    }

    let doc_hits = match knowledge_search_hits(ctx, &params).await {
        Ok(h) => h,
        Err(risposta) => return risposta,
    };
    // Nessun documento sopra soglia e' una RISPOSTA, non un fallimento: la
    // ricerca ha funzionato e ha guardato tutto. Il messaggio dice l'unica cosa
    // che l'agente puo' farci, che e' cambiare i parametri della domanda.
    if doc_hits.is_empty() {
        return RispostaTool::riuscito(
            json!({
                "results": [],
                "count": 0,
                "message": "nessun documento sopra min_score: abbassa min_score o riformula query",
            })
            .to_string(),
        );
    }

    match knowledge_search_render(ctx, &doc_hits).await {
        Ok(results) => {
            RispostaTool::riuscito(json!({"results": results, "count": results.len()}).to_string())
        }
        Err(e) => errore_db("lettura dei documenti trovati fallita", e),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// code_doc
// ═══════════════════════════════════════════════════════════════════════════

/// `code_doc` — documentazione code-wiki di un file. Cerca doc con
/// `kind='code_doc'` il cui `vault_file_path` o `title` matcha `file_path`.
pub async fn tool_code_doc(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::CodeDocInput};

    let params = match CodeDocInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let file_path = params.file_path.trim();
    if file_path.is_empty() {
        return crate::errore_tool(
            "file_path vuoto: passa il percorso del file, relativo alla root del progetto",
            NaturaFallimento::Rimediabile,
        );
    }

    let row = sqlx::query(
        r#"
        SELECT id, title, body_md FROM wiki_docs
        WHERE scope = 'project' AND project_id = $1 AND kind = 'code_doc'
          AND (title = $2 OR title LIKE $3 OR $2 LIKE '%' || title
               OR vault_file_path = $2 OR vault_file_path LIKE $3)
        ORDER BY (title = $2 OR vault_file_path = $2) DESC, updated_at DESC
        LIMIT 1
        "#,
    )
    .bind(ctx.project_id)
    .bind(file_path)
    .bind(format!("%{file_path}"))
    .fetch_optional(&*ctx.db)
    .await;

    match row {
        Ok(Some(r)) => {
            let title: String = r.try_get("title").unwrap_or_default();
            let body: String = r.try_get("body_md").unwrap_or_default();
            RispostaTool::riuscito(
                json!({ "file": title, "found": true, "body": body }).to_string(),
            )
        }
        // L'assenza E' la risposta, e il messaggio indirizza gia' altrove: resta
        // un successo, come la directory vuota di `list_files`.
        Ok(None) => RispostaTool::riuscito(
            json!({
                "file": file_path,
                "found": false,
                "message": "Nessuna documentazione (code_doc) per questo file. \
                            Prova knowledge_search per contesto correlato."
            })
            .to_string(),
        ),
        Err(e) => crate::errore_tool(
            format!("query fallita: {e}"),
            NaturaFallimento::DelSistema,
        ),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_get_note
// ═══════════════════════════════════════════════════════════════════════════

/// Costruisce il JSON di risposta di `knowledge_get_note` a partire dalla riga
/// `wiki_docs`. Isola il mapping (status, file_paths da tag "file:").
fn knowledge_note_json(note_id: Uuid, row: &sqlx::postgres::PgRow) -> Value {
    let edit_lock: String = row
        .try_get("edit_lock")
        .unwrap_or_else(|_| "none".to_string());
    let status = if edit_lock == "frozen" {
        "off_topic"
    } else {
        "active"
    };
    json!({
        "id": note_id.to_string(),
        "title": row.try_get::<String, _>("title").unwrap_or_default(),
        "body_md": row.try_get::<String, _>("body_md").unwrap_or_default(),
        "intent": row.try_get::<Option<String>, _>("intent").ok().flatten()
            .or_else(|| row.try_get::<String, _>("kind").ok()),
        "status": status,
        "tags": row.try_get::<Vec<String>, _>("tags").unwrap_or_default(),
        // file_paths: ricostruiti da tag con prefisso "file:" (se presenti),
        // best-effort per compatibilita' col contratto vecchio.
        "file_paths": row
            .try_get::<Vec<String>, _>("tags")
            .unwrap_or_default()
            .into_iter()
            .filter_map(|t| t.strip_prefix("file:").map(String::from))
            .collect::<Vec<_>>(),
    })
}

/// `knowledge_get_note` — body completo di un doc by id (scoped al progetto).
/// MIGRATO al contratto d'ingresso e a `RispostaTool` (regola Q).
///
/// PROMESSA FOSSILE, dichiarata e non chiusa: il catalogo annuncia «Aggiorna
/// access_count della nota» (`tool_schema.rs:1494`) e questo handler esegue una
/// sola SELECT. La stringa `access_count` non compare in nessuna migrazione ne'
/// in nessun sorgente del repo: non e' una funzione rotta, e' una frase
/// sopravvissuta a una colonna che non e' mai esistita. Toglierla dal catalogo
/// cambia cio' che il modello legge, quindi la decisione non e' di questa
/// migrazione — ma resta scritto qui che il tool non conta niente.
pub async fn tool_knowledge_get_note(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::KnowledgeGetNoteInput};

    let params = match KnowledgeGetNoteInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let note_id = match uuid_nota(&params.note_id, "note_id") {
        Ok(id) => id,
        Err(risposta) => return risposta,
    };

    let row = match sqlx::query(
        r#"
        SELECT id, title, body_md, intent, kind, tags, edit_lock,
               created_at, updated_at
        FROM wiki_docs
        WHERE id = $1 AND scope = 'project' AND project_id = $2
        "#,
    )
    .bind(note_id)
    .bind(ctx.project_id)
    .fetch_optional(&*ctx.db)
    .await
    {
        Ok(Some(r)) => r,
        // RIMEDIABILE: l'id e' sbagliato o la nota appartiene a un altro
        // progetto, e il messaggio deve dire dove trovarne uno valido — prima
        // diceva solo «non trovata o non accessibile», che non e' un'azione.
        Ok(None) => {
            return crate::errore_tool(
                "nota non trovata o non accessibile: usa knowledge_search \
                 per gli id delle note di questo progetto",
                NaturaFallimento::Rimediabile,
            )
        }
        Err(e) => return crate::errore_tool(format!("DB: {e}"), NaturaFallimento::DelSistema),
    };

    RispostaTool::riuscito(knowledge_note_json(note_id, &row).to_string())
}

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_create_note
// ═══════════════════════════════════════════════════════════════════════════

/// Input validati di `knowledge_create_note`.
#[derive(Debug)]
struct CreateNoteParams {
    title: String,
    body_md: String,
    intent: String,
    tags: Vec<String>,
}

/// Valida e normalizza gli input di `knowledge_create_note` dal contratto
/// d'ingresso. I `file_paths` diventano tag con prefisso "file:" (preserva
/// l'info nel nuovo schema).
///
/// Il limite del titolo e' contato in CARATTERI e non piu' in byte: il catalogo
/// promette «1-200 char», e `str::len()` conta byte — un titolo di 150 lettere
/// accentate ne occupa 300 e veniva respinto ben sotto il limite promesso, con
/// un messaggio che citava un numero che l'agente vedeva rispettato.
fn parse_create_note_params(input: &Value) -> Result<CreateNoteParams, RispostaTool> {
    use crate::{input_contract::InputTool, tool_inputs::KnowledgeCreateNoteInput};

    let params = KnowledgeCreateNoteInput::leggi(input)?;
    let title = params.title.trim().to_string();
    let lunghezza = title.chars().count();
    if title.is_empty() || lunghezza > 200 {
        return Err(crate::errore_tool(
            format!("title di {lunghezza} caratteri: serve un titolo breve, da 1 a 200 caratteri"),
            NaturaFallimento::Rimediabile,
        ));
    }
    let body_md = params.body_md.trim().to_string();
    if body_md.is_empty() {
        return Err(crate::errore_tool(
            "body_md vuoto: passa il contenuto Markdown della nota",
            NaturaFallimento::Rimediabile,
        ));
    }
    // L'`intent` e' un enum del contratto: un valore fuori vocabolario non
    // arriva fin qui, e il default resta quello storico.
    let intent = params
        .intent
        .map(|i| i.come_stringa())
        .unwrap_or("feature")
        .to_string();
    let mut tags = params.tags.unwrap_or_default();
    for percorso in params.file_paths.unwrap_or_default() {
        if !percorso.is_empty() {
            tags.push(format!("file:{percorso}"));
        }
    }
    Ok(CreateNoteParams {
        title,
        body_md,
        intent,
        tags,
    })
}

/// Upsert del doc in `wiki_docs` (scope=project, kind='note'). Ritorna l'id del
/// doc creato/aggiornato.
async fn insert_note_doc(
    ctx: &ToolContextCore,
    p: &CreateNoteParams,
    slug: &str,
    body_hash: &str,
) -> Result<Uuid, sqlx::Error> {
    // kind = 'note' fisso; intent porta la categoria semantica.
    sqlx::query_scalar(
        r#"
        INSERT INTO wiki_docs (
            scope, project_id, slug, title, body_md, body_hash,
            kind, intent, tags,
            edit_lock, protected_sections, manually_edited,
            current_version, auto_generated, public_read
        ) VALUES (
            'project', $1, $2, $3, $4, $5,
            $6, $7, $8,
            'none', '{}', FALSE,
            1, FALSE, FALSE
        )
        ON CONFLICT (scope, COALESCE(project_id::text,''), slug) DO UPDATE SET
            title    = EXCLUDED.title,
            body_md  = EXCLUDED.body_md,
            body_hash= EXCLUDED.body_hash,
            tags     = EXCLUDED.tags,
            intent   = EXCLUDED.intent,
            updated_at = NOW()
        RETURNING id
        "#,
    )
    .bind(ctx.project_id)
    .bind(slug)
    .bind(&p.title)
    .bind(&p.body_md)
    .bind(body_hash)
    .bind("note")
    .bind(&p.intent)
    .bind(&p.tags)
    .fetch_one(&*ctx.db)
    .await
}

/// Embedding + upsert Qdrant del doc (best-effort). Ritorna `true` se il punto
/// e' stato indicizzato. Non propaga errori: logga WARN e ritorna `false`.
///
/// E' l'unico errore di questo modulo che resta INGHIOTTITO, ed e' legittimo
/// perche' non viene inghiottito affatto: la nota E' stata scritta, e il fatto
/// che l'indicizzazione non sia riuscita viaggia in un CAMPO della risposta
/// (`qdrant_indexed`). Dichiarare qui un fallimento direbbe all'agente che la
/// nota non esiste, e gliela farebbe riscrivere.
async fn index_note_qdrant(ctx: &ToolContextCore, note_id: Uuid, p: &CreateNoteParams) -> bool {
    let snippet = embed_slice(&p.body_md);
    let combined = format!("{}\n\n{snippet}", p.title);
    let vector = match ctx.embedder.embed_text("", &combined).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "knowledge_create_note: embed fallito");
            return false;
        }
    };
    let point_id = note_id.to_string();
    let payload = json!({
        "scope": "project",
        "doc_id": point_id,
        "project_id": ctx.project_id.to_string(),
        "title": p.title,
        "tags": p.tags,
        "kind": "note",
        "intent": p.intent,
    });
    match nexus_wiki::content_points::upsert_wiki_content_point(&ctx.db, &point_id, vector, payload).await
    {
        Ok(_) => {
            let _ = sqlx::query("UPDATE wiki_docs SET qdrant_point_id = $1 WHERE id = $2")
                .bind(&point_id)
                .bind(note_id)
                .execute(&*ctx.db)
                .await;
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "knowledge_create_note: Qdrant upsert fallito");
            false
        }
    }
}

/// `knowledge_create_note` — crea un doc scope=project + embedding Qdrant.
/// MIGRATO al contratto d'ingresso e a `RispostaTool` (regola Q).
pub async fn tool_knowledge_create_note(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    let params = match parse_create_note_params(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };

    // Slug derivato dal title (slugify minimal: lowercase + replace).
    let slug = nexus_wiki::vault::slugify(&params.title);
    if slug.is_empty() {
        return crate::errore_tool(
            "title senza caratteri utili allo slug: il nome del documento si deriva dal \
             titolo, che deve contenere almeno una lettera o una cifra",
            NaturaFallimento::Rimediabile,
        );
    }
    let body_hash = nexus_wiki::vault::sha256_hex(&params.body_md);

    let note_id = match insert_note_doc(ctx, &params, &slug, &body_hash).await {
        Ok(id) => id,
        Err(e) => return errore_db("scrittura della nota fallita", e),
    };

    let qdrant_indexed = index_note_qdrant(ctx, note_id, &params).await;

    tracing::info!(
        project_id = %ctx.project_id,
        note_id = %note_id,
        intent = %params.intent,
        "knowledge_create_note: doc creato via MCP tool (wiki_docs)"
    );

    RispostaTool::riuscito(
        json!({
            "ok": true,
            "note_id": note_id.to_string(),
            "intent": params.intent,
            "qdrant_indexed": qdrant_indexed,
        })
        .to_string(),
    )
}

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_get_links
// ═══════════════════════════════════════════════════════════════════════════

/// Verifica che il doc appartenga al progetto corrente.
///
/// `Ok(false)` significa «la nota non c'e'», e solo quello: prima un DB muto
/// dava lo stesso `false` (`.unwrap_or(0) > 0`), e l'handler lo raccontava come
/// «nota non trovata nel progetto corrente» — mandando l'agente a correggere un
/// id che era gia' giusto.
async fn note_in_project(ctx: &ToolContextCore, note_id: Uuid) -> Result<bool, sqlx::Error> {
    let quante = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM wiki_docs \
         WHERE id = $1 AND scope = 'project' AND project_id = $2",
    )
    .bind(note_id)
    .bind(ctx.project_id)
    .fetch_one(&*ctx.db)
    .await?;
    Ok(quante > 0)
}

/// Carica i link in una direzione (`outgoing`=true: edge da `note_id`;
/// altrimenti verso `note_id`) verso doc visibili al progetto (proprio progetto
/// o meta public_read=true), escludendo i doc frozen.
///
/// Un elenco VUOTO significa «questa nota non ha link in questa direzione», e
/// solo quello: l'errore di query non vi collassa piu' dentro.
async fn load_directional_links(
    ctx: &ToolContextCore,
    note_id: Uuid,
    outgoing: bool,
) -> Result<Vec<sqlx::postgres::PgRow>, sqlx::Error> {
    // Le due direzioni differiscono solo per la colonna di ancoraggio e il JOIN.
    let sql = if outgoing {
        r#"
        SELECT l.from_doc_id, l.to_doc_id AS other_id, l.rel_type, l.created_by,
               l.confidence, d.title, d.intent, d.kind, d.scope, d.edit_lock
        FROM wiki_links l
        JOIN wiki_docs d ON d.id = l.to_doc_id
        WHERE l.from_doc_id = $1
          AND ( (d.scope = 'project' AND d.project_id = $2)
                OR (d.scope = 'meta' AND d.public_read = TRUE) )
          AND d.edit_lock <> 'frozen'
        ORDER BY l.confidence DESC
        "#
    } else {
        r#"
        SELECT l.to_doc_id, l.from_doc_id AS other_id, l.rel_type, l.created_by,
               l.confidence, d.title, d.intent, d.kind, d.scope, d.edit_lock
        FROM wiki_links l
        JOIN wiki_docs d ON d.id = l.from_doc_id
        WHERE l.to_doc_id = $1
          AND ( (d.scope = 'project' AND d.project_id = $2)
                OR (d.scope = 'meta' AND d.public_read = TRUE) )
          AND d.edit_lock <> 'frozen'
        ORDER BY l.confidence DESC
        "#
    };
    sqlx::query(sql)
        .bind(note_id)
        .bind(ctx.project_id)
        .fetch_all(&*ctx.db)
        .await
}

/// Serializza le righe di link nel formato agente-facing (rel_type tradotto).
fn links_to_json(rows: &[sqlx::postgres::PgRow]) -> Vec<Value> {
    rows.iter()
        .filter_map(|r| {
            let other = r.try_get::<Uuid, _>("other_id").ok()?;
            let stored_rel: String = r.try_get("rel_type").unwrap_or_default();
            Some(json!({
                "note_id": other.to_string(),
                "title": r.try_get::<String, _>("title").unwrap_or_default(),
                "intent": r.try_get::<Option<String>, _>("intent").ok().flatten()
                    .or_else(|| r.try_get::<String, _>("kind").ok()),
                "rel_type": map_rel_from_wiki(&stored_rel),
                "rel_type_raw": stored_rel,
                "created_by": r.try_get::<String, _>("created_by").unwrap_or_default(),
                "confidence": r.try_get::<f32, _>("confidence").unwrap_or(1.0),
                "scope": r.try_get::<String, _>("scope").unwrap_or_default(),
            }))
        })
        .collect()
}

/// `knowledge_get_links` — outbound + inbound links di un doc, scoped al progetto.
/// MIGRATO al contratto d'ingresso e a `RispostaTool` (regola Q).
///
/// Una nota SENZA link e' un successo con due elenchi vuoti: e' la stessa
/// distinzione della directory vuota di `list_files`, e qui la fa il DB — che
/// prima non veniva interrogato sull'esito della propria risposta.
pub async fn tool_knowledge_get_links(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::KnowledgeGetLinksInput};

    let params = match KnowledgeGetLinksInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let note_id = match uuid_nota(&params.note_id, "note_id") {
        Ok(id) => id,
        Err(risposta) => return risposta,
    };

    match note_in_project(ctx, note_id).await {
        Ok(true) => {}
        Ok(false) => {
            return crate::errore_tool(
                "nota non trovata nel progetto corrente: usa knowledge_search \
                 per gli id delle note di questo progetto",
                NaturaFallimento::Rimediabile,
            )
        }
        Err(e) => return errore_db("verifica di appartenenza della nota fallita", e),
    }

    let out = match load_directional_links(ctx, note_id, true).await {
        Ok(rows) => links_to_json(&rows),
        Err(e) => return errore_db("lettura dei link uscenti fallita", e),
    };
    let inc = match load_directional_links(ctx, note_id, false).await {
        Ok(rows) => links_to_json(&rows),
        Err(e) => return errore_db("lettura dei link entranti fallita", e),
    };
    RispostaTool::riuscito(
        json!({
            "note_id": note_id.to_string(),
            "outgoing": out,
            "incoming": inc,
            "outgoing_count": out.len(),
            "incoming_count": inc.len(),
        })
        .to_string(),
    )
}

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_get_subgraph
// ═══════════════════════════════════════════════════════════════════════════

/// Parametri validati di `knowledge_get_subgraph`.
struct SubgraphParams {
    max_nodes: usize,
    depth: usize,
    /// rel_type gia' mappati al vocabolario `wiki_links` per la query.
    rel_filter_wiki: Vec<String>,
}

/// Estrae e valida i parametri comuni di `knowledge_get_subgraph`. Il filtro
/// vuoto o assente vale «tutte le relazioni», che e' il default storico.
fn parse_subgraph_params(letti: &KnowledgeGetSubgraphInput) -> SubgraphParams {
    let max_nodes = letti.max_nodes.unwrap_or(30).clamp(1, 100) as usize;
    let depth = letti.depth.unwrap_or(2).clamp(1, 4) as usize;
    // I `rel_types` sono un enum del contratto: un valore fuori vocabolario non
    // arriva fin qui, quindi non c'e' piu' nulla da filtrare a mano.
    let richiesti: Vec<String> = letti
        .rel_types
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|r| r.come_stringa().to_string())
        .collect();
    let rel_filter_wiki = if richiesti.is_empty() {
        KNOWLEDGE_REL_TYPES
            .iter()
            .map(|r| map_rel_to_wiki(r).to_string())
            .collect()
    } else {
        richiesti
            .iter()
            .map(|r| map_rel_to_wiki(r).to_string())
            .collect()
    };
    SubgraphParams {
        max_nodes,
        depth,
        rel_filter_wiki,
    }
}

/// Risolve i nodi seed: da `query` (semantica via Qdrant) o da `note_id`.
async fn resolve_subgraph_seed(
    ctx: &ToolContextCore,
    letti: &KnowledgeGetSubgraphInput,
    max_nodes: usize,
) -> Result<Vec<Uuid>, RispostaTool> {
    if let Some(q) = letti
        .query
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return seed_da_query(ctx, q, max_nodes).await;
    }
    // Un `note_id` malformato non e' piu' un seed ASSENTE: prima
    // `.and_then(Uuid::parse_str(..).ok())` collassava i due casi e l'agente si
    // sentiva dire che non aveva passato il campo che aveva passato.
    if let Some(grezzo) = letti.note_id.as_deref() {
        let id = uuid_nota(grezzo, "note_id")?;
        // L'esistenza del seed si chiede QUI. Senza, un id che questo progetto
        // non vede — di un altro progetto, o di nessuno — attraversava BFS, nodi
        // e archi e usciva come sottografo VUOTO: «questa nota non ha vicini»
        // detto di una nota che qui non c'e'. E' l'assenza inventata che questo
        // handler e' stato migrato per togliere, in una forma che nessun DB muto
        // produce: lo STESSO id dava un fallimento con la causa in
        // `knowledge_get_links` e un successo con zero nodi qui.
        //
        // Il criterio e' la VISIBILITA' (punto unico `note_accessibili`), non
        // l'appartenenza al progetto: una nota `meta` con `public_read` e' un
        // estremo di link legittimo, i suoi vicini di progetto sono un
        // sottografo reale, e restringere a `scope='project'` avrebbe fatto
        // fallire un seed che prima funzionava.
        return match note_accessibili(ctx, &[id]).await {
            Ok(0) => Err(crate::errore_tool(
                "nota seed non trovata: usa knowledge_search per gli id delle note \
                 di questo progetto, oppure passa 'query' invece di 'note_id'",
                NaturaFallimento::Rimediabile,
            )),
            Ok(_) => Ok(vec![id]),
            Err(e) => Err(errore_db("verifica di esistenza della nota seed fallita", e)),
        };
    }
    Err(crate::errore_tool(
        "seed mancante: passa 'query' (testo da cui partire) oppure 'note_id' (UUID di una nota)",
        NaturaFallimento::Rimediabile,
    ))
}

/// I nodi seed trovati per similarita' semantica. La soglia e' 0.0 per
/// costruzione: qui il seed serve a partire, non a decidere pertinenza.
async fn seed_da_query(
    ctx: &ToolContextCore,
    query: &str,
    max_nodes: usize,
) -> Result<Vec<Uuid>, RispostaTool> {
    let vector = match ctx.embedder.embed_text("", embed_slice(query)).await {
        Ok(v) => v,
        Err(e) => return Err(errore_embedding("del seed", e)),
    };
    // Un indice muto dava un elenco vuoto, quindi un sottografo VUOTO
    // dichiarato riuscito: «la knowledge base non ha nulla su questo tema»
    // detto da chi non e' riuscito a guardare.
    let hits = match nexus_wiki::content_points::search_wiki_content_points_filtered(
        &ctx.db,
        vector,
        max_nodes,
        0.0,
        Some(project_qdrant_filter(ctx.project_id)),
    )
    .await
    {
        Ok(h) => h,
        Err(e) => return Err(errore_indice(e)),
    };
    let mut nodes: Vec<Uuid> = Vec::new();
    for h in hits.iter() {
        if let Ok(id) = h.point_id.parse::<Uuid>() {
            if !nodes.contains(&id) {
                nodes.push(id);
            }
        }
    }
    Ok(nodes)
}

/// BFS via `wiki_links` a partire dai nodi seed, fino a `depth` livelli o
/// `max_nodes` nodi. Muta `nodes` aggiungendo i vicini scoperti.
///
/// Una query fallita interrompe l'espansione DICHIARANDOLO: prima produceva una
/// frontiera vuota, cioe' un sottografo troncato che nessuno distingueva da uno
/// che non ha altri vicini.
async fn expand_subgraph_bfs(
    ctx: &ToolContextCore,
    p: &SubgraphParams,
    nodes: &mut Vec<Uuid>,
) -> Result<(), sqlx::Error> {
    let mut frontier = nodes.clone();
    for _ in 0..p.depth {
        if nodes.len() >= p.max_nodes {
            break;
        }
        let neigh = sqlx::query(
            r#"
            SELECT from_doc_id, to_doc_id FROM wiki_links
            WHERE rel_type = ANY($1)
              AND (from_doc_id = ANY($2) OR to_doc_id = ANY($2))
            "#,
        )
        .bind(&p.rel_filter_wiki)
        .bind(&*frontier)
        .fetch_all(&*ctx.db)
        .await?;
        let mut next: Vec<Uuid> = Vec::new();
        for r in &neigh {
            for col in ["from_doc_id", "to_doc_id"] {
                if let Ok(id) = r.try_get::<Uuid, _>(col) {
                    if !nodes.contains(&id) && !next.contains(&id) {
                        next.push(id);
                    }
                }
            }
        }
        if next.is_empty() {
            break;
        }
        for id in next.iter() {
            if nodes.len() < p.max_nodes {
                nodes.push(*id);
            }
        }
        frontier = next;
    }
    Ok(())
}

/// Dettagli dei nodi validi (scope=project + project_id + non-frozen). Ritorna
/// gli id validi e la loro serializzazione JSON.
async fn subgraph_nodes(
    ctx: &ToolContextCore,
    nodes: &[Uuid],
) -> Result<(Vec<Uuid>, Vec<Value>), sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT id, title, intent, kind, edit_lock FROM wiki_docs
        WHERE id = ANY($1) AND scope = 'project' AND project_id = $2
          AND edit_lock <> 'frozen'
        "#,
    )
    .bind(nodes)
    .bind(ctx.project_id)
    .fetch_all(&*ctx.db)
    .await?;
    let valid_ids: Vec<Uuid> = rows
        .iter()
        .filter_map(|r| r.try_get::<Uuid, _>("id").ok())
        .collect();
    let node_json: Vec<Value> = rows
        .iter()
        .filter_map(|r| {
            let id = r.try_get::<Uuid, _>("id").ok()?;
            Some(json!({
                "note_id": id.to_string(),
                "title": r.try_get::<String, _>("title").unwrap_or_default(),
                "intent": r.try_get::<Option<String>, _>("intent").ok().flatten()
                    .or_else(|| r.try_get::<String, _>("kind").ok()),
                "status": "active",
            }))
        })
        .collect();
    Ok((valid_ids, node_json))
}

/// Archi intra-sottografo tra i nodi validi, serializzati per l'agente.
async fn subgraph_edges(
    ctx: &ToolContextCore,
    rel_filter_wiki: &[String],
    valid_ids: &[Uuid],
) -> Result<Vec<Value>, sqlx::Error> {
    let edges = sqlx::query(
        r#"
        SELECT from_doc_id, to_doc_id, rel_type, confidence FROM wiki_links
        WHERE rel_type = ANY($1)
          AND from_doc_id = ANY($2) AND to_doc_id = ANY($2)
        "#,
    )
    .bind(rel_filter_wiki)
    .bind(valid_ids)
    .fetch_all(&*ctx.db)
    .await?;
    Ok(edges
        .iter()
        .filter_map(|r| {
            let f = r.try_get::<Uuid, _>("from_doc_id").ok()?;
            let t = r.try_get::<Uuid, _>("to_doc_id").ok()?;
            let stored_rel: String = r.try_get("rel_type").unwrap_or_default();
            Some(json!({
                "from": f.to_string(),
                "to": t.to_string(),
                "rel_type": map_rel_from_wiki(&stored_rel),
                "rel_type_raw": stored_rel,
                "confidence": r.try_get::<f32, _>("confidence").unwrap_or(1.0),
            }))
        })
        .collect())
}

/// `knowledge_get_subgraph` — BFS dal seed (query semantica o note_id) sui link.
/// MIGRATO al contratto d'ingresso e a `RispostaTool` (regola Q).
///
/// E' l'handler su cui gli errori inghiottiti pesavano di piu': QUATTRO dei suoi
/// cinque passi (seed, espansione, nodi, archi) trasformavano un guasto in un
/// grafo piu' piccolo, e il piu' piccolo di tutti — zero nodi e zero archi —
/// usciva come risposta legittima. Un sottografo troncato non si distingue a
/// occhio da uno completo, ed e' su quello che l'agente decide cosa leggere.
pub async fn tool_knowledge_get_subgraph(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    use crate::input_contract::InputTool;

    let letti = match KnowledgeGetSubgraphInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let params = parse_subgraph_params(&letti);

    let mut nodes = match resolve_subgraph_seed(ctx, &letti, params.max_nodes).await {
        Ok(n) => n,
        Err(risposta) => return risposta,
    };
    // Nessun seed trovato e' una RISPOSTA: la ricerca ha guardato e non c'e'
    // nulla che assomigli alla query.
    if nodes.is_empty() {
        return RispostaTool::riuscito(
            json!({
                "nodes": [], "edges": [], "node_count": 0, "edge_count": 0,
                "message": "nessun nodo seed trovato per questa query",
            })
            .to_string(),
        );
    }

    if let Err(e) = expand_subgraph_bfs(ctx, &params, &mut nodes).await {
        return errore_db("espansione del sottografo fallita", e);
    }
    let (valid_ids, node_json) = match subgraph_nodes(ctx, &nodes).await {
        Ok(v) => v,
        Err(e) => return errore_db("lettura dei nodi del sottografo fallita", e),
    };
    let edge_json = match subgraph_edges(ctx, &params.rel_filter_wiki, &valid_ids).await {
        Ok(v) => v,
        Err(e) => return errore_db("lettura degli archi del sottografo fallita", e),
    };

    RispostaTool::riuscito(
        json!({
            "nodes": node_json,
            "edges": edge_json,
            "node_count": node_json.len(),
            "edge_count": edge_json.len(),
        })
        .to_string(),
    )
}

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_create_link
// ═══════════════════════════════════════════════════════════════════════════

/// Input validati di `knowledge_create_link`.
struct CreateLinkParams {
    from: Uuid,
    to: Uuid,
    /// rel_type agente-facing (per l'output).
    rel_input: String,
    /// rel_type mappato al vocabolario `wiki_links` (per lo storage).
    rel_wiki: &'static str,
    confidence: f32,
}

/// Valida e normalizza gli input di `knowledge_create_link` dal contratto
/// d'ingresso. Il `rel_type` e' un enum del contratto: il valore fuori
/// vocabolario lo respinge la deserializzazione, col suo elenco di ammessi.
fn parse_create_link_params(input: &Value) -> Result<CreateLinkParams, RispostaTool> {
    use crate::{input_contract::InputTool, tool_inputs::KnowledgeCreateLinkInput};

    let params = KnowledgeCreateLinkInput::leggi(input)?;
    let from = uuid_nota(&params.from_note_id, "from_note_id")?;
    let to = uuid_nota(&params.to_note_id, "to_note_id")?;
    if from == to {
        return Err(crate::errore_tool(
            "self-link non ammesso: from_note_id e to_note_id sono la stessa nota, \
             passa due id diversi",
            NaturaFallimento::Rimediabile,
        ));
    }
    let rel_input = params.rel_type.come_stringa().to_string();
    let rel_wiki = map_rel_to_wiki(&rel_input);
    let confidence = params.confidence.unwrap_or(1.0).clamp(0.0, 1.0) as f32;
    Ok(CreateLinkParams {
        from,
        to,
        rel_input,
        rel_wiki,
        confidence,
    })
}

/// Quante, fra le note passate, sono ACCESSIBILI da questo progetto: proprie,
/// oppure meta con `public_read`.
///
/// PUNTO UNICO (regola L) del predicato di visibilita'. Le domande che vi si
/// appoggiano sono due e differiscono solo per l'insieme: `both_docs_accessible`
/// chiede «ci sono entrambe?» prima di legarle, il seed di
/// `knowledge_get_subgraph` chiede «c'e' questa?» prima di partire. Il
/// vocabolario di «accessibile» dev'essere lo stesso, o due tool direbbero cose
/// diverse degli stessi id — ed e' cio' che accadeva: una nota `meta` con
/// `public_read` e' un estremo di link legittimo per `knowledge_create_link` e
/// un vicino legittimo per `knowledge_get_links`, quindi un criterio scritto sul
/// solo `scope='project'` la escluderebbe da un seed che il resto del modulo
/// tratta come normale.
async fn note_accessibili(ctx: &ToolContextCore, ids: &[Uuid]) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM wiki_docs \
         WHERE id = ANY($1) \
           AND ( (scope='project' AND project_id = $2) \
                 OR (scope='meta' AND public_read = TRUE) )",
    )
    .bind(ids)
    .bind(ctx.project_id)
    .fetch_one(&*ctx.db)
    .await
}

/// Verifica che entrambi i doc esistano e siano accessibili dal progetto
/// (entrambi project corrente, oppure to_note appartiene a meta public).
///
/// `Ok(false)` significa «almeno una delle due note non c'e'». Il `.unwrap_or(0)`
/// di prima faceva di un DB muto la stessa cosa, e il messaggio che ne usciva
/// accusava gli id — cioe' l'unico dato che era sicuramente giusto.
///
/// I due id sono DIVERSI per costruzione (il cappio lo ferma
/// `parse_create_link_params` prima di arrivare qui), quindi il conteggio a due
/// e' esatto: con `from == to` l'array ne conterebbe una sola.
async fn both_docs_accessible(
    ctx: &ToolContextCore,
    from: Uuid,
    to: Uuid,
) -> Result<bool, sqlx::Error> {
    Ok(note_accessibili(ctx, &[from, to]).await? == 2)
}

/// `knowledge_create_link` — crea o aggiorna un link tra due doc del progetto.
/// MIGRATO al contratto d'ingresso e a `RispostaTool` (regola Q).
pub async fn tool_knowledge_create_link(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    let p = match parse_create_link_params(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };

    match both_docs_accessible(ctx, p.from, p.to).await {
        Ok(true) => {}
        Ok(false) => {
            return crate::errore_tool(
                "una o entrambe le note non esistono nel progetto corrente: \
                 usa knowledge_search per gli id delle note di questo progetto",
                NaturaFallimento::Rimediabile,
            )
        }
        Err(e) => return errore_db("verifica di esistenza delle due note fallita", e),
    }

    // wiki_links: PK = (from_doc_id, to_doc_id, rel_type). ON CONFLICT update
    // della confidence/created_by.
    let res = sqlx::query(
        r#"
        INSERT INTO wiki_links (from_doc_id, to_doc_id, rel_type, created_by, confidence, evidence)
        VALUES ($1, $2, $3, 'agent', $4, 'agent tool knowledge_create_link')
        ON CONFLICT (from_doc_id, to_doc_id, rel_type)
        DO UPDATE SET confidence = EXCLUDED.confidence, created_by = 'agent'
        "#,
    )
    .bind(p.from)
    .bind(p.to)
    .bind(p.rel_wiki)
    .bind(p.confidence)
    .execute(&*ctx.db)
    .await;

    match res {
        Ok(_) => RispostaTool::riuscito(
            json!({
                "ok": true,
                "from_note_id": p.from.to_string(),
                "to_note_id": p.to.to_string(),
                "rel_type": p.rel_input,
                "rel_type_raw": p.rel_wiki,
            })
            .to_string(),
        ),
        // "INSERT" qui e' il prefisso di un messaggio d'errore diagnostico, non
        // una query costruita per interpolazione: la INSERT sopra e' interamente
        // parametrizzata via .bind(). Il messaggio evita la keyword SQL letterale.
        Err(e) => errore_db("creazione del link fallita", e),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_set_relevance
// ═══════════════════════════════════════════════════════════════════════════

/// `knowledge_set_relevance` — marca un doc come off-topic (`edit_lock='frozen'`)
/// o on-topic (`edit_lock='none'`). Il campo `relevance_score` non e' piu'
/// persistito nel nuovo schema; viene accettato per compatibilita' ma ignorato.
/// MIGRATO al contratto d'ingresso e a `RispostaTool` (regola Q).
///
/// DIVERGENZA DICHIARATA, non chiusa: il catalogo promette `relevance_score`
/// (`tool_schema.rs:1586`, «Punteggio di pertinenza 0-1») e questo handler non
/// lo legge, oggi come prima — non c'e' una colonna dove metterlo. Il parametro
/// e' una promessa che il sistema non mantiene, e il contratto lo dichiara
/// perche' il catalogo lo dichiara: toglierlo dallo schema cambierebbe cio' che
/// il modello vede, e va deciso da chi sa se quella colonna arrivera'. Qui si
/// annota che passarlo non ha effetto.
///
/// Un tipo sbagliato ora si distingue da un campo assente: `off_topic` letto con
/// `as_bool()` respingeva la stringa `"true"` col messaggio «off_topic (bool)
/// mancante», che per chi l'aveva passata era falso.
pub async fn tool_knowledge_set_relevance(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::KnowledgeSetRelevanceInput};

    let params = match KnowledgeSetRelevanceInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let note_id = match uuid_nota(&params.note_id, "note_id") {
        Ok(id) => id,
        Err(risposta) => return risposta,
    };
    let off_topic = params.off_topic;
    let new_lock = if off_topic { "frozen" } else { "none" };

    let res = sqlx::query(
        r#"
        UPDATE wiki_docs
        SET edit_lock = $2, updated_at = NOW()
        WHERE id = $1 AND scope = 'project' AND project_id = $3
        "#,
    )
    .bind(note_id)
    .bind(new_lock)
    .bind(ctx.project_id)
    .execute(&*ctx.db)
    .await;

    match res {
        Ok(r) if r.rows_affected() > 0 => RispostaTool::riuscito(
            json!({
                "ok": true,
                "note_id": note_id.to_string(),
                "off_topic": off_topic,
            })
            .to_string(),
        ),
        // RIMEDIABILE: l'id e' sbagliato o appartiene a un altro progetto, e il
        // messaggio nomina il tool con cui trovarne uno valido — senza, dire
        // «rimediabile» sarebbe una promessa non mantenuta.
        Ok(_) => crate::errore_tool(
            "nota non trovata nel progetto corrente: usa knowledge_search \
             per gli id delle note di questo progetto",
            NaturaFallimento::Rimediabile,
        ),
        // "UPDATE" qui e' il prefisso di un messaggio d'errore diagnostico, non
        // una query costruita per interpolazione: la UPDATE sopra e' interamente
        // parametrizzata via .bind(). Il messaggio evita la keyword SQL letterale.
        Err(e) => crate::errore_tool(
            format!("aggiornamento rilevanza fallito: {e}"),
            NaturaFallimento::DelSistema,
        ),
    }
}

/// L'uuid di una nota, dal parametro che il contratto ha letto come stringa.
///
/// PUNTO UNICO dei quattro handler che ricevono un id di nota. Il contratto
/// pretende che il campo CI SIA e sia una stringa; che sia un uuid non lo puo'
/// dire, e quel controllo resta qui — ma i due casi ora sono DUE. Prima
/// `.and_then(Uuid::parse_str(s).ok())` li collassava, e chi aveva passato un id
/// malformato riceveva «mancante»: un messaggio che gli diceva di aggiungere un
/// campo che aveva gia' messo.
fn uuid_nota(grezzo: &str, campo: &str) -> Result<Uuid, RispostaTool> {
    Uuid::parse_str(grezzo).map_err(|_| {
        crate::errore_tool(
            format!(
                "{campo}: '{grezzo}' non e' un UUID valido. \
                 Usa knowledge_search per gli id delle note di questo progetto."
            ),
            NaturaFallimento::Rimediabile,
        )
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_import_graph
// ═══════════════════════════════════════════════════════════════════════════

/// Config di `knowledge_import_graph` letta dai settings (riusa le chiavi
/// storiche; safe defaults se mancanti).
struct GraphImportConfig {
    enabled: bool,
    max_nodes: usize,
}

/// Legge la config di import grafi dai settings.
///
/// I default valgono per la RIGA ASSENTE, che e' una configurazione legittima;
/// il DB muto e' un'altra cosa e ora risale. Un interruttore che l'amministratore
/// ha spento non puo' tornare acceso perche' nessuno e' riuscito a leggerlo.
async fn load_graph_import_config(
    ctx: &ToolContextCore,
) -> Result<GraphImportConfig, sqlx::Error> {
    let mut enabled = true;
    let mut max_nodes = 2000usize;
    let rows = sqlx::query(
        "SELECT key, value FROM settings WHERE key IN \
         ('knowledge.graph_import_enabled','knowledge.graph_import_max_nodes')",
    )
    .fetch_all(&*ctx.db)
    .await?;
    for r in &rows {
        let k: String = r.try_get("key").unwrap_or_default();
        let v: String = r.try_get("value").unwrap_or_default();
        match k.as_str() {
            "knowledge.graph_import_enabled" => {
                enabled = !matches!(v.trim().to_lowercase().as_str(), "false" | "0" | "off");
            }
            "knowledge.graph_import_max_nodes" => {
                max_nodes = v.trim().parse().unwrap_or(2000);
            }
            _ => {}
        }
    }
    Ok(GraphImportConfig { enabled, max_nodes })
}

/// Campi normalizzati di un nodo di import pronti per l'INSERT in `wiki_docs`.
struct GraphNode {
    title: String,
    body: String,
    tags: Vec<String>,
    slug: String,
    body_hash: String,
}

/// Estrae e normalizza i campi di un nodo del grafo esterno. `None` se il nodo
/// e' privo di `id` (va saltato).
fn prepare_graph_node(n: &Value, source_id: &str) -> Option<GraphNode> {
    let ext_id = n
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if ext_id.is_empty() {
        return None;
    }
    let label = n
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or(&ext_id)
        .to_string();
    let title: String = label.chars().take(200).collect();
    let body = n
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or(&title)
        .to_string();
    let node_type = n.get("node_type").and_then(|v| v.as_str()).unwrap_or("");
    let mut tags: Vec<String> = Vec::new();
    if !node_type.is_empty() {
        tags.push(node_type.to_string());
    }
    tags.push(format!("ext:{source_id}"));

    // Slug stabile: includes ext_id per evitare collisioni.
    let slug = nexus_wiki::vault::slugify(&format!("imp-{source_id}-{ext_id}"));
    let body_hash = nexus_wiki::vault::sha256_hex(&body);
    Some(GraphNode {
        title,
        body,
        tags,
        slug,
        body_hash,
    })
}

/// Importa un singolo nodo del grafo esterno in `wiki_docs`.
///
/// I tre esiti sono ora TRE: `Ok(Some)` importato, `Ok(None)` saltato perche' il
/// nodo non porta un `id`, `Err` scrittura fallita. Il `res.ok()` di prima li
/// riduceva a due, e la conseguenza si leggeva nel conteggio finale: un import
/// contro un DB che rifiutava ogni riga usciva `ok: true, nodes_created: 0`,
/// indistinguibile da un grafo di soli nodi senza id.
async fn import_graph_node(
    ctx: &ToolContextCore,
    n: &Value,
    source_id: &str,
) -> Result<Option<Uuid>, sqlx::Error> {
    let Some(node) = prepare_graph_node(n, source_id) else {
        return Ok(None);
    };
    let id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO wiki_docs (
            scope, project_id, slug, title, body_md, body_hash,
            kind, intent, tags,
            edit_lock, protected_sections, manually_edited,
            current_version, auto_generated, public_read
        ) VALUES (
            'project', $1, $2, $3, $4, $5,
            'note', 'domain', $6,
            'none', '{}', FALSE,
            1, TRUE, FALSE
        )
        ON CONFLICT (scope, COALESCE(project_id::text,''), slug) DO UPDATE SET
            title=EXCLUDED.title, body_md=EXCLUDED.body_md, body_hash=EXCLUDED.body_hash,
            tags=EXCLUDED.tags, updated_at=NOW()
        RETURNING id
        "#,
    )
    .bind(ctx.project_id)
    .bind(&node.slug)
    .bind(&node.title)
    .bind(&node.body)
    .bind(&node.body_hash)
    .bind(&node.tags)
    .fetch_one(&*ctx.db)
    .await?;
    Ok(Some(id))
}

/// Traduce l'`edge_type` esterno verso il `rel_type` di `wiki_links`
/// (heuristica semplice).
fn map_edge_type_to_rel(etype: &str) -> &'static str {
    match etype {
        "depends_on" | "requires" | "needs" => "depends_on",
        "blocks" => "blocks",
        "blocked_by" => "blocked_by",
        "implements" => "implements",
        "tests" => "tests",
        "refines" | "refinement" => "refines",
        _ => "relates",
    }
}

/// Importa un singolo arco del grafo esterno in `wiki_links`, risolvendo gli
/// endpoint tramite `id_map`. `Ok(true)` = arco inserito, `Ok(false)` = arco
/// saltato perche' incompleto o cappio, `Err` = scrittura fallita.
///
/// Il `.is_ok()` di prima univa il salto legittimo e il guasto: un import in cui
/// nessun arco entrava per un errore di scrittura si presentava come un grafo di
/// soli nodi, che e' una forma di grafo perfettamente plausibile.
async fn import_graph_edge(
    ctx: &ToolContextCore,
    e: &Value,
    id_map: &std::collections::HashMap<String, Uuid>,
) -> Result<bool, sqlx::Error> {
    let source = e.get("source").and_then(|v| v.as_str()).unwrap_or("");
    let target = e.get("target").and_then(|v| v.as_str()).unwrap_or("");
    if source.is_empty() || target.is_empty() {
        return Ok(false);
    }
    let (Some(&f), Some(&t)) = (id_map.get(source), id_map.get(target)) else {
        return Ok(false);
    };
    if f == t {
        return Ok(false);
    }
    let etype = e
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    let rel = map_edge_type_to_rel(&etype);
    sqlx::query(
        r#"
        INSERT INTO wiki_links (from_doc_id, to_doc_id, rel_type, created_by, confidence, evidence)
        VALUES ($1, $2, $3, 'external', 1.0, 'imported via knowledge_import_graph')
        ON CONFLICT (from_doc_id, to_doc_id, rel_type) DO NOTHING
        "#,
    )
    .bind(f)
    .bind(t)
    .bind(rel)
    .execute(&*ctx.db)
    .await?;
    Ok(true)
}

/// I quattro numeri di un import: cio' che e' entrato e cio' che e' stato
/// SALTATO.
///
/// I salti esistono e sono LEGITTIMI — un nodo privo di `id`, un arco con un
/// estremo che nessun nodo ha risolto, un cappio — ma non uscivano da questa
/// funzione: il conteggio portava i soli creati, e un grafo di 50 nodi di cui 20
/// senza `id` si presentava come `nodes_created: 30, ok: true`, che l'agente
/// legge come «importato per intero». Il caso limite lo dice meglio: con TUTTI i
/// nodi privi di `id` usciva `nodes_created: 0, ok: true`, mentre il payload col
/// solo array `nodes` vuoto — stessa conseguenza, niente importato — e' un
/// fallimento dichiarato da [`parse_graph_payload`]. Stesso effetto, due verdetti
/// opposti.
///
/// La distinzione a tre esiti che [`import_graph_node`] ha appena guadagnato
/// moriva qui dentro: il terzo caso non aveva un campo dove arrivare.
#[derive(Default)]
struct EsitoImport {
    nodi_creati: usize,
    nodi_saltati: usize,
    archi_creati: usize,
    archi_saltati: usize,
}

impl EsitoImport {
    /// I quattro numeri come li legge l'agente. PUNTO UNICO della loro
    /// composizione (regola L): li portano sia il successo sia il corpo del
    /// fallimento parziale, e due scritture divergerebbero al primo campo nuovo.
    fn campi(&self) -> serde_json::Map<String, Value> {
        let mut corpo = serde_json::Map::new();
        corpo.insert("nodes_created".to_string(), json!(self.nodi_creati));
        corpo.insert("nodes_skipped".to_string(), json!(self.nodi_saltati));
        corpo.insert("edges_created".to_string(), json!(self.archi_creati));
        corpo.insert("edges_skipped".to_string(), json!(self.archi_saltati));
        corpo
    }
}

/// Esegue i due passi di import (nodi -> `wiki_docs`, archi -> `wiki_links`).
///
/// Si ferma alla prima scrittura fallita invece di proseguire contando zero: un
/// import parziale dichiarato riuscito e' un grafo che l'agente crede completo.
/// I conteggi viaggiano ANCHE nell'errore perche' cio' che e' entrato prima del
/// guasto resta scritto — non c'e' transazione — e un fallimento che tace su
/// quanto ha gia' modificato lascia l'agente a credere che la KB sia intatta.
async fn run_graph_import(
    ctx: &ToolContextCore,
    nodes_in: &[Value],
    edges_in: &[Value],
    source_id: &str,
) -> Result<EsitoImport, (EsitoImport, sqlx::Error)> {
    let mut id_map: std::collections::HashMap<String, Uuid> = std::collections::HashMap::new();
    let mut esito = EsitoImport::default();
    for n in nodes_in {
        match import_graph_node(ctx, n, source_id).await {
            Ok(Some(id)) => {
                let ext_id = n.get("id").and_then(|v| v.as_str()).unwrap_or("").trim();
                id_map.insert(ext_id.to_string(), id);
                esito.nodi_creati += 1;
            }
            Ok(None) => esito.nodi_saltati += 1,
            Err(e) => return Err((esito, e)),
        }
    }
    for arco in edges_in {
        match import_graph_edge(ctx, arco, &id_map).await {
            Ok(true) => esito.archi_creati += 1,
            Ok(false) => esito.archi_saltati += 1,
            Err(e) => return Err((esito, e)),
        }
    }
    Ok(esito)
}

/// Fa il parsing del payload JSON node-link e ne valida i nodi (non vuoti,
/// entro `max_nodes`). Ritorna `(nodes_in, edges_in)`.
///
/// Tutti i suoi fallimenti sono RIMEDIABILI: il grafo lo compone l'agente, e
/// ogni messaggio nomina cosa correggere nel payload che ha appena passato.
fn parse_graph_payload(
    content: &str,
    max_nodes: usize,
) -> Result<(Vec<Value>, Vec<Value>), RispostaTool> {
    let payload: Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(e) => {
            return Err(crate::errore_tool(
                format!("content non e' JSON valido: {e}"),
                NaturaFallimento::Rimediabile,
            ))
        }
    };
    let nodes_in = payload
        .get("nodes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let edges_in = payload
        .get("edges")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if nodes_in.is_empty() {
        return Err(crate::errore_tool(
            "nessun nodo nel grafo: content deve avere un array 'nodes' non vuoto, \
             con almeno un oggetto {id, label}",
            NaturaFallimento::Rimediabile,
        ));
    }
    if nodes_in.len() > max_nodes {
        return Err(crate::errore_tool(
            format!(
                "troppi nodi: {} contro il massimo di {max_nodes}; spezza il grafo \
                 in piu' chiamate",
                nodes_in.len()
            ),
            NaturaFallimento::Rimediabile,
        ));
    }
    Ok((nodes_in, edges_in))
}

/// I due controlli che precedono la lettura del grafo: che ci sia qualcosa da
/// importare, e che l'import sia acceso. Ritorna la config, che porta il tetto
/// ai nodi.
///
/// Le due cause sono di NATURA opposta e il tipo le tiene distinte: un `content`
/// vuoto lo corregge l'agente, un interruttore spento no.
async fn import_ammesso(
    ctx: &ToolContextCore,
    content: &str,
) -> Result<GraphImportConfig, RispostaTool> {
    if content.trim().is_empty() {
        return Err(crate::errore_tool(
            "content vuoto: passa il grafo come JSON node-link {\"nodes\":[...],\"edges\":[...]}",
            NaturaFallimento::Rimediabile,
        ));
    }
    let cfg = load_graph_import_config(ctx)
        .await
        .map_err(|e| errore_db("lettura della configurazione di import fallita", e))?;
    if !cfg.enabled {
        return Err(crate::errore_tool(
            "import grafi disabilitato dalla configurazione \
             (settings knowledge.graph_import_enabled=false)",
            NaturaFallimento::DelSistema,
        ));
    }
    Ok(cfg)
}

/// `knowledge_import_graph` — import di un grafo esterno nella KB.
/// Nodi -> `wiki_docs` (scope=project), archi -> `wiki_links`.
/// MIGRATO al contratto d'ingresso e a `RispostaTool` (regola Q).
///
/// Il solo formato e' JSON node-link
/// (`{"nodes":[{id,label,content?,node_type?}], "edges":[{source,target,type?}]}`):
/// il parser di Mermaid/DOT e' stato rimosso col modulo `knowledge/`. Il
/// rifiuto a RUNTIME dei due formati non implementati non c'e' piu' perche' non
/// serve piu': `format` e' un enum del contratto con un solo valore ammesso,
/// quindi mermaid e dot li ferma la deserializzazione — che e' esattamente la
/// divergenza che il contratto d'ingresso e' nato per chiudere (il catalogo li
/// prometteva, l'handler li respingeva un salto piu' in la').
pub async fn tool_knowledge_import_graph(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::KnowledgeImportGraphInput};

    let params = match KnowledgeImportGraphInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let cfg = match import_ammesso(ctx, &params.content).await {
        Ok(c) => c,
        Err(risposta) => return risposta,
    };
    let source_id = params
        .source_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("import")
        .to_string();

    let (nodes_in, edges_in) = match parse_graph_payload(&params.content, cfg.max_nodes) {
        Ok(v) => v,
        Err(risposta) => return risposta,
    };

    let esito = match run_graph_import(ctx, &nodes_in, &edges_in, &source_id).await {
        Ok(v) => v,
        Err((parziale, e)) => {
            let mut corpo = parziale.campi();
            corpo.insert(
                "error".to_string(),
                json!(format!("import del grafo fallito: {e}")),
            );
            corpo.insert(
                "hint".to_string(),
                json!("la KB e' stata modificata solo in parte: i nodi e gli archi contati qui \
                       sopra sono gia' scritti"),
            );
            return crate::errore_tool_con_dettagli(
                Value::Object(corpo),
                NaturaFallimento::DelSistema,
            );
        }
    };

    // `parse_graph_payload` garantisce almeno un nodo nel payload, quindi zero
    // creati significa che sono stati saltati TUTTI, e la causa e' una sola.
    // Prima usciva come `ok: true, nodes_created: 0`, cioe' lo stesso verdetto di
    // un import riuscito per un payload che non ha importato niente.
    if esito.nodi_creati == 0 {
        let mut corpo = esito.campi();
        corpo.insert(
            "error".to_string(),
            json!(format!(
                "nessun nodo importato: tutti i {} nodi del payload sono privi del campo \
                 'id', che e' la chiave con cui gli archi li risolvono",
                esito.nodi_saltati
            )),
        );
        return crate::errore_tool_con_dettagli(
            Value::Object(corpo),
            NaturaFallimento::Rimediabile,
        );
    }

    let format = params.format.come_stringa();
    tracing::info!(
        project_id = %ctx.project_id,
        format,
        nodes_created = esito.nodi_creati,
        nodes_skipped = esito.nodi_saltati,
        edges_created = esito.archi_creati,
        edges_skipped = esito.archi_saltati,
        "knowledge_import_graph: grafo esterno importato in wiki_docs/wiki_links"
    );

    let mut corpo = esito.campi();
    corpo.insert("ok".to_string(), json!(true));
    corpo.insert("format".to_string(), json!(format));
    corpo.insert("source_id".to_string(), json!(source_id));
    RispostaTool::riuscito(Value::Object(corpo).to_string())
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_mapping_roundtrip_known() {
        for r in KNOWLEDGE_REL_TYPES.iter() {
            let to_wiki = map_rel_to_wiki(r);
            let back = map_rel_from_wiki(to_wiki);
            assert_eq!(back, *r, "roundtrip rotto su {r}");
        }
    }

    #[test]
    fn rel_mapping_passes_through_unknown_wiki_rels() {
        // I rel emessi dai worker auto (mentions, implements, tests) non hanno
        // un equivalente "agent-facing" e devono passare tal quale al client.
        assert_eq!(map_rel_from_wiki("mentions"), "mentions");
        assert_eq!(map_rel_from_wiki("implements"), "implements");
        assert_eq!(map_rel_from_wiki("tests"), "tests");
    }

    #[test]
    fn rel_mapping_specific_translations() {
        assert_eq!(map_rel_to_wiki("correction"), "correction_of");
        assert_eq!(map_rel_to_wiki("refinement"), "refines");
        assert_eq!(map_rel_to_wiki("duplicate"), "duplicate_of");
        assert_eq!(map_rel_from_wiki("correction_of"), "correction");
        assert_eq!(map_rel_from_wiki("refines"), "refinement");
        assert_eq!(map_rel_from_wiki("duplicate_of"), "duplicate");
    }

    #[test]
    fn edge_type_mapping_known_and_default() {
        // heuristica di import: alias noti + default 'relates'.
        assert_eq!(map_edge_type_to_rel("depends_on"), "depends_on");
        assert_eq!(map_edge_type_to_rel("requires"), "depends_on");
        assert_eq!(map_edge_type_to_rel("needs"), "depends_on");
        assert_eq!(map_edge_type_to_rel("refinement"), "refines");
        assert_eq!(map_edge_type_to_rel("qualcosa_di_ignoto"), "relates");
    }

    /// IL PANIC. Un testo oltre il limite con un carattere multibyte a cavallo
    /// del confine faceva cadere il processo su `&text[..2000]`.
    ///
    /// La query mette una 'e' accentata ESATTAMENTE a cavallo del byte 2000:
    /// 1999 caratteri ASCII, poi due byte che il taglio spezzerebbe a meta'.
    ///
    /// MUTAZIONE che rende rosso: rimettere `&text[..EMBED_MAX_BYTES]` al posto
    /// del ciclo su `is_char_boundary`. Il test non fallisce con un assert: il
    /// processo PANICA con «byte index 2000 is not a char boundary», che e'
    /// esattamente cio' che accadeva a chi cercava con una query lunga in una
    /// lingua accentata.
    #[test]
    fn un_carattere_multibyte_sul_confine_non_fa_panicare_il_taglio() {
        let query = format!("{}è{}", "a".repeat(1999), "b".repeat(100));
        assert!(query.len() > EMBED_MAX_BYTES);
        assert!(
            !query.is_char_boundary(EMBED_MAX_BYTES),
            "il test non riproduce il caso se il byte 2000 e' gia' un confine valido"
        );

        let tagliato = embed_slice(&query);

        // Taglia PRIMA del limite, mai dopo: il limite dell'embedder e' in byte.
        assert!(tagliato.len() <= EMBED_MAX_BYTES);
        // E non spezza il carattere: il risultato resta uno `&str` valido, che
        // e' il solo modo in cui poteva esistere senza panicare.
        assert!(tagliato.chars().all(|c| c == 'a'));
        assert_eq!(tagliato.len(), 1999);
    }

    #[test]
    fn un_testo_entro_il_limite_non_viene_toccato() {
        let corto = "ricerca sui documenti del progetto";
        assert_eq!(embed_slice(corto), corto);
        let al_limite = "a".repeat(EMBED_MAX_BYTES);
        assert_eq!(embed_slice(&al_limite).len(), EMBED_MAX_BYTES);
    }

    // ── Il confine col contratto d'ingresso ────────────────────────────────
    // Questi test partono dal `Value` grezzo, che e' cio' che il dispatch
    // consegna: costruire a mano una `Knowledge*Input` fisserebbe l'assunto da
    // verificare, cioe' che la deserializzazione faccia quel che si crede
    // (regola O).

    /// Il catalogo promette «1-200 char» e il codice contava BYTE.
    ///
    /// MUTAZIONE che rende rosso: rimettere `title.len()` al posto di
    /// `title.chars().count()`. Un titolo di 200 lettere accentate occupa 400
    /// byte e veniva respinto a meta' del limite promesso, con un messaggio che
    /// citava un numero che l'agente vedeva rispettato.
    #[test]
    fn duecento_lettere_accentate_stanno_dentro_il_limite_del_titolo() {
        let titolo = "à".repeat(200);
        assert_eq!(titolo.len(), 400, "il test non riproduce il caso in ASCII");
        let letti = parse_create_note_params(&json!({
            "title": titolo,
            "body_md": "corpo della nota",
        }))
        .expect("200 caratteri sono dentro il limite che il catalogo promette");
        assert_eq!(letti.title.chars().count(), 200);
        // Il default storico dell'intent non cambia con l'enum del contratto.
        assert_eq!(letti.intent, "feature");
    }

    #[test]
    fn duecentouno_caratteri_restano_fuori_e_il_messaggio_dice_quanti() {
        let esito = parse_create_note_params(&json!({
            "title": "a".repeat(201),
            "body_md": "corpo",
        }))
        .expect_err("201 caratteri sforano");
        assert_eq!(esito.esito, nexus_types::tool_outcome::EsitoTool::Fallito);
        assert_eq!(esito.natura, Some(NaturaFallimento::Rimediabile));
        assert!(esito.testo.contains("201"), "{}", esito.testo);
    }

    /// La giunzione col contratto: un campo che lo schema non dichiara viene
    /// RIFIUTATO (`deny_unknown_fields`), e il fallimento nomina il tool.
    ///
    /// MUTAZIONE che rende rosso: togliere `deny_unknown_fields` da
    /// `tool_object!`. Prima della migrazione un campo di troppo passava in
    /// silenzio e l'agente credeva di aver ristretto qualcosa.
    #[test]
    fn un_campo_non_dichiarato_non_passa_dal_contratto() {
        let esito = parse_search_params(&json!({
            "query": "autenticazione",
            "intent": "decision",
        }))
        .expect_err("'intent' non e' un parametro di knowledge_search");
        assert_eq!(esito.natura, Some(NaturaFallimento::Rimediabile));
        assert!(esito.testo.contains("knowledge_search"), "{}", esito.testo);
    }

    #[test]
    fn il_filtro_relazioni_assente_vale_tutte_e_tradotte_nel_vocabolario_wiki() {
        use crate::input_contract::InputTool;

        let letti = KnowledgeGetSubgraphInput::leggi(&json!({"note_id": "x"}))
            .expect("nessun campo obbligatorio");
        let p = parse_subgraph_params(&letti);
        assert_eq!(p.rel_filter_wiki.len(), KNOWLEDGE_REL_TYPES.len());
        // Tradotte, non lasciate nel vocabolario agente-facing: e' la colonna
        // `wiki_links.rel_type` che la query interroga.
        assert!(p.rel_filter_wiki.contains(&"correction_of".to_string()));
        assert_eq!(p.depth, 2);
        assert_eq!(p.max_nodes, 30);

        let solo_blocchi =
            KnowledgeGetSubgraphInput::leggi(&json!({"rel_types": ["blocks", "blocked_by"]}))
                .expect("valori dentro il vocabolario");
        let p = parse_subgraph_params(&solo_blocchi);
        assert_eq!(p.rel_filter_wiki, vec!["blocks", "blocked_by"]);
    }

    /// Un payload senza nodi e' un fallimento RIMEDIABILE, e il messaggio dice
    /// cosa deve contenere `content`.
    #[test]
    fn un_grafo_senza_nodi_e_un_fallimento_che_dice_cosa_manca() {
        let esito = parse_graph_payload(r#"{"edges": []}"#, 10).expect_err("nessun nodo");
        assert_eq!(esito.natura, Some(NaturaFallimento::Rimediabile));
        assert!(esito.testo.contains("nodes"), "{}", esito.testo);

        let troppi = format!(r#"{{"nodes": [{}]}}"#, [r#"{"id":"a"}"#; 3].join(","));
        let esito = parse_graph_payload(&troppi, 2).expect_err("oltre il tetto");
        assert!(esito.testo.contains('3') && esito.testo.contains('2'));
    }

    /// I nodi SALTATI hanno un campo, e non spariscono nel conteggio dei creati.
    ///
    /// MUTAZIONE che rende rosso: togliere `nodes_skipped`/`edges_skipped` da
    /// [`EsitoImport::campi`]. Senza, un grafo di cui meta' dei nodi e' privo di
    /// `id` esce indistinguibile da uno importato per intero.
    #[test]
    fn i_salti_di_un_import_hanno_un_campo() {
        let esito = EsitoImport {
            nodi_creati: 30,
            nodi_saltati: 20,
            archi_creati: 4,
            archi_saltati: 11,
        };
        let campi = esito.campi();
        assert_eq!(campi["nodes_created"], json!(30));
        assert_eq!(campi["nodes_skipped"], json!(20));
        assert_eq!(campi["edges_created"], json!(4));
        assert_eq!(campi["edges_skipped"], json!(11));
    }
}
