//! Tool nexus_generate_video.
//!
//! Genera un video dal prompt dell'agente e lo salva path-safe dentro la
//! project_root, sotto `.nexus/generated/<nome>.mp4`. Gemello di
//! image_tools::tool_nexus_generate_image (OUTPUT-file) ma per il backend ASYNC
//! Veo: il gateway incapsula il poll-loop (start + poll con timeout DB-driven),
//! quindi dal punto di vista del tool la chiamata resta sincrona (regola H: il
//! timeout HTTP del client e' dimensionato >= del poll-loop lato gateway).
//!
//! Flusso:
//!   1) Verifica ctx.can_write (il tool produce un file su disco): senza
//!      permesso di scrittura ritorna un errore esplicito (regola H).
//!   2) Risolve provider/model dal purpose `generate_video` via routing
//!      (resolve_purpose_via_http): NESSUN nome modello hardcoded (regola G).
//!   3) Chiama il Nexus LLM Gateway (`POST /v1/videos`) pinnando il provider
//!      risolto; il gateway possiede routing/cooldown/privacy + il poll-loop
//!      async (regola L: punto unico gateway).
//!   4) Decodifica il base64 e salva l'MP4 path-safe sotto la project_root
//!      (resolve_workspace_target, stesso punto unico di image_tools /
//!      figma_tools: niente path traversal, file confinato in project_root,
//!      regola E isolamento progetti). Se il provider ha risposto con una sola
//!      gcsUri (niente bytes inline) NON scarichiamo: ritorniamo l'url con nota
//!      (regola H: niente fetch nascosto di una URL esterna).
//!   5) Ritorna { video_path, model_used, provider_used } al modello.
//!
//! Niente magic fallback: se il purpose non risolve, il gateway e' giu', il
//! provider non genera video o il poll va in timeout, l'errore risale onestamente
//! al modello.

use nexus_types::tool_outcome::{NaturaFallimento, RispostaTool};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde_json::{json, Value};

use super::gateway_client::{gateway_generate_video, GwCaller};
use super::ToolContextCore;
use nexus_types::routing_client::resolve_purpose_via_http;
use nexus_types::workspace_paths::resolve_workspace_target;

/// Purpose che mappa al modello video-gen (mig 0482, required_capability=
/// video_gen). Punto unico di selezione del modello: niente nome modello
/// hardcoded (regola G).
const VIDEO_PURPOSE: &str = "generate_video";
/// Subdir sotto la project_root dove salvare i video generati (gemella image/tts).
const GENERATED_SUBDIR: &str = ".nexus/generated";
/// Estensione di salvataggio (gli endpoint video-gen del gateway emettono MP4).
const OUTPUT_EXT: &str = "mp4";

pub async fn tool_nexus_generate_video(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::NexusGenerateVideoInput};

    // 1) Permesso di scrittura obbligatorio: il tool PRODUCE un file su disco.
    if !ctx.can_write {
        // DEL SISTEMA: decisione del progetto sul run, non un parametro.
        return crate::errore_tool(
            "Permesso di scrittura non concesso: impossibile salvare il video \
             generato su disco. Esegui in una modalita' che consente la scrittura file.",
            NaturaFallimento::DelSistema,
        );
    }

    // 2) Parametri dal contratto.
    let params = match NexusGenerateVideoInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let prompt = params.prompt.trim().to_string();
    if prompt.is_empty() {
        return crate::errore_tool(
            "Parametro 'prompt' obbligatorio (descrizione testuale del video da generare).",
            NaturaFallimento::Rimediabile,
        );
    }
    // `max(0)` prima del cast: il contratto dichiara `i64`, e un negativo
    // passando da `as u32` diventerebbe una durata enorme. `as_u64` lo scartava
    // insieme al parametro, con lo stesso effetto ma senza dirlo.
    let duration_seconds = params.duration_seconds.map(|d| d.max(0) as u32);
    let filename = params
        .filename
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    // 3) Risolvi provider/model dal purpose (regola G: niente modello hardcoded).
    //    resolve_purpose_via_http e' il punto unico cross-processo che interroga
    //    il routing tier-only di mcp-core (capability='video_gen').
    let (provider, model) = match resolve_purpose_via_http(&ctx.db, VIDEO_PURPOSE).await {
        Ok(pm) => pm,
        Err(e) => {
            // Configurazione di piattaforma: nessun parametro della chiamata la
            // aggira, e il messaggio nomina la migrazione da verificare.
            return crate::errore_tool(
                format!(
                    "modello video-gen non risolvibile (purpose '{VIDEO_PURPOSE}'): {e}. \
                     Verifica nexus_purpose_model.generate_video (mig 0482) e che un modello \
                     video-gen sia abilitato nel catalog (mig 0482)."
                ),
                NaturaFallimento::DelSistema,
            );
        }
    };

    // 4) Genera via gateway (pin del provider risolto). Il gateway gestisce
    //    routing/cooldown/privacy + il poll-loop async (regola L: punto unico).
    let result = match gateway_generate_video(
        &ctx.db,
        &provider,
        &model,
        &prompt,
        duration_seconds,
        VIDEO_PURPOSE,
        &GwCaller {
            user_id: ctx.user_id,
            project_id: ctx.project_id,
            run_id: ctx.run_id,
        },
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            // TRANSITORIO come nel gemello image: quel che arriva qui e'
            // l'esaurimento di cooldown e failover del gateway.
            return crate::errore_tool(
                format!("generazione video via gateway fallita: {e}"),
                NaturaFallimento::Transitorio,
            );
        }
    };

    // 5) Estrai i byte dal base64 inline. Se il provider ha risposto solo con una
    //    gcsUri (niente bytes), non possiamo salvare path-safe: ritorniamo l'url
    //    con nota (regola H: niente fetch nascosto di una URL esterna).
    let Some(b64) = result.video_base64.as_deref() else {
        // DIVERGENZA CHIUSA, e si vedeva solo mettendo i due gemelli accanto:
        // `image_tools` dichiara questo identico caso come fallimento, qui
        // usciva un JSON con `note` e nessun marker — cioe' un SUCCESSO, per un
        // tool il cui compito era salvare il video nel progetto e che non l'ha
        // salvato. L'URL resta nel corpo: e' l'unica cosa che l'agente puo'
        // ancora fare, e il messaggio glielo dice.
        let dettagli = json!({
            "error": "il provider ha restituito solo una URL (gcsUri), non il video inline: \
                      impossibile salvarlo nel progetto. Scarica il video manualmente dall'URL.",
            "video_url": result.url.unwrap_or_default(),
            "model_used": result.model_used,
        });
        return crate::errore_tool_con_dettagli(dettagli, NaturaFallimento::DelSistema);
    };
    // Base64 malformato: e' il provider ad aver risposto male, non l'agente ad
    // aver chiesto male.
    let bytes = match B64.decode(b64) {
        Ok(b) => b,
        Err(e) => {
            return crate::errore_tool(
                format!("decodifica video base64 fallita: {e}"),
                NaturaFallimento::DelSistema,
            );
        }
    };

    // 6) Salva path-safe sotto la project_root.
    let (video_path, provider_used) = match save_video(&ctx.root_path, &bytes, filename).await {
        Ok(p) => (p, provider),
        // Come nel gemello image: `save_video` appiattisce gia' l'errore di I/O
        // in una `String`, quindi il `ErrorKind` qui non c'e' piu' e ricavarlo
        // dal messaggio sarebbe la regola M al contrario.
        Err(e) => {
            return crate::errore_tool(
                format!("salvataggio video fallito: {e}"),
                NaturaFallimento::DelSistema,
            )
        }
    };

    RispostaTool::riuscito(
        json!({
            "video_path": video_path,
            "model_used": result.model_used,
            "provider_used": provider_used,
        })
        .to_string(),
    )
}

/// Costruisce un nome file sicuro per il video generato. Se l'agente passa
/// `filename`, ne usa SOLO la parte basename (niente directory ne' traversal:
/// la path-safety finale e' garantita da `resolve_workspace_target`) forzando
/// l'estensione MP4; altrimenti genera un nome timestampato.
fn build_filename(requested: Option<&str>) -> String {
    let stem = requested
        .map(sanitize_stem)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f");
            format!("video_{ts}")
        });
    format!("{stem}.{OUTPUT_EXT}")
}

/// Riduce un nome richiesto al solo basename, elimina l'estensione e tiene solo
/// caratteri innocui ([A-Za-z0-9._-]). Difesa-in-profondita': il confinamento
/// vero e' in resolve_workspace_target, qui evitiamo solo nomi assurdi.
fn sanitize_stem(requested: &str) -> String {
    // Solo l'ultimo segmento (niente sottocartelle dal nome richiesto).
    let base = requested
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(requested)
        .trim();
    // Togli un'eventuale estensione (la forziamo a .mp4).
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
        // Evita un nome composto solo da separatori/punti.
        .trim_matches(['.', '_', '-'])
        .to_string()
}

/// Salva i byte MP4 sotto la project_root (path-safe) e ritorna il path relativo
/// pulito. Usa `resolve_workspace_target` (punto unico path-safety, regola L):
/// rifiuta traversal e qualunque target fuori dalla root (regola E).
async fn save_video(
    root: &std::path::Path,
    bytes: &[u8],
    requested_name: Option<&str>,
) -> Result<String, String> {
    let name = build_filename(requested_name);
    let rel = format!("{GENERATED_SUBDIR}/{name}");
    let (clean_rel, abs_target) = resolve_workspace_target(root, &rel)
        .map_err(|e| format!("path video non valido: {}", e.message()))?;

    if let Some(parent) = abs_target.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("creazione dir video fallita: {e}"))?;
    }
    tokio::fs::write(&abs_target, bytes)
        .await
        .map_err(|e| format!("scrittura video fallita: {e}"))?;
    Ok(clean_rel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_filename_default_e_mp4_timestamp() {
        let name = build_filename(None);
        assert!(name.starts_with("video_"));
        assert!(name.ends_with(".mp4"));
    }

    #[test]
    fn build_filename_forza_estensione_mp4() {
        // L'estensione richiesta viene sostituita con .mp4.
        let name = build_filename(Some("clip.mov"));
        assert_eq!(name, "clip.mp4");
    }

    #[test]
    fn build_filename_strippa_directory_dal_nome() {
        // Eventuali sottocartelle nel nome richiesto vengono scartate: il file
        // resta in .nexus/generated/. La path-safety vera e' comunque a valle.
        let name = build_filename(Some("../../etc/passwd"));
        assert_eq!(name, "passwd.mp4");
    }

    #[test]
    fn build_filename_sanitizza_caratteri() {
        let name = build_filename(Some("my video (final)!"));
        assert!(name.ends_with(".mp4"));
        assert!(!name.contains(' '));
        assert!(!name.contains('('));
        assert!(!name.contains('!'));
    }

    #[test]
    fn sanitize_stem_nome_solo_simboli_diventa_vuoto() {
        // Un nome fatto solo di separatori non deve produrre uno stem valido:
        // build_filename ricade sul timestamp.
        assert_eq!(sanitize_stem("..."), "");
        let name = build_filename(Some("..."));
        assert!(name.starts_with("video_"));
    }

    #[tokio::test]
    async fn save_video_confina_nella_root_e_crea_dir() {
        let tmp = std::env::temp_dir().join(format!("vid_test_{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&tmp).await.unwrap();
        let mp4 = b"\x00\x00\x00\x18ftypmp42_fake_body";
        let rel = save_video(&tmp, mp4, Some("clip")).await.unwrap();
        assert_eq!(rel, format!("{GENERATED_SUBDIR}/clip.mp4"));
        let written = tokio::fs::read(tmp.join(&rel)).await.unwrap();
        assert_eq!(written, mp4);
        tokio::fs::remove_dir_all(&tmp).await.ok();
    }

    #[tokio::test]
    async fn save_video_rifiuta_traversal_anche_se_nome_lo_contiene() {
        // Il nome con traversal e' gia' ridotto a basename da build_filename,
        // quindi non puo' uscire dalla root: il file finisce in .nexus/generated/.
        let tmp = std::env::temp_dir().join(format!("vid_test_{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&tmp).await.unwrap();
        let rel = save_video(&tmp, b"x", Some("../../../escape"))
            .await
            .unwrap();
        assert_eq!(rel, format!("{GENERATED_SUBDIR}/escape.mp4"));
        tokio::fs::remove_dir_all(&tmp).await.ok();
    }
}
