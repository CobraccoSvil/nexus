//! Server statico integrato per progetti HTML.
//!
//! Espone una route PUBBLICA (no JWT) `GET /preview/:project_id/*path` che serve
//! i file di un progetto direttamente dalla sua `project_root`. Motivazione:
//! quando un progetto e' un sito HTML statico (es. generato dall'agente), non
//! esisteva alcun modo di servirlo. L'agente non riusciva ad avviare
//! `python3 -m http.server` (lo lanciava col proprio exec non-detached, che lo
//! uccideva subito) e nel WSL dell'utente `systemd --user` non e' attivo, quindi
//! i servizi-processo non partono.
//!
//! Questo server e' INTEGRATO in mcp-core (sempre attivo, nessun processo extra,
//! nessuna porta da allocare, funziona anche senza systemd). E' pubblico perche'
//! deve essere apribile in una nuova scheda del browser (che non porta il JWT).
//!
//! SICUREZZA (regola E, isolamento progetti): il path richiesto e' confinato
//! rigorosamente alla `project_root` tramite canonicalizzazione + verifica
//! `starts_with`. Qualsiasi tentativo di path traversal (`..`, symlink fuori
//! root) viene respinto con 403/404. Serve SOLO file gia' presenti nella root
//! del progetto, in ambiente locale.

use axum::{
    body::Body,
    extract::{Path as AxumPath, State},
    http::{header, HeaderValue, StatusCode},
    response::Response,
    Extension, Json,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Claims;
use crate::projects::{api_error, load_project_context, parse_user_id, ApiError};
use crate::AppState;

type ApiResult = Result<Json<Value>, ApiError>;

/// Content-Type per estensione. Niente dipendenza esterna: copre i tipi web
/// piu' comuni. Default `application/octet-stream` per il resto.
fn content_type_for(path: &std::path::Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" => "text/plain; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "wasm" => "application/wasm",
        "map" => "application/json; charset=utf-8",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

fn plain(status: StatusCode, msg: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(msg.to_string()))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

/// Risolve la root primaria del progetto. Nessuna auth utente: il preview e'
/// pubblico e locale; la sicurezza e' garantita dal confinamento del path.
async fn project_root(state: &AppState, project_id: Uuid) -> Option<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT w.absolute_path FROM workspaces w \
         WHERE w.project_id = $1 AND w.is_primary = TRUE LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
}

/// Handler: `GET /preview/:project_id/*path`.
///
/// `path` vuoto (o directory) -> `index.html`. Restituisce i byte del file con
/// il Content-Type corretto, confinato alla root del progetto.
pub async fn serve_preview(
    State(state): State<AppState>,
    AxumPath(rest): AxumPath<String>,
) -> Response<Body> {
    // UNA sola route wildcard `/preview/*rest`: `rest` = "<project_id>/<path...>".
    // Splittiamo manualmente il primo segmento. Motivo: in axum 0.7 il pattern
    // misto `:project_id/*path` lasciava `:project_id` VUOTO (il wildcard
    // "rubava" la cattura), indipendentemente da estrazione per tupla/struct/map.
    // Con un solo wildcard l'estrazione e' deterministica.
    let rest = rest.trim_start_matches('/');
    let (project_id, req_path) = match rest.split_once('/') {
        Some((pid, path)) => (pid.to_string(), path.to_string()),
        None => (rest.to_string(), String::new()),
    };
    serve_preview_inner(state, project_id, req_path).await
}

async fn serve_preview_inner(
    state: AppState,
    project_id: String,
    req_path: String,
) -> Response<Body> {
    let project_id = match Uuid::parse_str(&project_id) {
        Ok(u) => u,
        Err(_) => return plain(StatusCode::BAD_REQUEST, "project id non valido"),
    };

    let root = match project_root(&state, project_id).await {
        Some(r) => r,
        None => return plain(StatusCode::NOT_FOUND, "progetto non trovato"),
    };

    // Rifiuto preventivo di componenti pericolose. La canonicalizzazione sotto
    // e' la difesa primaria; questo e' un fast-fail leggibile.
    if req_path.split('/').any(|seg| seg == ".." || seg == ".") {
        return plain(StatusCode::FORBIDDEN, "path non consentito");
    }

    let root_path = std::path::Path::new(&root);
    let root_canon = match tokio::fs::canonicalize(root_path).await {
        Ok(p) => p,
        Err(_) => return plain(StatusCode::NOT_FOUND, "root progetto non accessibile"),
    };

    // Path vuoto -> index.html (entry di default del sito).
    let rel = if req_path.trim_matches('/').is_empty() {
        "index.html".to_string()
    } else {
        req_path.trim_start_matches('/').to_string()
    };

    let mut target = root_canon.join(&rel);

    // Se punta a una directory, servi il suo index.html.
    if tokio::fs::metadata(&target)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false)
    {
        target = target.join("index.html");
    }

    // Canonicalizza il target reale e verifica che resti DENTRO la root.
    let target_canon = match tokio::fs::canonicalize(&target).await {
        Ok(p) => p,
        Err(_) => return plain(StatusCode::NOT_FOUND, "file non trovato"),
    };
    if !target_canon.starts_with(&root_canon) {
        return plain(
            StatusCode::FORBIDDEN,
            "accesso fuori dalla root del progetto",
        );
    }
    if !target_canon.is_file() {
        return plain(StatusCode::NOT_FOUND, "non e' un file");
    }

    let bytes = match tokio::fs::read(&target_canon).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(file = %target_canon.display(), error = %e, "preview: lettura file fallita");
            return plain(StatusCode::INTERNAL_SERVER_ERROR, "lettura file fallita");
        }
    };

    let ctype = content_type_for(&target_canon);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, HeaderValue::from_static(ctype))
        // Niente cache aggressiva: durante lo sviluppo i file cambiano spesso.
        .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
        .body(Body::from(bytes))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

/// Le entry canoniche di un sito, in ordine di preferenza.
const ENTRY_CANONICHE: [&str; 4] = ["index.html", "index.htm", "home.html", "main.html"];

/// Cartelle che non contengono mai il sito da mostrare: dipendenze, cache di
/// build, metadati di VCS. `dist` e `build` NON sono qui — sono proprio i posti
/// in cui un sito costruito finisce.
///
/// PUBBLICO perche' e' l'unico proprietario di questo vocabolario (regola L), e
/// ha un secondo lettore: il criterio che sceglie QUALE pagina il gate misura
/// (`decisions::pagina_del_run`) deve scartare le stesse cartelle di qui, o le
/// due strade che portano una pagina al gate — il rilevamento e la scrittura —
/// avrebbero due idee diverse di «cartella che non e' il sito». Quel criterio
/// vive in un crate che non vede questo, quindi l'elenco gli arriva coi fatti;
/// che vi arrivi DAVVERO lo verifica il ponte in
/// `agent_graph_adapter::pagina_del_run`.
pub const CARTELLE_ESCLUSE: [&str; 6] = [
    "node_modules",
    ".git",
    ".next",
    ".nuxt",
    ".svelte-kit",
    "target",
];

/// Rileva se il progetto contiene un sito statico servibile, e quale file usare
/// come entry. Ritorna il percorso RELATIVO alla root (es. `index.html` oppure
/// `landing/index.html`), che e' anche cio' che il proxy `/preview` si aspetta.
///
/// PERCHE' GUARDA ANCHE NELLE SOTTOCARTELLE. Prima cercava solo in root, con la
/// motivazione «l'entry di un sito sta in root». E' vero quando il progetto E'
/// un sito; e' falso quando il sito e' una PARTE del progetto — ed e' il caso
/// che l'agente produce piu' spesso, perche' e' quello che gli si chiede.
///
/// MISURATO il 07/08/2026 su gestione-corsi: l'agente ha generato in autonomia
/// `landing/index.html` (pagina statica completa, 6 corsi, filtro JS, nessuna
/// dipendenza esterna), il pannello Servizi non l'ha rilevata e non c'era modo
/// di aprirla da Nexus. Il proxy sapeva gia' servirla: `serve_preview` accetta
/// un percorso relativo arbitrario con confinamento nella root. Mancava solo
/// chi glielo dicesse.
///
/// L'ordine e' deterministico e mette la ROOT davanti a tutto: un progetto che
/// e' un sito continua a comportarsi esattamente come prima.
pub async fn detect_static_entry(root: &str) -> Option<String> {
    // 1) Entry canoniche nella root: il caso "il progetto e' un sito".
    for entry in ENTRY_CANONICHE {
        if e_un_file(&format!("{root}/{entry}")).await {
            return Some(entry.to_string());
        }
    }

    // 2) Entry canoniche in una sottocartella di primo livello: il caso "il
    //    sito e' una parte del progetto". Profondita' UNO e non ricorsione
    //    piena: piu' in fondo si troverebbero le pagine di esempio delle
    //    dipendenze, non il sito del progetto.
    let mut candidati: Vec<String> = Vec::new();
    if let Ok(mut dir) = tokio::fs::read_dir(root).await {
        while let Ok(Some(e)) = dir.next_entry().await {
            let nome = e.file_name().to_string_lossy().to_string();
            if nome.starts_with('.') || CARTELLE_ESCLUSE.contains(&nome.as_str()) {
                continue;
            }
            if !e.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            for entry in ENTRY_CANONICHE {
                if e_un_file(&format!("{root}/{nome}/{entry}")).await {
                    candidati.push(format!("{nome}/{entry}"));
                    break;
                }
            }
        }
    }
    if !candidati.is_empty() {
        // Ordine alfabetico: con due siti la scelta dev'essere la stessa a ogni
        // chiamata, o il pulsante cambierebbe bersaglio da un refresh all'altro.
        candidati.sort();
        return candidati.into_iter().next();
    }

    // 3) Ripiego: primo file .html/.htm nella root, per i siti che non seguono
    //    la convenzione index (es. pagine flotta.html/prenota.html senza index).
    let mut html_files: Vec<String> = Vec::new();
    if let Ok(mut dir) = tokio::fs::read_dir(root).await {
        while let Ok(Some(e)) = dir.next_entry().await {
            let name = e.file_name().to_string_lossy().to_string();
            let lower = name.to_ascii_lowercase();
            if (lower.ends_with(".html") || lower.ends_with(".htm"))
                && e.file_type().await.map(|t| t.is_file()).unwrap_or(false)
            {
                html_files.push(name);
            }
        }
    }
    html_files.sort();
    html_files.into_iter().next()
}

/// Il PERCORSO con cui si apre una pagina del progetto attraverso il proxy di
/// anteprima. Same-origin e relativo: il frontend lo apre cosi', e chi ha una
/// base assoluta (il gate) vi antepone la propria.
///
/// Un posto solo perche' la forma della route non e' un dettaglio di stampa: se
/// due chiamanti la componessero a modo loro, il giorno che la route cambia uno
/// dei due aprirebbe un indirizzo che risponde 404 — e per il gate un 404 non e'
/// un errore di configurazione, e' «la pagina non si e' caricata», cioe' un
/// difetto della misura travestito da difetto del codice.
pub fn percorso_preview(project_id: uuid::Uuid, entry: &str) -> String {
    format!("/preview/{}/{}", project_id, entry.trim_start_matches('/'))
}

async fn e_un_file(percorso: &str) -> bool {
    tokio::fs::metadata(percorso)
        .await
        .map(|m| m.is_file())
        .unwrap_or(false)
}

/// Handler protetto: `GET /api/projects/:id/static-site`.
///
/// Verifica l'accesso utente al progetto e ritorna se esiste un sito statico
/// servibile, l'entry e l'URL di preview (relativo: il frontend lo apre
/// same-origin tramite il proxy `/preview/*`). Usato dal pannello SERVIZI per
/// mostrare la card "Sito statico HTML" con il pulsante "Apri nel browser".
pub async fn static_site_info(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    // Verifica accesso (stessa porta d'ingresso degli altri endpoint progetto).
    let context = load_project_context(&state.db, project_id, user_id).await?;

    match detect_static_entry(context.root_path.to_string_lossy().as_ref()).await {
        Some(entry) => Ok(Json(json!({
            "detected": true,
            // URL relativo same-origin: il proxy web-ide inoltra /preview/* a
            // mcp-core. Apribile direttamente in una nuova scheda.
            "url": percorso_preview(project_id, &entry),
            "entry": entry,
        }))),
        None => Ok(Json(json!({ "detected": false }))),
    }
}

#[cfg(test)]
mod tests_rilevamento_entry {
    use super::detect_static_entry;

    fn radice() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn scrivi(dir: &std::path::Path, rel: &str) {
        let p = dir.join(rel);
        if let Some(genitore) = p.parent() {
            std::fs::create_dir_all(genitore).expect("mkdir");
        }
        std::fs::write(p, "<html></html>").expect("write");
    }

    /// Il caso storico non cambia: se il progetto E' un sito, l'entry e' in root.
    #[tokio::test]
    async fn la_root_ha_la_precedenza() {
        let d = radice();
        scrivi(d.path(), "index.html");
        scrivi(d.path(), "landing/index.html");
        assert_eq!(
            detect_static_entry(&d.path().to_string_lossy()).await,
            Some("index.html".to_string()),
            "un sito in root non deve essere scavalcato da una sottocartella"
        );
    }

    /// IL caso che mancava: il sito e' una PARTE del progetto.
    ///
    /// MUTAZIONE: togliendo la ricerca a profondita' uno, questo test torna
    /// `None` — ed e' esattamente cio' che l'utente vedeva, cioe' niente.
    #[tokio::test]
    async fn una_sottocartella_viene_trovata() {
        let d = radice();
        scrivi(d.path(), "landing/index.html");
        scrivi(d.path(), "backend/Program.cs");
        assert_eq!(
            detect_static_entry(&d.path().to_string_lossy()).await,
            Some("landing/index.html".to_string())
        );
    }

    /// Le dipendenze contengono pagine di esempio a migliaia: nessuna di quelle
    /// e' il sito del progetto.
    #[tokio::test]
    async fn le_cartelle_di_dipendenze_sono_escluse() {
        let d = radice();
        scrivi(d.path(), "node_modules/pacchetto/index.html");
        scrivi(d.path(), ".next/server/index.html");
        assert_eq!(
            detect_static_entry(&d.path().to_string_lossy()).await,
            None,
            "nessun sito del progetto: non si deve offrire quello di una dipendenza"
        );
    }

    /// Con due candidati la scelta e' STABILE: un pulsante che cambia bersaglio
    /// a ogni refresh e' peggio di un pulsante assente.
    #[tokio::test]
    async fn con_due_siti_la_scelta_e_deterministica() {
        let d = radice();
        scrivi(d.path(), "zeta/index.html");
        scrivi(d.path(), "alfa/index.html");
        let primo = detect_static_entry(&d.path().to_string_lossy()).await;
        let secondo = detect_static_entry(&d.path().to_string_lossy()).await;
        assert_eq!(primo, Some("alfa/index.html".to_string()));
        assert_eq!(primo, secondo);
    }

    /// `dist` e `build` NON sono escluse: e' li' che finisce un sito costruito.
    #[tokio::test]
    async fn la_cartella_di_build_non_e_esclusa() {
        let d = radice();
        scrivi(d.path(), "dist/index.html");
        assert_eq!(
            detect_static_entry(&d.path().to_string_lossy()).await,
            Some("dist/index.html".to_string())
        );
    }
}
