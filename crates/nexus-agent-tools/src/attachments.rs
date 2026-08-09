//! Tool nexus_list_attachments / nexus_read_attachment.
//!
//! Permettono all'agente di scoprire gli allegati caricati dall'utente nel
//! turno corrente (o in qualsiasi turno della sessione) e di leggerli a
//! richiesta, in modalita' streaming-friendly (offset+length).
//!
//! Vedi ADR 0010 e migrazione 0192.

use std::io::SeekFrom;

use base64::Engine;
use serde_json::{json, Value};
use sqlx::Row;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use uuid::Uuid;

use nexus_types::tool_outcome::{NaturaFallimento, RispostaTool};

use super::attachment_inspector::{load_attachment, uuid_allegato};
use super::read_cache::{self, ReadCacheKey, ReadKind};
use super::ToolContextCore;

/// Max bytes leggibili in una singola chiamata di nexus_read_attachment.
const MAX_READ_BYTES: usize = 102_400; // 100 KB

/// MIME considerati testuali nonostante non inizino con "text/".
const TEXT_LIKE_MIMES: &[&str] = &[
    "application/json",
    "application/xml",
    "application/x-sh",
    "application/x-makefile",
    "application/javascript",
    "application/yaml",
    "application/toml",
    "application/x-yaml",
];

/// Quale sessione elencare: quella CHIESTA, altrimenti quella del contesto.
///
/// Il contratto dichiara `session_id` come stringa opzionale e non puo' dire
/// «uuid»: quel controllo resta qui. Entrambi i rami sono RIMEDIABILI e il
/// messaggio dice come — un uuid malformato si riscrive, e un contesto senza
/// sessione si supera passando il parametro.
fn sessione_da_elencare(
    ctx: &ToolContextCore,
    chiesta: Option<&str>,
) -> Result<Uuid, RispostaTool> {
    match chiesta {
        Some(s) => Uuid::parse_str(s).map_err(|_| {
            crate::errore_tool(
                format!(
                    "Parametro 'session_id' ('{s}') non e' un UUID valido: passa l'UUID di \
                     una sessione chat, oppure ometti il parametro per usare quella corrente."
                ),
                NaturaFallimento::Rimediabile,
            )
        }),
        None => ctx.session_id.ok_or_else(|| {
            crate::errore_tool(
                "Nessuna session_id nel contesto: passa 'session_id' esplicito, \
                 l'UUID della sessione chat di cui vuoi gli allegati.",
                NaturaFallimento::Rimediabile,
            )
        }),
    }
}

/// Una riga della lista. Una colonna illeggibile degrada al vuoto: la riga
/// c'e', ed e' meglio di un elenco che si interrompe a meta'.
///
/// `created_at` fa eccezione perche' per un istante il vuoto non esiste, e
/// l'ignoto non prende un valore comodo (regola Q): un `now()` di ripiego
/// sarebbe indistinguibile da una data vera, cioe' un dato INVENTATO in un
/// campo su cui l'agente decide quale allegato e' l'ultimo caricato. L'assenza
/// si dichiara, e il posto per dichiararla e' `null`.
fn riga_allegato(r: &sqlx::postgres::PgRow) -> Value {
    let id: Uuid = r.try_get("id").unwrap_or_else(|_| Uuid::nil());
    let file_name: String = r.try_get("file_name").unwrap_or_default();
    let mime_type: String = r.try_get("mime_type").unwrap_or_default();
    let size_bytes: i64 = r.try_get("size_bytes").unwrap_or(0);
    let kind: String = r.try_get("kind").unwrap_or_default();
    let created_at = r
        .try_get::<chrono::DateTime<chrono::Utc>, _>("created_at")
        .map(|d| Value::String(d.to_rfc3339()))
        .unwrap_or(Value::Null);
    json!({
        "id": id.to_string(),
        "file_name": file_name,
        "mime_type": mime_type,
        "size_bytes": size_bytes,
        "kind": kind,
        "created_at": created_at,
    })
}

/// Lista gli allegati di una sessione chat.
///
/// Input: { "session_id": <uuid?> } — opzionale, default = ctx.session_id.
pub async fn tool_nexus_list_attachments(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::NexusListAttachmentsInput};

    let params = match NexusListAttachmentsInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let session_id = match sessione_da_elencare(ctx, params.session_id.as_deref()) {
        Ok(s) => s,
        Err(risposta) => return risposta,
    };

    let rows = sqlx::query(
        "SELECT a.id, a.file_name, a.mime_type, a.size_bytes, a.kind, a.created_at \
         FROM chat_message_attachments a \
         JOIN chat_messages m ON m.id = a.message_id \
         WHERE m.session_id = $1 AND a.project_id = $2 \
         ORDER BY a.created_at ASC",
    )
    .bind(session_id)
    .bind(ctx.project_id)
    .fetch_all(&*ctx.db)
    .await;

    match rows {
        // Una sessione SENZA allegati e' una risposta, non un guasto: l'elenco
        // vuoto esce come successo con `count: 0`, e l'agente sa che non c'e'
        // nulla da leggere invece di cercare un errore che non esiste.
        Ok(rows) => {
            let items: Vec<Value> = rows.iter().map(riga_allegato).collect();
            let corpo = json!({
                "session_id": session_id.to_string(),
                "count": items.len(),
                "attachments": items,
            });
            RispostaTool::riuscito(corpo.to_string())
        }
        // La query non ha girato: nessuna riformulazione della chiamata la fa
        // girare, e ripeterla identica rifallisce.
        Err(err) => {
            tracing::warn!(error=%err, "nexus_list_attachments: query fallita");
            crate::errore_tool(
                format!("Errore lettura allegati: {err}"),
                NaturaFallimento::DelSistema,
            )
        }
    }
}

/// Gli estremi della finestra da leggere, con il cap gia' applicato.
///
/// Il contratto dichiara due interi e non puo' dire «non negativi»: quel
/// controllo resta qui, ed e' RIMEDIABILE col campo e il valore nel messaggio.
/// Prima `Value::as_u64` scartava un negativo INSIEME al parametro, e la lettura
/// ripartiva silenziosamente da capo con la finestra di default.
fn estremi_lettura(
    offset: Option<i64>,
    length: Option<i64>,
) -> Result<(u64, usize), RispostaTool> {
    let negativo = |campo: &str, v: i64| {
        crate::errore_tool(
            format!("Parametro '{campo}' negativo ({v}): usa un intero >= 0."),
            NaturaFallimento::Rimediabile,
        )
    };
    let offset = match offset {
        Some(v) if v < 0 => return Err(negativo("offset", v)),
        Some(v) => v as u64,
        None => 0,
    };
    let length = match length {
        Some(v) if v < 0 => return Err(negativo("length", v)),
        Some(v) => (v as usize).min(MAX_READ_BYTES),
        None => MAX_READ_BYTES,
    };
    Ok((offset, length))
}

/// L'allegato non e' arrivato.
///
/// [`load_attachment`] appiattisce in `String` sia l'id che non risulta sia la
/// query che non ha girato, quindi qui la causa non e' piu' distinguibile.
/// RIMEDIABILE come per gli altri tool di estrazione (`documento_da_allegato`):
/// il caso dominante e' l'id sbagliato, e il messaggio nomina il tool che
/// restituisce quelli veri — dire «rimediabile» senza dire come sarebbe una
/// promessa non mantenuta.
fn allegato_non_disponibile(e: String) -> RispostaTool {
    crate::errore_tool(
        format!("{e}. Usa nexus_list_attachments per gli id degli allegati di questa sessione."),
        NaturaFallimento::Rimediabile,
    )
}

/// L'unica leva che l'agente ha su un errore di I/O di questo tool.
///
/// Il percorso del file non viene dalla chiamata — lo porta il DB — quindi
/// «prova un altro percorso» qui non vuol dire niente: cio' che si puo'
/// cambiare e' l'allegato.
const COME_CAMBIARE_ALLEGATO: &str =
    "Il percorso viene dal DB e non dalla tua chiamata: verifica con nexus_list_attachments \
     che l'allegato esista ancora, oppure passa un altro attachment_id.";

/// Un errore del filesystem sull'allegato: la natura viene dal `ErrorKind`
/// (regola M), mai dal messaggio del sistema operativo, che e' localizzato e
/// diverso fra Windows e Linux.
///
/// Dove la natura e' RIMEDIABILE il testo deve dire COME, o la dichiarazione e'
/// una promessa non mantenuta (vedi il doc di [`NaturaFallimento::Rimediabile`]):
/// un allegato che il DB dichiara e il disco non consegna e' `NotFound`, e da
/// solo «Impossibile aprire il file 'C:\\...\\storage\\ab12.bin'» non dice
/// all'agente nulla che possa fare. La direttiva si aggiunge SOLO li': sulle
/// altre nature la sua ce l'ha gia' [`NaturaFallimento::direttiva`], e sarebbero
/// due indicazioni contrarie nello stesso messaggio.
fn errore_io(cosa: String, e: &std::io::Error) -> RispostaTool {
    let natura = NaturaFallimento::da_errore_io(e);
    let come = match natura {
        NaturaFallimento::Rimediabile => format!(" {COME_CAMBIARE_ALLEGATO}"),
        _ => String::new(),
    };
    crate::errore_tool(format!("{cosa}: {e}{come}"), natura)
}

/// Legge un range di byte da un allegato e ritorna testo o base64.
///
/// Input: { "attachment_id": <uuid>, "encoding"?: "auto|text|base64",
///          "offset"?: u64, "length"?: usize }
pub async fn tool_nexus_read_attachment(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::NexusReadAttachmentInput};

    let params = match NexusReadAttachmentInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let attachment_id = match uuid_allegato(&params.attachment_id) {
        Ok(id) => id,
        Err(risposta) => return risposta,
    };
    // Il vocabolario dell'encoding viene dal contratto: un valore fuori elenco
    // non arriva qui, lo ferma la deserializzazione, e il messaggio che serde
    // compone elenca gia' i valori ammessi.
    let encoding_req = params
        .encoding
        .map(|e| e.come_stringa())
        .unwrap_or("auto")
        .to_string();
    let (offset, effective_length) = match estremi_lettura(params.offset, params.length) {
        Ok(estremi) => estremi,
        Err(risposta) => return risposta,
    };

    // La lookup delega al punto unico (`load_attachment`), che fa la stessa
    // query — incluso il rifiuto del `file_path` vuoto in DB — per l'ispettore,
    // gli estrattori di documenti e gli archivi.
    let record = match load_attachment(&ctx.db, attachment_id, ctx.project_id).await {
        Ok(r) => r,
        Err(e) => return allegato_non_disponibile(e),
    };
    let total_size = record.size_bytes.max(0) as u64;
    let file_path = record.file_path.to_string_lossy().into_owned();
    let mime_type = record.mime_type;
    let file_name = record.file_name;

    // FIX 2 (ADR 0012): deduplica via read_cache. La key include
    // attachment_id, offset, length, encoding richiesto. Cache hit > 1 = hint
    // al modello di cambiare strategia (passare a tool di estrazione struttur.).
    let cache_key = ReadCacheKey {
        attachment_id,
        kind: ReadKind::Attachment,
        entry_path: None,
        offset,
        length: effective_length as u64,
        encoding: encoding_req.clone(),
    };
    read_cache::get_or_compute(&ctx.db, cache_key, move || async move {
        read_attachment_raw(
            attachment_id,
            file_path,
            mime_type,
            file_name,
            offset,
            effective_length,
            total_size,
            encoding_req,
        )
        .await
    })
    .await
}

/// In che forma esce il contenuto letto, e con quale etichetta.
///
/// Estratta perche' [`read_attachment_raw`] resti leggibile: la decisione
/// (richiesta esplicita, altrimenti il MIME) e il ripiego a base64 sui byte non
/// UTF-8 sono una cosa sola, e nessun chiamante ne vuole una meta'.
fn codifica(buf: Vec<u8>, mime_type: &str, encoding_req: &str) -> (String, &'static str) {
    let is_text_like = mime_type.starts_with("text/")
        || TEXT_LIKE_MIMES
            .iter()
            .any(|m| mime_type.eq_ignore_ascii_case(m));
    let come_testo = match encoding_req {
        "text" => true,
        "base64" => false,
        _ => is_text_like,
    };
    if come_testo {
        return match String::from_utf8(buf) {
            Ok(s) => (s, "text"),
            // Ripiego a base64 se i byte non sono UTF-8 validi: l'errore
            // restituisce i byte originali, che non vanno quindi clonati prima.
            Err(e) => (
                base64::engine::general_purpose::STANDARD.encode(e.as_bytes()),
                "base64",
            ),
        };
    }
    (
        base64::engine::general_purpose::STANDARD.encode(&buf),
        "base64",
    )
}

/// Lettura raw senza cache (chiamata dal closure di `read_cache::get_or_compute`).
#[allow(clippy::too_many_arguments)]
async fn read_attachment_raw(
    attachment_id: Uuid,
    file_path: String,
    mime_type: String,
    file_name: String,
    offset: u64,
    effective_length: usize,
    total_size: u64,
    encoding_req: String,
) -> RispostaTool {
    let effective_length = if total_size > 0 {
        let remaining = total_size.saturating_sub(offset) as usize;
        effective_length.min(remaining)
    } else {
        effective_length
    };

    // Apri file e seek.
    let mut file = match tokio::fs::File::open(&file_path).await {
        Ok(f) => f,
        Err(err) => return errore_io(format!("Impossibile aprire il file '{file_path}'"), &err),
    };
    if offset > 0 {
        if let Err(err) = file.seek(SeekFrom::Start(offset)).await {
            return errore_io(format!("Seek fallita a offset {offset}"), &err);
        }
    }

    let mut buf: Vec<u8> = vec![0u8; effective_length];
    let read_bytes = match file.read(&mut buf).await {
        Ok(n) => n,
        Err(err) => return errore_io("Errore lettura file".to_string(), &err),
    };
    buf.truncate(read_bytes);

    let (content, encoding_label) = codifica(buf, &mime_type, &encoding_req);
    let truncated = (offset + read_bytes as u64) < total_size;

    let corpo = json!({
        "id": attachment_id.to_string(),
        "name": file_name,
        "mime_type": mime_type,
        "encoding": encoding_label,
        "offset": offset,
        "length": read_bytes,
        "total_size": total_size,
        "truncated": truncated,
        "content": content,
    });
    RispostaTool::riuscito(corpo.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_core::{NoopEmbedder, NoopMutationHooks};
    use nexus_types::tool_outcome::{is_tool_failure, EsitoTool, RispostaTool};
    use std::sync::Arc;

    /// Contesto reale (la struct di produzione). Il pool e' lazy e non viene mai
    /// contattato: i rami d'errore qui esercitati rifiutano l'input PRIMA di
    /// toccare il DB, ed e' il percorso che il modello incontra per primo quando
    /// sbaglia una chiamata.
    fn ctx_di_prova(root: std::path::PathBuf) -> ToolContextCore {
        let db =
            sqlx::PgPool::connect_lazy("postgres://test:test@127.0.0.1:1/test").expect("pool lazy");
        ToolContextCore {
            root_path: root,
            user_id: Uuid::nil(),
            is_git_repo: false,
            can_write: true,
            project_id: Uuid::nil(),
            session_id: None,
            db: Arc::new(db.clone()),
            run_db: Arc::new(db),
            parent_run_id: None,
            run_id: None,
            long_running_patterns: Vec::new(),
            user_role: "admin".to_string(),
            is_nexus_operator: true,
            project_channels: Arc::new(dashmap::DashMap::new()),
            monitor_registry: Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new())),
            hooks: Arc::new(NoopMutationHooks),
            embedder: Arc::new(NoopEmbedder),
            isolated_subrun: false,
            write_scope: Vec::new(),
        }
    }

    /// I tool che rispondono in JSON dichiaravano il fallimento SOLO nel campo
    /// `error` del corpo: al confine del dispatch quel corpo diventa una `String`
    /// e `RispostaTool::da_testo_legacy` non ha nulla da leggere, quindi il tool
    /// risulta RIUSCITO. Un allegato che rifiuta l'estrazione a ogni tentativo
    /// diventa cosi' una ripetizione produttiva, e l'anti-loop la classifica come
    /// stallo invece che come causa radice da diagnosticare (regola M).
    ///
    /// Il test attraversa i PRODUTTORI veri — le funzioni che il dispatch chiama —
    /// e arriva alla CONSEGUENZA, l'esito nel campo, non alla stringa.
    /// Mutazione che rende rosso: sostituire un `crate::errore_tool` con un
    /// `RispostaTool::riuscito` in uno qualunque dei rami «parametro mancante».
    #[tokio::test]
    async fn l_errore_di_un_tool_json_e_dichiarato_alla_macchina() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ctx_di_prova(dir.path().to_path_buf());

        // Tutti MIGRATI: la proprieta' e' dichiarata dove non si puo' perdere.
        // Il marker non c'e' piu' e non deve esserci — il corpo torna a essere
        // un JSON integro — quindi il criterio e' il CAMPO, e in piu' c'e' la
        // natura, che il canale legacy non poteva trasportare.
        let migrati: Vec<(&str, RispostaTool)> = vec![
            (
                "nexus_extract_pdf_text",
                crate::document_tools::tool_nexus_extract_pdf_text(&ctx, &json!({})).await,
            ),
            (
                "nexus_list_archive_entries",
                crate::archive_tools::tool_nexus_list_archive_entries(&ctx, &json!({})).await,
            ),
            (
                "nexus_read_attachment",
                tool_nexus_read_attachment(&ctx, &json!({})).await,
            ),
            (
                "nexus_read_archive_entry",
                crate::archive_tools::tool_nexus_read_archive_entry(&ctx, &json!({})).await,
            ),
        ];

        for (nome, uscita) in migrati {
            assert_eq!(
                uscita.esito,
                EsitoTool::Fallito,
                "{nome} non dichiara il fallimento nel campo: {uscita:?}"
            );
            assert_eq!(
                uscita.natura,
                Some(nexus_types::tool_outcome::NaturaFallimento::Rimediabile),
                "{nome}: un parametro mancante lo corregge l'agente: {uscita:?}"
            );
            assert!(uscita.testo.contains("attachment_id"), "{nome}: {uscita:?}");
            assert!(
                !is_tool_failure(&uscita.testo),
                "{nome}: il marker non deve piu' comparire nel testo di un tool migrato,                  o il corpo JSON resta spezzato: {uscita:?}"
            );
        }
    }

    /// Il percorso di SUCCESSO non e' toccato, e non e' un dettaglio: il presidio
    /// del budget letture allegati
    /// (`nexus-agent-graph::decisions::tool_dispatch::extract_returned_bytes`)
    /// deserializza questo testo e cerca l'intero `length` di primo livello. Un
    /// marker in testa renderebbe il corpo non deserializzabile, quindi la
    /// proprieta' va fissata qui, sul produttore vero: se un domani una lettura
    /// riuscita venisse marcata, il budget smetterebbe di contare i byte letti e
    /// nessun test dell'altro crate se ne accorgerebbe, perche' li' l'input e'
    /// costruito a mano (regola O).
    #[tokio::test]
    async fn la_lettura_riuscita_resta_un_json_con_length_intero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("nota.txt");
        std::fs::write(&file, "0123456789").expect("seed");

        let uscita = read_attachment_raw(
            Uuid::nil(),
            file.to_string_lossy().into_owned(),
            "text/plain".to_string(),
            "nota.txt".to_string(),
            0,
            MAX_READ_BYTES,
            10,
            "auto".to_string(),
        )
        .await;

        assert_eq!(
            uscita.esito,
            EsitoTool::Riuscito,
            "lettura riuscita dichiarata fallita: {uscita:?}"
        );
        assert!(
            !is_tool_failure(&uscita.testo),
            "il marker non deve comparire nel testo di un tool migrato: {uscita:?}"
        );
        let corpo: Value =
            serde_json::from_str(&uscita.testo).expect("il successo resta JSON integro");
        assert_eq!(
            corpo.get("length").and_then(Value::as_i64),
            Some(10),
            "il campo su cui il budget conta i byte letti: {uscita:?}"
        );
    }

    /// La faccia opposta: una lettura FALLITA non deve poter dichiarare byte
    /// letti. Il budget ne contava gia' zero — nessun `length` nel corpo
    /// d'errore — e continua a contarne zero.
    ///
    /// La natura viene dal `ErrorKind` e non dal messaggio del sistema operativo
    /// (regola M): un file che non c'e' e' `NotFound`, cioe' RIMEDIABILE — un
    /// altro allegato, un altro percorso. MUTAZIONE: fissando `DelSistema` a
    /// mano in `errore_io`, l'agente riceve «cambia strada» per un id che puo'
    /// correggere, e questo test rosseggia.
    #[tokio::test]
    async fn la_lettura_fallita_non_dichiara_byte_letti() {
        let dir = tempfile::tempdir().expect("tempdir");
        let uscita = read_attachment_raw(
            Uuid::nil(),
            dir.path().join("assente.bin").to_string_lossy().into_owned(),
            "application/octet-stream".to_string(),
            "assente.bin".to_string(),
            0,
            MAX_READ_BYTES,
            10,
            "auto".to_string(),
        )
        .await;

        assert_eq!(
            uscita.esito,
            EsitoTool::Fallito,
            "apertura fallita non dichiarata: {uscita:?}"
        );
        assert_eq!(
            uscita.natura,
            Some(nexus_types::tool_outcome::NaturaFallimento::Rimediabile),
            "un file che non c'e' e' NotFound, e l'agente puo' cambiare id: {uscita:?}"
        );
        assert!(
            !uscita.testo.contains("\"length\""),
            "un errore non porta byte letti: {uscita:?}"
        );
    }

    /// Un elenco VUOTO non e' un fallimento: la sessione senza allegati e' una
    /// risposta, e degradarla a errore manderebbe l'agente a cercare una causa
    /// che non esiste. Qui la sessione non c'e' affatto — il contesto non ne ha
    /// e il parametro non e' passato — ed e' invece un fallimento RIMEDIABILE,
    /// col messaggio che nomina il parametro da passare.
    #[tokio::test]
    async fn senza_sessione_il_tool_dice_quale_parametro_passare() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ctx_di_prova(dir.path().to_path_buf());

        let uscita = tool_nexus_list_attachments(&ctx, &json!({})).await;
        assert_eq!(uscita.esito, EsitoTool::Fallito, "{uscita:?}");
        assert_eq!(
            uscita.natura,
            Some(nexus_types::tool_outcome::NaturaFallimento::Rimediabile),
            "passare 'session_id' e' cosa che l'agente puo' fare: {uscita:?}"
        );
        assert!(uscita.testo.contains("session_id"), "{uscita:?}");

        // E un uuid malformato viene fermato PRIMA della query, con il valore
        // rifiutato nel messaggio.
        let uscita = tool_nexus_list_attachments(&ctx, &json!({"session_id": "non-un-uuid"})).await;
        assert_eq!(uscita.esito, EsitoTool::Fallito, "{uscita:?}");
        assert!(uscita.testo.contains("non-un-uuid"), "{uscita:?}");
    }

    /// Gli estremi negativi non vengono piu' scartati in silenzio insieme al
    /// parametro: il contratto dichiara due interi e non puo' dire «non
    /// negativi», quindi il rifiuto vive qui e nomina campo e valore.
    ///
    /// MUTAZIONE: riportando `estremi_lettura` a un `max(0)`, un `offset: -5`
    /// torna a leggere dall'inizio senza dirlo e questo test rosseggia.
    #[test]
    fn un_estremo_negativo_e_rifiutato_invece_che_ignorato() {
        let errore = estremi_lettura(Some(-5), None).expect_err("offset negativo");
        assert_eq!(
            errore.natura,
            Some(nexus_types::tool_outcome::NaturaFallimento::Rimediabile)
        );
        assert!(errore.testo.contains("offset"), "{}", errore.testo);
        assert!(errore.testo.contains("-5"), "{}", errore.testo);

        estremi_lettura(None, Some(-1)).expect_err("length negativa");

        // Il cap resta applicato dove il valore e' legittimo.
        let (offset, length) =
            estremi_lettura(Some(10), Some(MAX_READ_BYTES as i64 * 4)).expect("estremi validi");
        assert_eq!(offset, 10);
        assert_eq!(length, MAX_READ_BYTES, "il cap e' il tetto della finestra");

        let (offset, length) = estremi_lettura(None, None).expect("estremi di default");
        assert_eq!((offset, length), (0, MAX_READ_BYTES));
    }

    /// Lo stesso rifiuto visto DAL TOOL, cioe' per la strada che percorre il
    /// modello (regola O): il contratto deserializza `-5` come `i64` — non lo
    /// scarta ne' lo rifiuta lui — e il controllo che lo ferma e' quello sopra.
    /// La prova sta in piedi senza DB perche' gli estremi si leggono PRIMA
    /// della lookup dell'allegato, ed e' anche il motivo per cui l'ordine dei
    /// due passi conta: invertirli farebbe pagare una query per un input che si
    /// sapeva gia' sbagliato.
    ///
    /// MUTAZIONE: dichiarando `offset` come `u64` nel contratto, il messaggio
    /// smette di nominare il valore rifiutato e questo test rosseggia — mentre
    /// quello sulla funzione pura resterebbe verde, perche' non passa di li'.
    #[tokio::test]
    async fn un_offset_negativo_e_rifiutato_prima_di_toccare_il_db() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ctx_di_prova(dir.path().to_path_buf());

        let uscita = tool_nexus_read_attachment(
            &ctx,
            &json!({"attachment_id": Uuid::nil().to_string(), "offset": -5}),
        )
        .await;

        assert_eq!(uscita.esito, EsitoTool::Fallito, "{uscita:?}");
        assert_eq!(
            uscita.natura,
            Some(nexus_types::tool_outcome::NaturaFallimento::Rimediabile),
            "un estremo sbagliato lo corregge l'agente: {uscita:?}"
        );
        assert!(uscita.testo.contains("offset"), "{uscita:?}");
        assert!(uscita.testo.contains("-5"), "{uscita:?}");
    }
}
