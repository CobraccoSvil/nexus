//! Tool audio: nexus_transcribe_audio (speech-to-text) e nexus_text_to_speech
//! (text-to-speech).
//!
//! `nexus_transcribe_audio` trascrive un audio allegato alla chat usando un
//! modello audio-in (OpenAI whisper di default, configurato in
//! nexus_purpose_model.transcribe_audio).
//!
//! Flusso (gemello di vision_tools, ma INPUT audio):
//!   1) Recupera l'allegato dal DB filtrando per project_id (regola E).
//!   2) Verifica via magic-byte detection che il kind sia audio_*.
//!   3) Verifica che size_bytes sia entro il limite DB
//!      (agent.attachment.audio_max_bytes, default 25 MB).
//!   4) Legge il file, lo codifica base64 e chiama il Nexus LLM Gateway
//!      (`POST /v1/audio/transcriptions`) pinnando il provider/modello risolto
//!      dal purpose `transcribe_audio`. La chiamata e' tutta Rust.
//!   5) Ritorna { text, model_used } al modello.
//!
//! `nexus_text_to_speech` fa l'inverso (OUTPUT audio, gemello di
//! image_tools::tool_nexus_generate_image): converte un testo in un file audio e
//! lo salva path-safe nel progetto (`.nexus/generated/<nome>.<ext>`), risolvendo
//! il modello dal purpose `text_to_speech`. Il formato e' fissato a mp3: il
//! catalogo non dichiara un parametro con cui l'agente possa sceglierlo, e
//! l'estensione di salvataggio deve restare coerente col MIME prodotto.
//!
//! Niente hardcoded (regola G): il modello arriva dal purpose via routing
//! (resolve_purpose_via_http), il limite size da settings, l'URL del gateway
//! dalla porta nel DB. Il gateway possiede routing/cooldown/privacy e mappa la
//! richiesta al dialetto del provider (regola L: punto unico gateway).
//!
//! MIGRATI al contratto d'ingresso e a `RispostaTool` (regola Q): l'esito sta
//! nel campo `esito` e la NATURA del fallimento accanto, invece che in un marker
//! anteposto al testo. Le nature seguono il gemello gia' migrato
//! `image_tools::tool_nexus_generate_image`, perche' la domanda e' la stessa
//! (regola L): permesso negato e purpose non risolvibile sono del SISTEMA, un
//! parametro sbagliato e' RIMEDIABILE, cio' che arriva dal gateway e'
//! l'esaurimento del suo cooldown/failover ed e' TRANSITORIO.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde_json::{json, Value};
use uuid::Uuid;

use nexus_types::tool_outcome::{NaturaFallimento, RispostaTool};

use super::attachment_inspector::{
    detect_kind, load_attachment, read_header, uuid_allegato, AttachmentRecord,
};
use super::gateway_client::{
    gateway_text_to_speech, gateway_transcribe_audio, GwCaller, GwTranscribeOut, GwTtsOut,
};
use super::ToolContextCore;
use nexus_auth::get_setting_checked;
use nexus_types::routing_client::resolve_purpose_via_http;
use nexus_types::workspace_paths::resolve_workspace_target;

/// Purpose che mappa al modello audio-in (mig 0480, tier=light,
/// required_capability=audio_in). Punto unico di selezione del modello: niente
/// nome modello hardcoded (regola G).
const AUDIO_PURPOSE: &str = "transcribe_audio";
/// Default safe se il setting agent.attachment.audio_max_bytes non e' impostato.
/// 25 MB e' il limite dell'API OpenAI /audio/transcriptions.
const AUDIO_MAX_BYTES_DEFAULT: usize = 25 * 1024 * 1024;
/// Chiave del limite in `settings`. Costante perche' la nominano sia il lettore
/// sia i messaggi che mandano a correggerla: tre letterali divergerebbero.
const AUDIO_MAX_BYTES_KEY: &str = "agent.attachment.audio_max_bytes";

/// Purpose che mappa al modello audio-out (mig 0481, tier=light,
/// required_capability=audio_out). Punto unico di selezione del modello TTS:
/// niente nome modello hardcoded (regola G).
const TTS_PURPOSE: &str = "text_to_speech";
/// Subdir sotto la project_root dove salvare gli audio generati (parita' con
/// image_tools: stesso punto di output per gli artefatti media).
const GENERATED_SUBDIR: &str = ".nexus/generated";
/// Formato/estensione audio di default (l'API OpenAI /audio/speech emette mp3 se
/// non specificato altrimenti). Documentato, non un magic fallback di modello.
const TTS_DEFAULT_FORMAT: &str = "mp3";

/// `nexus_transcribe_audio(attachment_id, language?)`.
pub async fn tool_nexus_transcribe_audio(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::NexusTranscribeAudioInput};

    let params = match NexusTranscribeAudioInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let attachment_id = match uuid_allegato(&params.attachment_id) {
        Ok(id) => id,
        Err(risposta) => return risposta,
    };
    // Una lingua fatta di soli spazi equivale a non averla dichiarata: inoltrarla
    // al provider come codice ISO vuoto e' un modo di sbagliare in silenzio.
    let language = params
        .language
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let (record, mime_reale) = match audio_trascrivibile(ctx, attachment_id).await {
        Ok(coppia) => coppia,
        Err(risposta) => return risposta,
    };

    // Qui il `ErrorKind` c'e' ancora, quindi la natura la legge lui (regola M) e
    // non il messaggio del sistema operativo, che e' localizzato e cambia fra
    // Windows e Linux.
    let bytes = match tokio::fs::read(&record.file_path).await {
        Ok(b) => b,
        Err(e) => {
            let natura = NaturaFallimento::da_errore_io(&e);
            return crate::errore_tool(format!("read fallita: {e}"), natura);
        }
    };

    let result = match trascrivi(ctx, B64.encode(&bytes), mime_reale.clone(), language).await {
        Ok(r) => r,
        Err(risposta) => return risposta,
    };

    RispostaTool::riuscito(
        json!({
            "attachment_id": record.id.to_string(),
            "file_name": record.file_name,
            "mime_type": mime_reale,
            "text": result.text,
            "model_used": result.model_used,
        })
        .to_string(),
    )
}

/// L'allegato e' un audio che si puo' mandare al gateway? Ritorna il record e il
/// MIME REALE (quello dei magic byte, non quello dichiarato dall'upload).
///
/// Le nature sono diverse e per questo la funzione le distingue invece di darne
/// una sola: un id che non risulta nel progetto lo corregge l'agente, un file
/// che il DB dichiara e il filesystem non consegna e' un guasto dello storage,
/// un allegato che non e' audio e' rimediabile PERCHE' il messaggio nomina il
/// tool con cui scoprire l'estrattore giusto.
async fn audio_trascrivibile(
    ctx: &ToolContextCore,
    attachment_id: Uuid,
) -> Result<(AttachmentRecord, String), RispostaTool> {
    // Stessa scelta di `documento_da_allegato`: l'helper appiattisce in una
    // `String` sia «non trovato» sia «query fallita», e fra i due il caso vivo e'
    // l'id sbagliato — che `nexus_list_attachments` risolve.
    let record = load_attachment(&ctx.db, attachment_id, ctx.project_id)
        .await
        .map_err(|e| crate::errore_tool(e, NaturaFallimento::Rimediabile))?;

    // DEL SISTEMA e non derivata dal `ErrorKind`: `read_header` ha gia'
    // appiattito l'errore di I/O in una `String`, quindi il kind qui non c'e'
    // piu' e ricavarlo dal messaggio sarebbe la regola M al contrario.
    let header = read_header(&record.file_path)
        .await
        .map_err(|e| crate::errore_tool(e, NaturaFallimento::DelSistema))?;
    let (kind, mime_reale, _ext) = detect_kind(&header, &record.file_name, &record.mime_type);
    if !is_audio_kind(&kind) {
        return Err(kind_non_audio(&kind));
    }
    limite_dimensione(&ctx.db, &record).await?;
    Ok((record, mime_reale))
}

/// Il fallimento di un allegato che non e' audio.
///
/// RIMEDIABILE, e il messaggio dice COME: prima mandava a «usare il tool di
/// estrazione corretto» senza nominarne nessuno, cioe' prometteva un rimedio e
/// non lo consegnava. `nexus_inspect_attachment` risponde con
/// `next_action_recommended`, che e' esattamente il dato mancante.
fn kind_non_audio(kind: &str) -> RispostaTool {
    let dettagli = json!({
        "error": format!(
            "L'allegato non e' un audio (kind rilevato: '{kind}'). Chiama \
             nexus_inspect_attachment su questo attachment_id: il campo \
             next_action_recommended nomina il tool di estrazione per questo kind."
        ),
        "kind": kind,
    });
    crate::errore_tool_con_dettagli(dettagli, NaturaFallimento::Rimediabile)
}

/// La dimensione dichiarata dal DB sta dentro il limite configurato?
async fn limite_dimensione(
    db: &sqlx::PgPool,
    record: &AttachmentRecord,
) -> Result<(), RispostaTool> {
    let max_bytes = audio_max_bytes(db).await?;
    if record.size_bytes < 0 {
        return Err(dimensione_non_attendibile(record.size_bytes));
    }
    if record.size_bytes as usize > max_bytes {
        return Err(audio_oltre_limite(record.size_bytes, max_bytes));
    }
    Ok(())
}

/// Dimensione negativa in DB: NON e' «troppo grande».
///
/// Il ramo esisteva gia' (`size_bytes < 0` cadeva nello stesso `if` del limite)
/// ma diceva il falso: mostrava una dimensione negativa accanto a un limite e
/// mandava ad alzarlo, quando nessun limite ammette una dimensione negativa.
/// DEL SISTEMA: il dato in colonna e' inattendibile e l'agente non lo scrive.
fn dimensione_non_attendibile(size_bytes: i64) -> RispostaTool {
    let dettagli = json!({
        "error": format!(
            "L'allegato dichiara una dimensione negativa in DB ({size_bytes} byte): dato \
             non attendibile, l'audio non viene inviato al provider."
        ),
        "size_bytes": size_bytes,
    });
    crate::errore_tool_con_dettagli(dettagli, NaturaFallimento::DelSistema)
}

/// Audio oltre il limite configurato. DEL SISTEMA: ne' alzare una setting ne'
/// rimpicciolire un allegato gia' caricato sono cose che l'agente possa fare, e
/// ripetere la chiamata rifallirebbe identica.
fn audio_oltre_limite(size_bytes: i64, max_bytes: usize) -> RispostaTool {
    let dettagli = json!({
        "error": format!(
            "Audio troppo grande ({size_bytes} byte, limite {max_bytes} byte). Il limite e' \
             la setting '{AUDIO_MAX_BYTES_KEY}': alzarlo e' una decisione di configurazione, \
             non un parametro di questa chiamata."
        ),
        "size_bytes": size_bytes,
        "max_bytes": max_bytes,
    });
    crate::errore_tool_con_dettagli(dettagli, NaturaFallimento::DelSistema)
}

/// True se il kind rilevato dal magic-byte detection e' audio (parita' con i kind
/// emessi da `mime_to_kind` in attachment_inspector: mp3/wav + generico audio).
fn is_audio_kind(kind: &str) -> bool {
    matches!(kind, "mp3" | "wav" | "audio")
}

/// Il limite in byte da `settings`, oppure il fallimento che lo spiega.
///
/// La chiave ASSENTE resta il default documentato: 25 MB e' il limite dell'API
/// /audio/transcriptions, quindi non configurarla e' una scelta legittima e il
/// default non e' un fallback nascosto. Gli altri due casi NON ricadono piu' li'
/// (regola Q: l'ignoto non degrada a «va bene»). Un valore non numerico e' un
/// errore dell'operatore che il WARN nascondeva applicando un limite che nessuno
/// aveva chiesto; un DB che non risponde renderebbe comunque impossibili i due
/// passi successivi (`resolve_purpose_via_http` e la risoluzione porta/token del
/// gateway leggono entrambi dal DB), quindi dichiararlo qui e' la stessa
/// interruzione, detta prima e con la causa giusta invece che con un limite
/// inventato scritto nel messaggio d'errore.
async fn audio_max_bytes(db: &sqlx::PgPool) -> Result<usize, RispostaTool> {
    match get_setting_checked(db, AUDIO_MAX_BYTES_KEY).await {
        Ok(None) => Ok(AUDIO_MAX_BYTES_DEFAULT),
        Ok(Some(raw)) => raw
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|v| *v > 0)
            .ok_or_else(|| limite_non_configurato(&raw)),
        Err(e) => Err(crate::errore_tool(
            format!("lettura della setting '{AUDIO_MAX_BYTES_KEY}' fallita: {e}"),
            NaturaFallimento::DelSistema,
        )),
    }
}

/// Il limite c'e' in tabella ma non e' un numero di byte positivo. DEL SISTEMA:
/// e' configurazione di piattaforma, fuori dalla portata dell'agente.
fn limite_non_configurato(raw: &str) -> RispostaTool {
    crate::errore_tool(
        format!(
            "la setting '{AUDIO_MAX_BYTES_KEY}' non contiene un numero di byte positivo \
             (valore in DB: '{raw}'): va corretta prima di poter trascrivere un audio."
        ),
        NaturaFallimento::DelSistema,
    )
}

/// Risolve il modello audio-in dal purpose e trascrive via gateway.
///
/// Le due nature sono diverse: un purpose che non risolve e' configurazione di
/// piattaforma e nessun parametro della chiamata la aggira; cio' che arriva dal
/// gateway e' invece l'esaurimento del suo cooldown/failover — un fornitore
/// saturo o irraggiungibile, dove ritentare piu' tardi e' la strategia giusta.
/// Stessa lettura del gemello `image_tools::tool_nexus_generate_image`.
async fn trascrivi(
    ctx: &ToolContextCore,
    audio_base64: String,
    mime: String,
    language: Option<String>,
) -> Result<GwTranscribeOut, RispostaTool> {
    let (provider, model) = resolve_purpose_via_http(&ctx.db, AUDIO_PURPOSE)
        .await
        .map_err(|e| {
            crate::errore_tool(
                format!(
                    "modello audio-in non risolvibile (purpose '{AUDIO_PURPOSE}'): {e}. \
                     Verifica nexus_purpose_model.transcribe_audio (mig 0480) e che un \
                     modello audio-in sia abilitato nel catalog."
                ),
                NaturaFallimento::DelSistema,
            )
        })?;
    gateway_transcribe_audio(
        &ctx.db,
        &provider,
        &model,
        audio_base64,
        Some(mime),
        language,
        AUDIO_PURPOSE,
        // Identita' del chiamante: senza, il gateway scarta la riga di ledger e
        // le trascrizioni restano fuori dalla contabilita'.
        &GwCaller {
            user_id: ctx.user_id,
            project_id: ctx.project_id,
            run_id: ctx.run_id,
        },
    )
    .await
    .map_err(|e| {
        crate::errore_tool(
            format!("trascrizione audio via gateway fallita: {e}"),
            NaturaFallimento::Transitorio,
        )
    })
}

// ── nexus_text_to_speech (OUTPUT audio) ──────────────────────────────────────

/// Converte un testo in un file audio e lo salva path-safe nel progetto.
///
/// Flusso gemello di image_tools::tool_nexus_generate_image (OUTPUT-file):
///   1) Gate ctx.can_write (il tool PRODUCE un file su disco).
///   2) Risolve provider/model dal purpose `text_to_speech` via routing
///      (resolve_purpose_via_http): NESSUN nome modello hardcoded (regola G).
///   3) Chiama il gateway (`POST /v1/audio/speech`) pinnando il provider risolto.
///   4) Decodifica il base64 e salva l'audio path-safe sotto la project_root
///      (resolve_workspace_target, regola E), con estensione coerente col MIME
///      prodotto dal provider.
///   5) Ritorna { audio_path, model_used }.
pub async fn tool_nexus_text_to_speech(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::NexusTextToSpeechInput};

    // 1) Permesso di scrittura obbligatorio: il tool PRODUCE un file su disco.
    //    DEL SISTEMA: e' una decisione del progetto sul run, non un parametro.
    if !ctx.can_write {
        return crate::errore_tool(
            "Permesso di scrittura non concesso: impossibile salvare l'audio generato su \
             disco. Esegui in una modalita' che consente la scrittura file.",
            NaturaFallimento::DelSistema,
        );
    }

    let params = match NexusTextToSpeechInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    // Il contratto pretende che il campo CI SIA; che non sia fatto di soli spazi
    // non lo puo' dire, e quel controllo resta qui.
    let text = params.text.trim().to_string();
    if text.is_empty() {
        return crate::errore_tool(
            "Parametro 'text' vuoto: serve il testo da convertire in audio.",
            NaturaFallimento::Rimediabile,
        );
    }
    let voice = params
        .voice
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let filename = params
        .filename
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let result = match sintetizza(ctx, &text, voice).await {
        Ok(r) => r,
        Err(risposta) => return risposta,
    };
    let audio_path = match salva_audio_generato(ctx, &result, filename).await {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };

    RispostaTool::riuscito(
        json!({
            "audio_path": audio_path,
            "model_used": result.model_used,
        })
        .to_string(),
    )
}

/// Risolve il modello audio-out dal purpose e sintetizza via gateway. Nature
/// come in [`trascrivi`]: configurazione contro esaurimento dei fornitori.
async fn sintetizza(
    ctx: &ToolContextCore,
    text: &str,
    voice: Option<String>,
) -> Result<GwTtsOut, RispostaTool> {
    let (provider, model) = resolve_purpose_via_http(&ctx.db, TTS_PURPOSE)
        .await
        .map_err(|e| {
            crate::errore_tool(
                format!(
                    "modello audio-out non risolvibile (purpose '{TTS_PURPOSE}'): {e}. \
                     Verifica nexus_purpose_model.text_to_speech (mig 0481) e che un \
                     modello audio-out sia abilitato nel catalog."
                ),
                NaturaFallimento::DelSistema,
            )
        })?;
    gateway_text_to_speech(
        &ctx.db,
        &provider,
        &model,
        text,
        voice,
        // Formato fissato: l'estensione di salvataggio deve restare coerente con
        // cio' che il provider produce, e il catalogo non dichiara un parametro
        // con cui l'agente possa sceglierlo.
        Some(TTS_DEFAULT_FORMAT.to_string()),
        TTS_PURPOSE,
        &GwCaller {
            user_id: ctx.user_id,
            project_id: ctx.project_id,
            run_id: ctx.run_id,
        },
    )
    .await
    .map_err(|e| {
        crate::errore_tool(
            format!("sintesi vocale via gateway fallita: {e}"),
            NaturaFallimento::Transitorio,
        )
    })
}

/// Decodifica l'audio ricevuto dal gateway e lo salva path-safe nel progetto.
async fn salva_audio_generato(
    ctx: &ToolContextCore,
    result: &GwTtsOut,
    filename: Option<&str>,
) -> Result<String, RispostaTool> {
    let bytes = byte_audio(result)?;
    // Estensione dal MIME REALE prodotto dal provider; il formato richiesto e'
    // il ripiego quando quel MIME non e' fra quelli noti.
    let ext = ext_from_mime(&result.mime).unwrap_or(TTS_DEFAULT_FORMAT);
    save_audio(&ctx.root_path, &bytes, filename, ext)
        .await
        .map_err(|e| {
            // DEL SISTEMA e non derivata dal `ErrorKind`: `save_audio`
            // appiattisce gia' l'errore di I/O in una `String`, quindi il kind
            // qui non c'e' piu'. Fra le due letture possibili questa manda a
            // cercare un'altra strada invece di far risintetizzare l'audio —
            // che costa una chiamata al provider — contro un disco che non lo
            // accettera' comunque. Stessa scelta del gemello image_tools.
            crate::errore_tool(
                format!("salvataggio audio fallito: {e}"),
                NaturaFallimento::DelSistema,
            )
        })
}

/// I byte dell'audio dal base64 del gateway.
///
/// Entrambi i casi sono DEL SISTEMA: un base64 malformato e un audio vuoto sono
/// cio' che il provider ha risposto, e nessuna riformulazione del testo li
/// cambia. Stessa famiglia del «solo una URL temporanea» del gemello
/// image_tools: il tool non puo' salvare cio' che non ha ricevuto.
fn byte_audio(result: &GwTtsOut) -> Result<Vec<u8>, RispostaTool> {
    let bytes = B64.decode(result.audio_base64.trim()).map_err(|e| {
        crate::errore_tool(
            format!("decodifica audio base64 fallita: {e}"),
            NaturaFallimento::DelSistema,
        )
    })?;
    if bytes.is_empty() {
        return Err(audio_vuoto(&result.model_used));
    }
    Ok(bytes)
}

/// Il gateway ha risposto senza audio: fallimento che il codice dichiarava gia',
/// qui con la natura accanto invece che nel solo testo.
fn audio_vuoto(model_used: &str) -> RispostaTool {
    let dettagli = json!({
        "error": "il gateway ha restituito un audio vuoto.",
        "model_used": model_used,
    });
    crate::errore_tool_con_dettagli(dettagli, NaturaFallimento::DelSistema)
}

/// Estensione file dal MIME audio prodotto dal provider. `None` per MIME non
/// riconosciuto (il chiamante ricade sul formato richiesto). Funzione PURA.
fn ext_from_mime(mime: &str) -> Option<&'static str> {
    let m = mime.split(';').next().unwrap_or(mime).trim().to_lowercase();
    let ext = match m.as_str() {
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/opus" => "opus",
        "audio/ogg" => "ogg",
        "audio/aac" => "aac",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/pcm" | "audio/l16" => "pcm",
        _ => return None,
    };
    Some(ext)
}

/// Costruisce un nome file sicuro per l'audio generato. Se l'agente passa
/// `filename`, ne usa SOLO il basename (niente directory ne' traversal: la
/// path-safety finale e' garantita da `resolve_workspace_target`) forzando
/// l'estensione `ext`; altrimenti genera un nome timestampato. Gemella di
/// image_tools::build_filename (regola L: stesso pattern di naming output).
fn build_audio_filename(requested: Option<&str>, ext: &str) -> String {
    let stem = requested
        .map(sanitize_stem)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f");
            format!("speech_{ts}")
        });
    format!("{stem}.{ext}")
}

/// Riduce un nome richiesto al solo basename, elimina l'estensione e tiene solo
/// caratteri innocui ([A-Za-z0-9._-]). Difesa-in-profondita': il confinamento
/// vero e' in resolve_workspace_target.
fn sanitize_stem(requested: &str) -> String {
    let base = requested
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(requested)
        .trim();
    let stem = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base);
    stem.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches(['.', '_', '-'])
        .to_string()
}

/// Salva i byte audio sotto la project_root (path-safe) e ritorna il path
/// relativo pulito. Usa `resolve_workspace_target` (punto unico path-safety,
/// regola L): rifiuta traversal e qualunque target fuori dalla root (regola E).
async fn save_audio(
    root: &std::path::Path,
    bytes: &[u8],
    requested_name: Option<&str>,
    ext: &str,
) -> Result<String, String> {
    let name = build_audio_filename(requested_name, ext);
    let rel = format!("{GENERATED_SUBDIR}/{name}");
    let (clean_rel, abs_target) = resolve_workspace_target(root, &rel)
        .map_err(|e| format!("path audio non valido: {}", e.message()))?;

    if let Some(parent) = abs_target.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("creazione dir audio fallita: {e}"))?;
    }
    tokio::fs::write(&abs_target, bytes)
        .await
        .map_err(|e| format!("scrittura audio fallita: {e}"))?;
    Ok(clean_rel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_types::tool_outcome::EsitoTool;

    #[test]
    fn is_audio_kind_riconosce_audio_e_scarta_altro() {
        assert!(is_audio_kind("mp3"));
        assert!(is_audio_kind("wav"));
        assert!(is_audio_kind("audio"));
        // Non-audio: scartati (l'agente viene rimandato al tool corretto).
        assert!(!is_audio_kind("png"));
        assert!(!is_audio_kind("pdf"));
        assert!(!is_audio_kind("binary"));
        assert!(!is_audio_kind("video"));
    }

    /// Un allegato che non e' audio e' RIMEDIABILE, e il messaggio nomina cio'
    /// che serve per rimediare.
    ///
    /// MUTAZIONE: togliendo il nome del tool dal messaggio, la natura resta
    /// «rimediabile» ma la promessa non e' piu' mantenuta — e questo test
    /// rosseggia sulla riga che lo cerca.
    #[test]
    fn un_allegato_non_audio_dice_come_rimediare() {
        let out = kind_non_audio("pdf");
        assert_eq!(out.esito, EsitoTool::Fallito);
        assert_eq!(out.natura, Some(NaturaFallimento::Rimediabile));
        assert!(
            out.testo.contains("nexus_inspect_attachment"),
            "il messaggio nomina il tool con cui rimediare: {}",
            out.testo
        );
    }

    /// Limite e dimensione inattendibile sono DUE fallimenti, non uno: prima
    /// cadevano nello stesso ramo e una dimensione negativa veniva raccontata
    /// come «troppo grande» con l'invito ad alzare il limite.
    #[test]
    fn dimensione_negativa_e_oltre_limite_restano_distinti() {
        let negativa = dimensione_non_attendibile(-1);
        assert_eq!(negativa.natura, Some(NaturaFallimento::DelSistema));
        assert!(
            !negativa.testo.contains("troppo grande"),
            "un dato inattendibile non e' un file troppo grande: {}",
            negativa.testo
        );

        let oltre = audio_oltre_limite(30_000_000, AUDIO_MAX_BYTES_DEFAULT);
        assert_eq!(oltre.natura, Some(NaturaFallimento::DelSistema));
        assert!(oltre.testo.contains(AUDIO_MAX_BYTES_KEY), "{}", oltre.testo);
    }

    /// Un limite scritto male in tabella e' un errore di configurazione
    /// DICHIARATO, non un WARN seguito da un limite che nessuno ha chiesto.
    #[test]
    fn un_limite_non_numerico_e_del_sistema() {
        let out = limite_non_configurato("tanti");
        assert_eq!(out.esito, EsitoTool::Fallito);
        assert_eq!(out.natura, Some(NaturaFallimento::DelSistema));
        assert!(out.testo.contains("tanti"), "{}", out.testo);
    }

    #[test]
    fn ext_from_mime_mappa_audio_out() {
        assert_eq!(ext_from_mime("audio/mpeg"), Some("mp3"));
        // Content-Type con charset/parametri -> ignorati.
        assert_eq!(ext_from_mime("audio/mpeg; charset=binary"), Some("mp3"));
        assert_eq!(ext_from_mime("audio/wav"), Some("wav"));
        assert_eq!(ext_from_mime("audio/opus"), Some("opus"));
        assert_eq!(ext_from_mime("audio/flac"), Some("flac"));
        // MIME sconosciuto -> None (il chiamante ricade sul formato richiesto).
        assert_eq!(ext_from_mime("application/octet-stream"), None);
    }

    /// Un audio vuoto dal gateway resta un FALLIMENTO, ora dichiarato nel campo.
    #[test]
    fn un_audio_vuoto_non_diventa_un_successo() {
        let result = GwTtsOut {
            audio_base64: String::new(),
            mime: "audio/mpeg".to_string(),
            model_used: "prov/modello".to_string(),
        };
        let errore = byte_audio(&result).expect_err("un audio vuoto non e' salvabile");
        assert_eq!(errore.esito, EsitoTool::Fallito);
        assert_eq!(errore.natura, Some(NaturaFallimento::DelSistema));
        assert!(errore.testo.contains("prov/modello"), "{}", errore.testo);
    }

    #[test]
    fn build_audio_filename_default_e_timestamp() {
        let name = build_audio_filename(None, "mp3");
        assert!(name.starts_with("speech_"));
        assert!(name.ends_with(".mp3"));
    }

    #[test]
    fn build_audio_filename_forza_estensione_e_strippa_directory() {
        // L'estensione richiesta viene sostituita; le directory nel nome scartate.
        assert_eq!(build_audio_filename(Some("voce.wav"), "mp3"), "voce.mp3");
        assert_eq!(
            build_audio_filename(Some("../../etc/passwd"), "mp3"),
            "passwd.mp3"
        );
    }

    #[test]
    fn sanitize_stem_nome_solo_simboli_diventa_vuoto() {
        assert_eq!(sanitize_stem("..."), "");
        let name = build_audio_filename(Some("..."), "mp3");
        assert!(name.starts_with("speech_"));
    }

    #[tokio::test]
    async fn save_audio_confina_nella_root_e_crea_dir() {
        let tmp = std::env::temp_dir().join(format!("tts_test_{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&tmp).await.unwrap();
        let mp3 = b"ID3_fake_body";
        let rel = save_audio(&tmp, mp3, Some("saluto"), "mp3").await.unwrap();
        assert_eq!(rel, format!("{GENERATED_SUBDIR}/saluto.mp3"));
        let written = tokio::fs::read(tmp.join(&rel)).await.unwrap();
        assert_eq!(written, mp3);
        tokio::fs::remove_dir_all(&tmp).await.ok();
    }

    #[tokio::test]
    async fn save_audio_rifiuta_traversal_anche_se_nome_lo_contiene() {
        let tmp = std::env::temp_dir().join(format!("tts_test_{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&tmp).await.unwrap();
        let rel = save_audio(&tmp, b"x", Some("../../../escape"), "mp3")
            .await
            .unwrap();
        assert_eq!(rel, format!("{GENERATED_SUBDIR}/escape.mp3"));
        tokio::fs::remove_dir_all(&tmp).await.ok();
    }
}
