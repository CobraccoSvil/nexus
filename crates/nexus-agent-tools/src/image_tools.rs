//! Tool nexus_generate_image.
//!
//! Genera un'immagine dal prompt dell'agente e la salva path-safe dentro la
//! project_root, sotto `.nexus/generated/<nome>.png`. Ultimo pezzo dell'MVP
//! image-generation (PR6b-2).
//!
//! Flusso:
//!   1) Verifica ctx.can_write (il tool produce un file su disco): senza
//!      permesso di scrittura ritorna un errore esplicito (regola H).
//!   2) Risolve provider/model dal purpose `generate_image` via routing
//!      (resolve_purpose_via_http): NESSUN nome modello hardcoded (regola G).
//!   3) Chiama il Nexus LLM Gateway (`POST /v1/images/generations`) pinnando il
//!      provider risolto; il gateway possiede routing/cooldown/privacy e mappa
//!      la richiesta al dialetto del provider (regola L: punto unico gateway).
//!   4) Decodifica il base64 e salva il PNG path-safe sotto la project_root
//!      (resolve_workspace_target, stesso punto unico di figma_tools /
//!      visual_compare: niente path traversal, file confinato in project_root,
//!      regola E isolamento progetti).
//!   5) Ritorna { image_path, model_used, provider_used } al modello.
//!
//! Niente magic fallback: se il purpose non risolve, il gateway e' giu' o il
//! provider non genera immagini, l'errore risale onestamente al modello.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde_json::{json, Value};

use super::gateway_client::{gateway_image_generate, GwCaller};
use super::ToolContextCore;
use nexus_types::routing_client::resolve_purpose_via_http;
use nexus_types::workspace_paths::resolve_workspace_target;

/// Purpose che mappa al modello image-gen (mig 0478, tier=light,
/// required_capability=image_gen). Punto unico di selezione del modello: niente
/// nome modello hardcoded (regola G).
const IMAGE_PURPOSE: &str = "generate_image";
/// Subdir sotto la project_root dove salvare le immagini generate.
const GENERATED_SUBDIR: &str = ".nexus/generated";
/// Estensione/MIME di salvataggio (gli endpoint image-gen del gateway emettono PNG).
const OUTPUT_EXT: &str = "png";

pub async fn tool_nexus_generate_image(ctx: &ToolContextCore, input: &Value) -> String {
    // 1) Permesso di scrittura obbligatorio: il tool PRODUCE un file su disco.
    if !ctx.can_write {
        return crate::errore_json(
            "Permesso di scrittura non concesso: impossibile salvare l'immagine \
             generata su disco. Esegui in una modalita' che consente la scrittura file.",
        );
    }

    // 2) Prompt obbligatorio + parametri opzionali.
    let prompt = match input.get("prompt").and_then(Value::as_str).map(str::trim) {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => {
            return crate::errore_json(
                "Parametro 'prompt' obbligatorio (descrizione testuale dell'immagine da generare).",
            );
        }
    };
    let size = input
        .get("size")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let filename = input
        .get("filename")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    // 3) Risolvi provider/model dal purpose (regola G: niente modello hardcoded).
    //    resolve_purpose_via_http e' il punto unico cross-processo che interroga
    //    il routing tier-only di mcp-core (capability='image_gen').
    let (provider, model) = match resolve_purpose_via_http(&ctx.db, IMAGE_PURPOSE).await {
        Ok(pm) => pm,
        Err(e) => {
            return crate::errore_json(format!(
                "modello image-gen non risolvibile (purpose '{IMAGE_PURPOSE}'): {e}. \
                 Verifica nexus_purpose_model.generate_image (mig 0478) e che un modello \
                 image-gen sia abilitato nel catalog (mig 0479)."
            ));
        }
    };

    // 4) Genera via gateway (pin del provider risolto). Il gateway gestisce
    //    routing/cooldown/privacy e mappa la richiesta al dialetto del provider
    //    (regola L: punto unico gateway).
    let result = match gateway_image_generate(
        &ctx.db,
        &provider,
        &model,
        &prompt,
        size,
        IMAGE_PURPOSE,
        // Identita' del chiamante: senza, il gateway scarta la riga di ledger e
        // le immagini generate restano fuori dalla contabilita'.
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
            return crate::errore_json(format!("generazione immagine via gateway fallita: {e}"));
        }
    };

    // 5) Estrai i byte dal base64 inline. Se il provider ha risposto solo con una
    //    URL temporanea, non possiamo salvare path-safe: errore esplicito (regola
    //    H: niente fetch nascosto di una URL esterna).
    let Some(b64) = result.b64_json.as_deref() else {
        return crate::errore_json_con_dettagli(json!({
            "error": "il provider ha restituito solo una URL temporanea, non l'immagine inline: \
                      impossibile salvarla nel progetto.",
            "image_url": result.url.unwrap_or_default(),
            "model_used": result.model_used,
        }));
    };
    let bytes = match B64.decode(b64) {
        Ok(b) => b,
        Err(e) => {
            return crate::errore_json(format!("decodifica immagine base64 fallita: {e}"));
        }
    };

    // 6) Salva path-safe sotto la project_root.
    let (image_path, provider_used) = match save_image(&ctx.root_path, &bytes, filename).await {
        Ok(p) => (p, provider),
        Err(e) => return crate::errore_json(format!("salvataggio immagine fallito: {e}")),
    };

    json!({
        "image_path": image_path,
        "model_used": result.model_used,
        "provider_used": provider_used,
    })
    .to_string()
}

/// Costruisce un nome file sicuro per l'immagine generata. Se l'agente passa
/// `filename`, ne usa SOLO la parte basename (niente directory ne' traversal:
/// la path-safety finale e' garantita da `resolve_workspace_target`) forzando
/// l'estensione PNG; altrimenti genera un nome timestampato.
fn build_filename(requested: Option<&str>) -> String {
    let stem = requested
        .map(sanitize_stem)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f");
            format!("image_{ts}")
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
    // Togli un'eventuale estensione (la forziamo a .png).
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

/// Salva i byte PNG sotto la project_root (path-safe) e ritorna il path relativo
/// pulito. Usa `resolve_workspace_target` (punto unico path-safety, regola L):
/// rifiuta traversal e qualunque target fuori dalla root (regola E).
async fn save_image(
    root: &std::path::Path,
    bytes: &[u8],
    requested_name: Option<&str>,
) -> Result<String, String> {
    let name = build_filename(requested_name);
    let rel = format!("{GENERATED_SUBDIR}/{name}");
    let (clean_rel, abs_target) = resolve_workspace_target(root, &rel)
        .map_err(|e| format!("path immagine non valido: {}", e.message()))?;

    if let Some(parent) = abs_target.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("creazione dir immagini fallita: {e}"))?;
    }
    tokio::fs::write(&abs_target, bytes)
        .await
        .map_err(|e| format!("scrittura immagine fallita: {e}"))?;
    Ok(clean_rel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_filename_default_e_png_timestamp() {
        let name = build_filename(None);
        assert!(name.starts_with("image_"));
        assert!(name.ends_with(".png"));
    }

    #[test]
    fn build_filename_forza_estensione_png() {
        // L'estensione richiesta viene sostituita con .png.
        let name = build_filename(Some("logo.jpeg"));
        assert_eq!(name, "logo.png");
    }

    #[test]
    fn build_filename_strippa_directory_dal_nome() {
        // Eventuali sottocartelle nel nome richiesto vengono scartate: il file
        // resta in .nexus/generated/. La path-safety vera e' comunque a valle.
        let name = build_filename(Some("../../etc/passwd"));
        assert_eq!(name, "passwd.png");
    }

    #[test]
    fn build_filename_sanitizza_caratteri() {
        let name = build_filename(Some("my image (final)!"));
        // Spazi e parentesi diventano '_', niente caratteri pericolosi.
        assert!(name.ends_with(".png"));
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
        assert!(name.starts_with("image_"));
    }

    #[tokio::test]
    async fn save_image_confina_nella_root_e_crea_dir() {
        let tmp = std::env::temp_dir().join(format!("img_test_{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&tmp).await.unwrap();
        let png = b"\x89PNG\r\n\x1a\n_fake_body";
        let rel = save_image(&tmp, png, Some("logo")).await.unwrap();
        assert_eq!(rel, format!("{GENERATED_SUBDIR}/logo.png"));
        let written = tokio::fs::read(tmp.join(&rel)).await.unwrap();
        assert_eq!(written, png);
        tokio::fs::remove_dir_all(&tmp).await.ok();
    }

    #[tokio::test]
    async fn save_image_rifiuta_traversal_anche_se_nome_lo_contiene() {
        // Il nome con traversal e' gia' ridotto a basename da build_filename,
        // quindi non puo' uscire dalla root: il file finisce in .nexus/generated/.
        let tmp = std::env::temp_dir().join(format!("img_test_{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&tmp).await.unwrap();
        let rel = save_image(&tmp, b"x", Some("../../../escape"))
            .await
            .unwrap();
        assert_eq!(rel, format!("{GENERATED_SUBDIR}/escape.png"));
        tokio::fs::remove_dir_all(&tmp).await.ok();
    }
}
