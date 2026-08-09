//! Tool nexus_describe_image_attachment.
//!
//! Descrive un'immagine allegata alla chat usando un modello vision (Google
//! Gemini di default, configurato in nexus_purpose_model.vision_describe).
//!
//! Flusso:
//!   1) Recupera l'allegato dal DB filtrando per project_id.
//!   2) Verifica via magic-byte detection che il kind sia image_*.
//!   3) Verifica che size_bytes sia entro il limite DB
//!      (agent.attachment.image_max_bytes, default 2 MB).
//!   4) Legge il file, costruisce un data URI base64 e chiama il Nexus LLM
//!      Gateway (`POST /v1/complete`) con una richiesta MULTIMODALE (prompt
//!      testuale + blocco image_url), pinnando il provider/modello risolto dal
//!      purpose `vision_describe`. La chiamata e' tutta Rust: non passa piu'
//!      dal brain Python (/vision/describe rimosso).
//!   5) Parsa la risposta DESCRIZIONE:/OCR: e restituisce description +
//!      ocr_text + model_used al modello.
//!
//! L'esito viaggia nel CAMPO della `RispostaTool` (regola Q), non nel testo: i
//! modi in cui questo tool puo' fallire non sono la stessa cosa, e la `natura`
//! li distingue per l'agente che la legge — un id sbagliato lo corregge lui, un
//! limite in `settings` no, una risposta vuota del modello si ritenta.
//!
//! Niente hardcoded (regola G): il modello arriva dal purpose via routing
//! (resolve_purpose_via_http), il limite size da settings, l'URL del gateway
//! dalla porta nel DB. Il gateway possiede routing/cooldown/privacy e mappa il
//! blocco immagine al dialetto del provider (regola L: punto unico gateway).

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde_json::{json, Value};
use uuid::Uuid;

use super::attachment_inspector::{detect_kind, load_attachment, read_header, uuid_allegato};
use super::gateway_client::{gateway_vision_complete, GwVisionResult};
use super::ToolContextCore;
use nexus_auth::get_setting_checked;
use nexus_types::routing_client::resolve_purpose_via_http;
use nexus_types::tool_outcome::{NaturaFallimento, RispostaTool};

/// Purpose che mappa al modello vision (mig 0194). Punto unico di selezione del
/// modello: niente nome modello hardcoded (regola G).
const VISION_PURPOSE: &str = "vision_describe";
/// Token massimi della risposta vision (parita' col brain: 2048).
const VISION_MAX_TOKENS: u32 = 2048;
/// Default safe se il setting agent.attachment.image_max_bytes non e' impostato.
const IMAGE_MAX_BYTES_DEFAULT: usize = 2 * 1024 * 1024;

/// Prompt di default vision, parita' col brain (`_VISION_DEFAULT_PROMPT`):
/// impone il formato DESCRIZIONE:/OCR: che `parse_vision_response` separa.
const VISION_DEFAULT_PROMPT: &str = "Descrivi il contenuto visivo dell'immagine in italiano. \
Se contiene testo leggibile riporta tutti i testi nella sezione OCR. \
Formato risposta esatto: DESCRIZIONE: ...\nOCR: ... \
(riporta sezione OCR vuota se non c'e' testo).";

/// L'allegato gia' accertato IMMAGINE, pronto per il gateway: i campi che il
/// tool riportera' al modello piu' il data URI base64 del contenuto.
///
/// Esiste perche' i controlli d'ammissione (esiste? e' un'immagine? sta nel
/// limite?) sono cinque rami d'errore consecutivi: lasciati nell'handler lo
/// portavano oltre la soglia di lunghezza, e la natura di ciascuno finiva
/// mescolata alla composizione del risultato.
struct ImmagineAllegata {
    id: Uuid,
    file_name: String,
    /// MIME REALE dai magic byte, non quello dichiarato all'upload: e' quello
    /// che entra nel data URI ed e' quello che il tool riporta al modello.
    mime_reale: String,
    data_uri: String,
}

pub async fn tool_nexus_describe_image_attachment(
    ctx: &ToolContextCore,
    input: &Value,
) -> RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::NexusDescribeImageAttachmentInput};

    let params = match NexusDescribeImageAttachmentInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let attachment_id = match uuid_allegato(&params.attachment_id) {
        Ok(id) => id,
        Err(risposta) => return risposta,
    };
    // Una domanda fatta di soli spazi non e' una domanda: resta il prompt di
    // default, che e' l'unico a imporre il formato DESCRIZIONE:/OCR: su cui
    // `parse_vision_response` separa le due sezioni.
    let question = params
        .question
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let immagine = match immagine_da_allegato(ctx, attachment_id).await {
        Ok(i) => i,
        Err(risposta) => return risposta,
    };
    let result = match descrizione_vision(ctx, &immagine, question.as_deref()).await {
        Ok(r) => r,
        Err(risposta) => return risposta,
    };

    // Parsa DESCRIZIONE:/OCR: e restituisci al modello (parita' col brain).
    let (description, ocr_text) = parse_vision_response(&result.content);
    RispostaTool::riuscito(
        json!({
            "attachment_id": immagine.id.to_string(),
            "file_name": immagine.file_name,
            "mime_type": immagine.mime_reale,
            "description": description,
            "ocr_text": ocr_text.map(Value::String).unwrap_or(Value::Null),
            "model_used": result.model_used,
        })
        .to_string(),
    )
}

/// Lookup + magic-byte detection + limite di dimensione + lettura dei byte.
///
/// Le cinque nature sono DIVERSE e si dichiarano una per una — le due che
/// compongono un corpo con dettagli stanno accanto, in `rifiuto_non_immagine` e
/// `rifiuto_troppo_grande`:
/// - id che non risulta nel progetto: RIMEDIABILE, ed e' `nexus_list_attachments`
///   a dire quali id esistono;
/// - header illeggibile: il DB dichiara il file, lo storage non lo consegna. DEL
///   SISTEMA e non derivata dal `ErrorKind` perche' `read_header` lo ha gia'
///   appiattito in una `String`: ricavarlo dal messaggio sarebbe la regola M al
///   contrario, visto che quel testo e' localizzato e diverso fra Windows e Linux;
/// - lettura dei byte fallita: stesso evento, ma qui l'errore e' ancora tipizzato
///   e la natura la legge il `ErrorKind`.
async fn immagine_da_allegato(
    ctx: &ToolContextCore,
    attachment_id: Uuid,
) -> Result<ImmagineAllegata, RispostaTool> {
    let record = load_attachment(&ctx.db, attachment_id, ctx.project_id)
        .await
        .map_err(|e| crate::errore_tool(e, NaturaFallimento::Rimediabile))?;

    let header = match read_header(&record.file_path).await {
        Ok(h) => h,
        Err(e) => return Err(crate::errore_tool(e, NaturaFallimento::DelSistema)),
    };
    let (kind, mime_reale, _ext) = detect_kind(&header, &record.file_name, &record.mime_type);
    if !is_image_kind(&kind) {
        return Err(rifiuto_non_immagine(&kind));
    }

    // Limite size dal DB (no fallback nascosto: default safe documentato).
    let max_bytes = image_max_bytes(&ctx.db).await;
    if record.size_bytes < 0 || (record.size_bytes as usize) > max_bytes {
        return Err(rifiuto_troppo_grande(record.size_bytes, max_bytes));
    }

    // Il data URI base64 e' il formato `image_url` che il gateway mappa al
    // dialetto del provider. Qui l'errore di I/O e' ancora TIPIZZATO, quindi la
    // natura la legge il `ErrorKind` invece di sceglierla a mano (regola M).
    let bytes = match tokio::fs::read(&record.file_path).await {
        Ok(b) => b,
        Err(e) => {
            return Err(crate::errore_tool(
                format!("read fallita: {e}"),
                NaturaFallimento::da_errore_io(&e),
            ))
        }
    };
    let data_uri = format!("data:{mime_reale};base64,{}", B64.encode(&bytes));
    Ok(ImmagineAllegata {
        id: record.id,
        file_name: record.file_name,
        mime_reale,
        data_uri,
    })
}

/// Il rifiuto di un allegato che immagine non e'.
///
/// RIMEDIABILE, e il messaggio dice COME: prima si limitava a «usa il tool di
/// estrazione corretto per quel kind», che e' un rimando a un tool di cui non
/// diceva il nome. `nexus_inspect_attachment` quel nome lo restituisce nel campo
/// `next_action_recommended` (ADR 0012), quindi la rimediazione e' UNA chiamata
/// e non un giro di tentativi.
fn rifiuto_non_immagine(kind: &str) -> RispostaTool {
    let dettagli = json!({
        "error": format!(
            "L'allegato non e' un'immagine (kind rilevato: '{kind}'). Chiama \
             nexus_inspect_attachment su questo attachment_id: il campo \
             next_action_recommended nomina il tool di estrazione per questo kind."
        ),
        "kind": kind,
    });
    crate::errore_tool_con_dettagli(dettagli, NaturaFallimento::Rimediabile)
}

/// Il rifiuto di un'immagine oltre il limite di piattaforma.
///
/// DEL SISTEMA: l'immagine e' quella che e', e il limite vive in `settings`
/// (`agent.attachment.image_max_bytes`, mig 0194). Nessun parametro della
/// chiamata lo aggira, quindi la direttiva giusta e' cercare un'altra strada —
/// non ritentare, che rifallirebbe identico.
fn rifiuto_troppo_grande(size_bytes: i64, max_bytes: usize) -> RispostaTool {
    let dettagli = json!({
        "error": format!(
            "Immagine troppo grande ({size_bytes} byte, limite {max_bytes} byte). Configura \
             'agent.attachment.image_max_bytes' in settings se devi alzare il limite."
        ),
        "size_bytes": size_bytes,
        "max_bytes": max_bytes,
    });
    crate::errore_tool_con_dettagli(dettagli, NaturaFallimento::DelSistema)
}

/// Risolve il modello dal purpose e chiama il gateway in multimodale.
///
/// Il purpose e' il punto unico di selezione (regola G): se non risolve e' la
/// configurazione di piattaforma a mancare — DEL SISTEMA, e il messaggio nomina
/// la migrazione da verificare. Il fallimento della chiamata e' invece
/// TRANSITORIO: routing, cooldown e failover li possiede gia' il gateway, quindi
/// cio' che arriva fin qui e' il loro esaurimento.
async fn descrizione_vision(
    ctx: &ToolContextCore,
    immagine: &ImmagineAllegata,
    question: Option<&str>,
) -> Result<GwVisionResult, RispostaTool> {
    // resolve_purpose_via_http e' il punto unico cross-processo che interroga
    // il routing tier-only di mcp-core.
    let (provider, model) = match resolve_purpose_via_http(&ctx.db, VISION_PURPOSE).await {
        Ok(pm) => pm,
        Err(e) => {
            let messaggio = format!(
                "modello vision non risolvibile (purpose '{VISION_PURPOSE}'): {e}. \
                 Verifica nexus_purpose_model.vision_describe (mig 0194)."
            );
            return Err(crate::errore_tool(messaggio, NaturaFallimento::DelSistema));
        }
    };

    // Chiamata multimodale al gateway Rust (prompt + immagine). Il gateway
    // mappa il blocco image_url al dialetto del provider e gestisce
    // routing/cooldown/privacy (regola L: punto unico gateway).
    let content_blocks = json!([
        { "type": "text", "text": question.unwrap_or(VISION_DEFAULT_PROMPT) },
        { "type": "image_url", "image_url": { "url": immagine.data_uri } },
    ]);

    let result = match gateway_vision_complete(
        &ctx.db,
        &provider,
        &model,
        content_blocks,
        VISION_MAX_TOKENS,
        VISION_PURPOSE,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return Err(crate::errore_tool(
                format!("chiamata vision via gateway fallita: {e}"),
                NaturaFallimento::Transitorio,
            ));
        }
    };

    if result.content.trim().is_empty() {
        return Err(rifiuto_risposta_vuota(&result, &immagine.file_name));
    }
    Ok(result)
}

/// Il rifiuto di una completion VUOTA.
///
/// RAMO NUDO CHIUSO: usciva come successo, con `description: ""` — cioe'
/// l'agente leggeva «questa immagine non contiene nulla» dove il modello non
/// aveva prodotto nulla, e su un mockup o uno screenshot quella e' una
/// conclusione falsa su cui poi prosegue. Il gateway non la puo' distinguere per
/// suo conto: risponde HTTP 200 come per una risposta piena, quindi il caso si
/// riconosce solo qui, dove si sa cosa si era chiesto.
///
/// TRANSITORIO: ripetere identica la chiamata la fa ripassare da routing e
/// failover del gateway, che e' proprio cio' che serve a una completion vuota.
fn rifiuto_risposta_vuota(result: &GwVisionResult, file_name: &str) -> RispostaTool {
    let dettagli = json!({
        "error": format!(
            "il modello vision ({}) ha restituito una risposta vuota: nessuna \
             descrizione prodotta per '{file_name}'. Non significa che l'immagine sia vuota.",
            result.model_used
        ),
        "model_used": result.model_used,
    });
    crate::errore_tool_con_dettagli(dettagli, NaturaFallimento::Transitorio)
}

fn is_image_kind(kind: &str) -> bool {
    matches!(kind, "png" | "jpeg" | "gif" | "webp" | "svg" | "image")
}

/// Separa il payload `DESCRIZIONE: ...\nOCR: ...` in `(descrizione, ocr)`.
/// Parita' col brain (`_parse_vision_response`): se il modello non rispetta il
/// formato, ritorna l'intero testo come descrizione e `ocr = None`.
fn parse_vision_response(text: &str) -> (String, Option<String>) {
    if text.is_empty() {
        return (String::new(), None);
    }
    let upper = text.to_uppercase();
    let desc_idx = match upper.find("DESCRIZIONE:") {
        Some(i) => i,
        None => return (text.trim().to_string(), None),
    };
    let desc_start = desc_idx + "DESCRIZIONE:".len();
    let ocr_idx = upper.find("OCR:");
    match ocr_idx {
        Some(o) if o >= desc_idx => {
            let description = text[desc_start..o].trim().to_string();
            let ocr_text = text[o + "OCR:".len()..].trim();
            let ocr_value = if ocr_text.is_empty() {
                None
            } else {
                Some(ocr_text.to_string())
            };
            (description, ocr_value)
        }
        _ => (text[desc_start..].trim().to_string(), None),
    }
}

/// Legge agent.attachment.image_max_bytes da settings. Se mancante o DB
/// down, ritorna il default safe documentato (2 MB) e logga WARN.
async fn image_max_bytes(db: &sqlx::PgPool) -> usize {
    match get_setting_checked(db, "agent.attachment.image_max_bytes").await {
        Ok(Some(raw)) => match raw.trim().parse::<usize>() {
            Ok(v) if v > 0 => v,
            _ => {
                tracing::warn!(
                    raw = %raw,
                    "vision_tools: 'agent.attachment.image_max_bytes' non parsabile, uso default {}",
                    IMAGE_MAX_BYTES_DEFAULT
                );
                IMAGE_MAX_BYTES_DEFAULT
            }
        },
        Ok(None) => IMAGE_MAX_BYTES_DEFAULT,
        Err(e) => {
            tracing::warn!(error = %e, "vision_tools: lettura settings fallita, uso default");
            IMAGE_MAX_BYTES_DEFAULT
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_formato_completo_descrizione_e_ocr() {
        let (desc, ocr) = parse_vision_response("DESCRIZIONE: un gatto\nOCR: ciao mondo");
        assert_eq!(desc, "un gatto");
        assert_eq!(ocr.as_deref(), Some("ciao mondo"));
    }

    #[test]
    fn parse_ocr_vuoto_diventa_none() {
        let (desc, ocr) = parse_vision_response("DESCRIZIONE: solo immagine\nOCR:");
        assert_eq!(desc, "solo immagine");
        assert!(ocr.is_none());
    }

    #[test]
    fn parse_senza_marcatori_tutto_descrizione() {
        let (desc, ocr) = parse_vision_response("  testo libero senza formato  ");
        assert_eq!(desc, "testo libero senza formato");
        assert!(ocr.is_none());
    }

    #[test]
    fn parse_senza_ocr_solo_descrizione() {
        let (desc, ocr) = parse_vision_response("DESCRIZIONE: paesaggio montano");
        assert_eq!(desc, "paesaggio montano");
        assert!(ocr.is_none());
    }

    #[test]
    fn parse_testo_vuoto() {
        let (desc, ocr) = parse_vision_response("");
        assert!(desc.is_empty());
        assert!(ocr.is_none());
    }

    #[test]
    fn parse_case_insensitive_sui_marcatori() {
        // I marcatori sono cercati case-insensitive (upper), ma il testo
        // restituito mantiene il case originale.
        let (desc, ocr) = parse_vision_response("descrizione: Logo Blu\nocr: NEXUS");
        assert_eq!(desc, "Logo Blu");
        assert_eq!(ocr.as_deref(), Some("NEXUS"));
    }
}
