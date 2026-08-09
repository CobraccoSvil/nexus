//! Tool `nexus_visual_compare` — FASE 2 "resa Figma Make": verifica visiva.
//!
//! Screenshotta un URL locale dell'app costruita e lo confronta col design di
//! riferimento Figma (thumbnail.png dentro un .make, oppure un'immagine
//! allegata) usando un modello vision. Ritorna un'analisi strutturata degli
//! scostamenti di design (palette, tipografia, spaziature, layout, componenti)
//! e una stima di similarita' 0-100, cosi' l'agente in modalita' Continuo puo'
//! iterare e ridurre la distanza visiva.
//!
//! Riuso dell'infrastruttura esistente (niente nuova dipendenza headless):
//!   - Screenshot: driver Playwright gia' installato nel progetto, pilotato
//!     da uno script Node inline (stesso runtime di run_playwright_tests /
//!     browser_check). Nessun nuovo crate.
//!   - Reference .make: stesso accesso allegati di figma_tools /
//!     attachment_inspector (load_attachment + apertura ZIP).
//!   - Modello vision: risolto SOLO dal DB (regola G CLAUDE.md) via il purpose
//!     'visual_compare' lato brain (POST /vision/compare, mig 0214).
//!
//! Niente magic number: viewport, wait, timeout e soglia vivono in `settings`
//! (key `agent.visual_compare.*`, mig 0214). Niente panic.
//!
//! Esito (regola Q): il tool ritorna [`RispostaTool`] — il payload JSON sta nel
//! testo, l'esito e la NATURA del fallimento stanno nei campi. Prima il
//! fallimento viaggiava come marker anteposto al JSON, che spezzava il payload;
//! e i modi di fallire, che non sono la stessa cosa, arrivavano indistinti.
//! Ogni funzione interna dichiara la natura del PROPRIO errore ([`Fallimento`]):
//! appiattirla in una `String` obbligherebbe il chiamante a rileggerla dal
//! testo, che e' cio' che la regola M vieta.

use std::fmt::Display;
use std::io::{Cursor, Read};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use uuid::Uuid;

use super::attachment_inspector::{
    detect_kind, load_attachment, read_header, uuid_allegato, AttachmentRecord,
};
use super::AgentToolContext;
use crate::projects::resolve_workspace_target;
use crate::settings;
use nexus_agent_tools::input_contract::InputTool;
use nexus_agent_tools::tool_inputs::NexusVisualCompareInput;
use nexus_types::tool_outcome::{NaturaFallimento, RispostaTool};

// ── Default safe (usati solo se il setting manca; il DB e' la fonte di verita',
//    mig 0214). Non sono "magic fallback": sono gli stessi valori che il
//    catalogo PROMETTE al modello nella descrizione dei parametri. ───────────
const DEFAULT_VIEWPORT_WIDTH: u32 = 1280;
const DEFAULT_VIEWPORT_HEIGHT: u32 = 800;
const DEFAULT_WAIT_MS: u64 = 1500;
const DEFAULT_SCREENSHOT_TIMEOUT_SECS: u64 = 45;

/// Estremi accettati per l'override di viewport e attesa passato nella
/// chiamata: oltre 8K lo scatto non e' piu' una verifica ma un consumo di
/// memoria, e oltre un minuto l'attesa e' un timeout travestito.
const MAX_VIEWPORT_WIDTH: i64 = 7680;
const MAX_VIEWPORT_HEIGHT: i64 = 4320;
const MAX_WAIT_MS: u64 = 60_000;

/// Purpose del confronto visivo: il modello si risolve VIA TIER da qui (mig
/// 0214), mai per nome (regola G).
const VISUAL_COMPARE_PURPOSE: &str = "visual_compare";

/// Tetto di output della risposta vision. Il confronto restituisce un JSON
/// breve (punteggio + elenco differenze), non prosa.
const VISION_MAX_TOKENS: u32 = 1500;

/// Quanto della risposta grezza si allega quando il modello non rispetta il
/// formato: abbastanza per capire cosa ha risposto, non tanto da inondare il
/// contesto dell'agente.
const RAW_ANSWER_EXCERPT_CHARS: usize = 400;

/// Contratto di risposta imposto al modello. La prima immagine e' lo screenshot
/// reale, la seconda il riferimento: l'ordine e' quello dei blocchi inviati.
const VISUAL_COMPARE_PROMPT: &str = concat!(
    "Confronta due immagini di una interfaccia: la PRIMA e' lo screenshot reale, ",
    "la SECONDA e' il riferimento atteso.\n\n",
    "Rispondi ESCLUSIVAMENTE con un oggetto JSON, senza testo attorno, in questo formato:\n",
    "{\"similarity_score\": <numero 0-100>, \"differences\": [{\"area\": \"<zona dell'interfaccia>\", ",
    "\"expected\": \"<cosa mostra il riferimento>\", \"actual\": \"<cosa mostra lo screenshot>\", ",
    "\"severity\": \"alta|media|bassa\"}]}\n\n",
    "similarity_score: 100 = identiche, 0 = completamente diverse. Elenca solo differenze ",
    "VISIBILI e concrete (layout, colori, testo, elementi mancanti o in piu'). ",
    "Se non ci sono differenze rilevanti, usa un array vuoto.",
);
/// Subdir sotto la project_root dove salvare gli screenshot del tool.
const SCREENSHOT_SUBDIR: &str = ".nexus/visual_compare";

/// Il rifiuto di un `reference` che esiste ma non e' utilizzabile come immagine
/// di confronto. RIMEDIABILE, e il messaggio dice COME: i due tool che elencano
/// e classificano gli allegati sono nominati.
const RIFERIMENTO_NON_USABILE: &str =
    "Impossibile usare 'reference': il .make non contiene thumbnail.png e l'allegato non e' \
     un'immagine. Usa nexus_inspect_attachment per sapere che cos'e' e nexus_list_attachments \
     per gli id disponibili, passa l'attachment_id di un'immagine di riferimento, oppure ometti \
     'reference' per ottenere il solo screenshot.";

/// Parametri operativi risolti dal DB.
struct CompareSettings {
    viewport_width: u32,
    viewport_height: u32,
    wait_ms: u64,
    screenshot_timeout_secs: u64,
}

/// Un fallimento e la sua NATURA, dichiarata da chi conosce la causa.
///
/// Le funzioni interne di questo modulo — riferimento, screenshot, salvataggio,
/// confronto — sanno di che natura e' il loro errore. Restituirlo come `String`
/// la perderebbe, e il chiamante dovrebbe dedurla dal testo: la regola M al
/// contrario. Il tipo la trasporta fino al punto in cui diventa un campo.
struct Fallimento {
    messaggio: String,
    natura: NaturaFallimento,
}

impl Fallimento {
    /// L'agente puo' correggere da solo. Chi lo costruisce deve mettere nel
    /// messaggio l'informazione per farlo.
    fn rimediabile(messaggio: impl Display) -> Self {
        Self {
            messaggio: messaggio.to_string(),
            natura: NaturaFallimento::Rimediabile,
        }
    }

    /// Condizione di momento: ritentare la stessa chiamata e' la strategia
    /// corretta.
    fn transitorio(messaggio: impl Display) -> Self {
        Self {
            messaggio: messaggio.to_string(),
            natura: NaturaFallimento::Transitorio,
        }
    }

    /// Fuori dalla portata dell'agente: ambiente, configurazione, storage.
    fn di_sistema(messaggio: impl Display) -> Self {
        Self {
            messaggio: messaggio.to_string(),
            natura: NaturaFallimento::DelSistema,
        }
    }

    /// La natura di un errore del filesystem non si sceglie a mano: la legge il
    /// `ErrorKind` (regola M), perche' il messaggio del sistema operativo e'
    /// localizzato e diverso fra Windows e Linux.
    fn da_io(messaggio: impl Display, e: &std::io::Error) -> Self {
        Self {
            messaggio: messaggio.to_string(),
            natura: NaturaFallimento::da_errore_io(e),
        }
    }

    /// Il fallimento consegnato all'agente, quando il payload porta il solo
    /// campo `error`.
    fn in_risposta(self) -> RispostaTool {
        err(self.messaggio, self.natura)
    }
}

/// Costruisce l'esito FALLITO del tool: payload JSON nel testo, esito e natura
/// nei campi. Il corpo torna a essere un JSON integro, che il marker anteposto
/// spezzava.
fn fallito(payload: Value, natura: NaturaFallimento) -> RispostaTool {
    RispostaTool::fallito(payload.to_string()).con_natura(natura)
}

/// Il caso comune: il payload porta il solo campo `error`.
fn err(messaggio: impl Display, natura: NaturaFallimento) -> RispostaTool {
    fallito(json!({ "error": messaggio.to_string() }), natura)
}

/// `nexus_visual_compare(url, reference?, viewport?, wait_ms?)`.
pub(super) async fn tool_nexus_visual_compare(
    ctx: &AgentToolContext,
    input: &Value,
) -> RispostaTool {
    if !ctx.can_write {
        // Il permesso lo decide la policy del run, non la chiamata: non c'e'
        // nessun parametro da correggere e ripetere non cambia l'esito.
        return err(
            "Permesso di scrittura non concesso: impossibile salvare lo screenshot su disco.",
            NaturaFallimento::DelSistema,
        );
    }
    let params = match NexusVisualCompareInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let url = match url_validato(&params.url) {
        Ok(u) => u,
        Err(risposta) => return risposta,
    };
    let cfg = match risolvi_parametri(&ctx.db, &params).await {
        Ok(c) => c,
        Err(risposta) => return risposta,
    };
    let reference = match riferimento(ctx, params.reference.as_deref()).await {
        Ok(r) => r,
        Err(risposta) => return risposta,
    };
    scatta_e_confronta(ctx, &url, &cfg, reference).await
}

/// L'url dello scatto, validato per cio' che lo schema non puo' vincolare: una
/// stringa c'e' sempre, che sia un indirizzo HTTP no. Entrambi i rifiuti sono
/// RIMEDIABILI e nominano il campo.
fn url_validato(grezzo: &str) -> Result<String, RispostaTool> {
    let url = grezzo.trim();
    if url.is_empty() {
        let msg = "Il campo 'url' e' vuoto: passa l'indirizzo locale dell'app avviata \
                   (es. http://localhost:29348/), leggendo la porta da quella allocata al \
                   servizio.";
        return Err(err(msg, NaturaFallimento::Rimediabile));
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        let msg = format!(
            "Il campo 'url' deve iniziare con http:// o https:// (ricevuto '{url}'): passa \
             l'indirizzo completo del servizio, non un percorso o un nome host nudo."
        );
        return Err(err(msg, NaturaFallimento::Rimediabile));
    }
    Ok(url.to_string())
}

/// I parametri operativi: default dal DB, override dalla chiamata.
///
/// DIVERGENZA CHIUSA: viewport e `wait_ms` erano letti con `as_u64`, quindi un
/// valore NEGATIVO o zero spariva in silenzio e la chiamata proseguiva col
/// default — il catalogo li dichiara `integer`, quindi quel valore arriva fin
/// qui ed e' un errore della chiamata: dirlo e' meglio che ignorarlo.
async fn risolvi_parametri(
    db: &sqlx::PgPool,
    params: &NexusVisualCompareInput,
) -> Result<CompareSettings, RispostaTool> {
    let mut cfg = load_settings(db).await;
    if let Some(vp) = &params.viewport {
        if let Some(w) = vp.width {
            cfg.viewport_width = dimensione_viewport("width", w, MAX_VIEWPORT_WIDTH)?;
        }
        if let Some(h) = vp.height {
            cfg.viewport_height = dimensione_viewport("height", h, MAX_VIEWPORT_HEIGHT)?;
        }
    }
    if let Some(w) = params.wait_ms {
        cfg.wait_ms = attesa_richiesta(w)?;
    }
    Ok(cfg)
}

/// Una dimensione del viewport passata nella chiamata. Il `min` non e' una
/// politica: e' il tetto oltre il quale lo scatto non serve piu' a verificare.
fn dimensione_viewport(campo: &str, valore: i64, max: i64) -> Result<u32, RispostaTool> {
    if valore <= 0 {
        let msg = format!(
            "'viewport.{campo}' deve essere un intero positivo (ricevuto {valore}). Omettilo \
             per il default configurato in settings (agent.visual_compare.viewport_{campo})."
        );
        return Err(err(msg, NaturaFallimento::Rimediabile));
    }
    Ok(valore.min(max) as u32)
}

/// L'attesa dopo il load passata nella chiamata. Zero e' legittimo (scatto
/// immediato); il negativo no.
fn attesa_richiesta(valore: i64) -> Result<u64, RispostaTool> {
    if valore < 0 {
        let msg = format!(
            "'wait_ms' deve essere un intero >= 0 (ricevuto {valore}). Omettilo per il default \
             configurato in settings (agent.visual_compare.wait_ms)."
        );
        return Err(err(msg, NaturaFallimento::Rimediabile));
    }
    Ok((valore as u64).min(MAX_WAIT_MS))
}

/// L'immagine di riferimento, se la chiamata ne ha chiesta una.
///
/// DIVERGENZA CHIUSA: un `reference` vuoto era trattato come «campo omesso»,
/// cioe' una richiesta di CONFRONTO usciva come un semplice screenshot. Il
/// catalogo dice che per saltare il confronto si OMETTE il campo, quindi la
/// stringa vuota e' una chiamata malformata e ora lo dichiara.
async fn riferimento(
    ctx: &AgentToolContext,
    reference: Option<&str>,
) -> Result<Option<ReferenceImage>, RispostaTool> {
    let Some(grezzo) = reference.map(str::trim) else {
        return Ok(None);
    };
    if grezzo.is_empty() {
        let msg = "Il campo 'reference' e' vuoto: passa l'attachment_id del design di \
                   riferimento (nexus_list_attachments elenca quelli della sessione), oppure \
                   OMETTI il campo per ottenere il solo screenshot senza confronto.";
        return Err(err(msg, NaturaFallimento::Rimediabile));
    }
    // Il contratto pretende che il campo sia una stringa; che sia un uuid lo
    // dice il punto unico degli allegati, che nomina anche il tool con cui
    // rimediare.
    let attachment_id = uuid_allegato(grezzo)?;
    match resolve_reference(ctx, attachment_id).await {
        Ok(Some(img)) => Ok(Some(img)),
        Ok(None) => Err(err(RIFERIMENTO_NON_USABILE, NaturaFallimento::Rimediabile)),
        Err(f) => Err(f.in_risposta()),
    }
}

/// Cattura, salva e — se c'e' un riferimento — confronta.
async fn scatta_e_confronta(
    ctx: &AgentToolContext,
    url: &str,
    cfg: &CompareSettings,
    reference: Option<ReferenceImage>,
) -> RispostaTool {
    let shot = match capture_screenshot(&ctx.root_path, url, cfg).await {
        Ok(bytes) => bytes,
        Err(f) => return fallimento_scatto(&f),
    };
    let screenshot_path = match save_screenshot(&ctx.root_path, &shot).await {
        Ok(p) => p,
        Err(f) => return f.in_risposta(),
    };
    // Senza riferimento non c'e' confronto da fare, e questo NON e' un
    // fallimento: lo screenshot e' stato prodotto e salvato, che e' tutto cio'
    // che la chiamata aveva chiesto.
    let Some(reference) = reference else {
        return solo_screenshot(screenshot_path);
    };
    confronto_vision(ctx, &shot, &reference, screenshot_path).await
}

/// L'esito quando la chiamata non chiedeva un confronto.
fn solo_screenshot(screenshot_path: String) -> RispostaTool {
    let payload = json!({
        "screenshot_path": screenshot_path,
        "reference_source": Value::Null,
        "note": "Screenshot catturato e salvato su disco. Nessun 'reference' fornito: \
                 impossibile calcolare similarity_score/differences. Passa l'attachment_id \
                 del .make o di un'immagine di riferimento per il confronto vision.",
    });
    RispostaTool::riuscito(payload.to_string())
}

/// Il fallimento della cattura, col suggerimento che corrisponde alla causa.
fn fallimento_scatto(f: &Fallimento) -> RispostaTool {
    let payload = json!({
        "error": format!("cattura screenshot fallita: {}", f.messaggio),
        "hint": suggerimento_scatto(f.natura),
    });
    fallito(payload, f.natura)
}

/// Che cosa fare, dato il MODO in cui la cattura e' fallita.
///
/// Prima l'hint era uno solo e nominava insieme il dev server e l'installazione
/// di Playwright: su un timeout mandava a installare un browser gia' presente,
/// su un ambiente incompleto a controllare un url che era giusto.
fn suggerimento_scatto(natura: NaturaFallimento) -> &'static str {
    match natura {
        NaturaFallimento::Rimediabile => {
            "Il browser ha aperto l'url e non ha ottenuto la pagina: verifica che il servizio sia \
             avviato (list_active_services) e che host, porta e route siano quelli allocati al \
             progetto, poi richiama nexus_visual_compare con l'url corretto."
        }
        NaturaFallimento::Transitorio => {
            "La pagina non ha finito di caricare entro il timeout: attendi che il servizio sia \
             pronto e riprova, eventualmente alzando 'wait_ms'."
        }
        NaturaFallimento::DelSistema => {
            "L'ambiente di cattura non e' pronto (node, pacchetto playwright o Chromium): non \
             insistere in loop, procedi senza la verifica visiva o dichiara il blocco."
        }
    }
}

/// Immagine di riferimento risolta + provenienza.
struct ReferenceImage {
    bytes: Vec<u8>,
    mime: String,
    /// "thumbnail" (estratta dal .make) oppure "attachment" (immagine allegata).
    source: String,
}

/// Carica i parametri dal DB con default safe documentati.
async fn load_settings(db: &sqlx::PgPool) -> CompareSettings {
    CompareSettings {
        viewport_width: setting_u64(
            db,
            "agent.visual_compare.viewport_width",
            DEFAULT_VIEWPORT_WIDTH as u64,
        )
        .await as u32,
        viewport_height: setting_u64(
            db,
            "agent.visual_compare.viewport_height",
            DEFAULT_VIEWPORT_HEIGHT as u64,
        )
        .await as u32,
        wait_ms: setting_u64(db, "agent.visual_compare.wait_ms", DEFAULT_WAIT_MS).await,
        screenshot_timeout_secs: setting_u64(
            db,
            "agent.visual_compare.screenshot_timeout_secs",
            DEFAULT_SCREENSHOT_TIMEOUT_SECS,
        )
        .await,
    }
}

/// Legge un setting numerico; default safe se assente/non parsabile/DB down.
///
/// L'errore NON e' inghiottito in silenzio: e' loggato, e la degradazione tocca
/// solo la GEOMETRIA dello scatto — non un esito. Trasformare un DB muto su una
/// chiave cosmetica in un fallimento del tool baratterebbe un default che il
/// catalogo stesso promette al modello ("Default 1280x800") con una verifica
/// visiva bloccata.
async fn setting_u64(db: &sqlx::PgPool, key: &str, default: u64) -> u64 {
    match settings::get_setting(db, key).await {
        Ok(Some(raw)) => match raw.trim().parse::<u64>() {
            Ok(v) if v > 0 => v,
            _ => {
                tracing::warn!(key = %key, raw = %raw, default, "visual_compare: setting non parsabile, uso default");
                default
            }
        },
        Ok(None) => default,
        Err(e) => {
            tracing::warn!(key = %key, error = %e, default, "visual_compare: lettura setting fallita, uso default");
            default
        }
    }
}

/// Risolve l'immagine di riferimento dall'attachment_id passato in `reference`.
///
/// - `.make` (kind figma) -> estrae thumbnail.png dallo ZIP;
/// - immagine -> usa i byte direttamente.
///
/// Ritorna `Ok(None)` se l'allegato non e' utilizzabile come riferimento
/// (es. .make senza thumbnail e non immagine).
///
/// Le due nature dei primi due passi sono DIVERSE e sono le stesse scelte dal
/// punto unico `documento_da_allegato`: un allegato che non risulta nel progetto
/// e' un id sbagliato (RIMEDIABILE), un file che il DB dichiara e lo storage non
/// consegna e' un guasto che nessuna riformulazione aggira.
async fn resolve_reference(
    ctx: &AgentToolContext,
    attachment_id: Uuid,
) -> Result<Option<ReferenceImage>, Fallimento> {
    let record = load_attachment(&ctx.db, attachment_id, ctx.project_id)
        .await
        .map_err(Fallimento::rimediabile)?;
    let header = read_header(&record.file_path)
        .await
        .map_err(Fallimento::di_sistema)?;
    let (kind, mime_reale, _ext) = detect_kind(&header, &record.file_name, &record.mime_type);

    // Caso immagine diretta.
    if is_image_kind(&kind) {
        let bytes = tokio::fs::read(&record.file_path)
            .await
            .map_err(|e| Fallimento::da_io(format!("read immagine reference fallita: {e}"), &e))?;
        return Ok(Some(ReferenceImage {
            bytes,
            mime: mime_reale,
            source: "attachment".to_string(),
        }));
    }

    // Caso .make Figma: estrai thumbnail.png dallo ZIP.
    if kind == "figma" {
        return extract_make_thumbnail(&record).await;
    }

    Ok(None)
}

/// Estrae `thumbnail.png` da un archivio .make. Ritorna `Ok(None)` se assente.
///
/// Un archivio illeggibile e' RIMEDIABILE perche' l'alternativa esiste ed e'
/// nominata: passare un'immagine di riferimento invece del .make.
async fn extract_make_thumbnail(
    record: &AttachmentRecord,
) -> Result<Option<ReferenceImage>, Fallimento> {
    let bytes = tokio::fs::read(&record.file_path)
        .await
        .map_err(|e| Fallimento::da_io(format!("read .make fallita: {e}"), &e))?;

    let thumb = tokio::task::spawn_blocking(move || extract_thumbnail_bytes(&bytes))
        .await
        .map_err(|e| Fallimento::di_sistema(format!("spawn_blocking fallita: {e}")))?
        .map_err(|e| {
            Fallimento::rimediabile(format!(
                "{e}. Passa in 'reference' l'attachment_id di un'immagine del design invece del \
                 .make, oppure ometti il campo per il solo screenshot."
            ))
        })?;

    Ok(thumb.map(|bytes| ReferenceImage {
        bytes,
        mime: "image/png".to_string(),
        source: "thumbnail".to_string(),
    }))
}

/// Apre lo ZIP e legge `thumbnail.png` se presente. Errori ZIP propagati;
/// thumbnail assente -> `Ok(None)`.
fn extract_thumbnail_bytes(bytes: &[u8]) -> Result<Option<Vec<u8>>, String> {
    if bytes.len() < 4 || &bytes[0..4] != b"PK\x03\x04" {
        return Err("il .make non e' uno ZIP valido".into());
    }
    let reader = Cursor::new(bytes);
    let mut archive =
        zip::ZipArchive::new(reader).map_err(|e| format!("apertura ZIP fallita: {e}"))?;
    let mut idx: Option<usize> = None;
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            let name = entry.name().to_lowercase();
            if name == "thumbnail.png" || name.ends_with("/thumbnail.png") {
                idx = Some(i);
                break;
            }
        }
    }
    let Some(idx) = idx else {
        return Ok(None);
    };
    let mut entry = archive
        .by_index(idx)
        .map_err(|e| format!("apertura thumbnail fallita: {e}"))?;
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry
        .read_to_end(&mut buf)
        .map_err(|e| format!("lettura thumbnail fallita: {e}"))?;
    Ok(Some(buf))
}

fn is_image_kind(kind: &str) -> bool {
    matches!(kind, "png" | "jpeg" | "gif" | "webp" | "image")
}

/// Lo script Node che pilota Playwright: stampa i byte PNG su stdout in base64
/// (per evitare problemi di encoding binario sui pipe) preceduti da `marker`.
fn script_screenshot(exe_path: &str, url: &str, cfg: &CompareSettings, marker: &str) -> String {
    format!(
        r#"
const {{ chromium }} = require('playwright');
(async () => {{
  let browser;
  try {{
    browser = await chromium.launch({{ headless: true, executablePath: {exe}, args: ['--no-sandbox'] }});
    const page = await browser.newPage({{ viewport: {{ width: {w}, height: {h} }} }});
    await page.goto({url}, {{ waitUntil: 'networkidle', timeout: {nav_timeout} }});
    await page.waitForTimeout({wait});
    const buf = await page.screenshot({{ fullPage: false }});
    process.stdout.write('{marker}' + buf.toString('base64'));
    await browser.close();
  }} catch (e) {{
    if (browser) {{ try {{ await browser.close(); }} catch (_) {{}} }}
    process.stderr.write('SHOT_ERROR:' + (e && e.message ? e.message : String(e)));
    process.exit(2);
  }}
}})();
"#,
        exe = serde_json::to_string(exe_path).unwrap_or_else(|_| "\"\"".to_string()),
        w = cfg.viewport_width,
        h = cfg.viewport_height,
        url = serde_json::to_string(url).unwrap_or_else(|_| "\"\"".to_string()),
        nav_timeout = cfg.screenshot_timeout_secs.saturating_mul(1000),
        wait = cfg.wait_ms,
        marker = marker,
    )
}

/// Cattura uno screenshot dell'URL via Playwright (driver gia' installato nel
/// progetto), pilotato da uno script Node inline. Riusa lo stesso runtime di
/// run_playwright_tests: nessuna nuova dipendenza headless.
async fn capture_screenshot(
    root: &Path,
    url: &str,
    cfg: &CompareSettings,
) -> Result<Vec<u8>, Fallimento> {
    const MARKER: &str = "NEXUS_SHOT_B64:";

    // Preflight d1: risolviamo il Chromium COMPLETO dalla cache Playwright via
    // il PUNTO UNICO (regola L). Se assente, ritorniamo subito un messaggio
    // AZIONABILE invece dell'errore generico "cattura screenshot fallita" che
    // costringeva l'agente a indovinare la causa. Passiamo executablePath allo
    // script Node cosi' non dipendiamo dalla risoluzione browser interna di
    // Playwright (che cerca path/revisioni non sempre allineati): e' la stessa
    // strategia --executable-path applicata al server MCP @playwright/mcp.
    // DEL SISTEMA: un browser assente non si rimedia riformulando la chiamata.
    let chromium_exe = crate::playwright_env::resolve_chromium_from_env().map_err(|e| {
        Fallimento::di_sistema(format!(
            "Chromium non disponibile per lo screenshot: {e}. \
             Dopo l'installazione il browser vive in \
             ~/.cache/ms-playwright/chromium-<rev>/chrome-linux64/chrome."
        ))
    })?;
    let exe_path = chromium_exe.to_string_lossy().to_string();
    let script = script_screenshot(&exe_path, url, cfg, MARKER);

    let mut cmd = Command::new("node");
    cmd.arg("-e")
        .arg(&script)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Il pacchetto `playwright` vive nella install centrale di Nexus (la
    // node_modules del workspace == WorkingDirectory del servizio), NON nel
    // progetto utente: un progetto Figma Make non ha (ne' deve avere) playwright
    // tra le sue dipendenze. Puntiamo NODE_PATH a quella install cosi'
    // visual_compare funziona su QUALSIASI progetto senza installare nulla nella
    // project_root (isolamento progetti, regola E). I browser restano nella
    // cache globale condivisa (~/.cache/ms-playwright). Non hardcoded: il path
    // deriva dalla CWD del processo (il systemd unit fissa WorkingDirectory).
    if let Ok(cwd) = std::env::current_dir() {
        let nm = cwd.join("node_modules");
        if nm.join("playwright").is_dir() {
            cmd.env("NODE_PATH", &nm);
        }
    }

    let mut child = cmd.spawn().map_err(|e| {
        Fallimento::di_sistema(format!(
            "avvio node fallito ({e}): assicurati che node e il pacchetto playwright \
             siano installati nel progetto"
        ))
    })?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Timeout complessivo = navigazione + wait + margine.
    let overall = Duration::from_secs(cfg.screenshot_timeout_secs.saturating_add(15))
        .saturating_add(Duration::from_millis(cfg.wait_ms));

    let status = tokio::time::timeout(overall, child.wait()).await;
    let status = match status {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(Fallimento::di_sistema(format!("attesa node fallita: {e}"))),
        Err(_) => {
            let _ = child.start_kill();
            // TRANSITORIO: un servizio che sta ancora finendo di compilare o di
            // idratare risponde al giro dopo, e ripetere e' la strategia giusta.
            return Err(Fallimento::transitorio(format!(
                "timeout {}s nello screenshot (load/render troppo lenti o url non raggiungibile)",
                overall.as_secs()
            )));
        }
    };

    let mut out = String::new();
    if let Some(mut s) = stdout {
        let _ = s.read_to_string(&mut out).await;
    }
    let mut errbuf = String::new();
    if let Some(mut s) = stderr {
        let _ = s.read_to_string(&mut errbuf).await;
    }

    if !status.success() {
        // RIMEDIABILE: il browser e' partito e non ha ottenuto la pagina —
        // connessione rifiutata, host o porta sbagliati, route inesistente. Sono
        // tutte cose che l'agente corregge, e il suggerimento lo dice.
        return Err(Fallimento::rimediabile(format!(
            "script screenshot fallito: {}",
            dettaglio_errore_script(&errbuf)
        )));
    }

    let b64 = out
        .find(MARKER)
        .map(|pos| out[pos + MARKER.len()..].trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            Fallimento::di_sistema("lo script non ha prodotto un'immagine (marcatore assente)")
        })?;

    B64.decode(b64)
        .map_err(|e| Fallimento::di_sistema(format!("decodifica screenshot base64 fallita: {e}")))
}

/// Il motivo che lo script Node ha scritto su stderr, o l'ultima riga utile.
fn dettaglio_errore_script(errbuf: &str) -> String {
    errbuf
        .lines()
        .find(|l| l.contains("SHOT_ERROR:"))
        .map(|l| l.replace("SHOT_ERROR:", "").trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| errbuf.lines().last().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "errore sconosciuto".to_string())
}

/// Salva lo screenshot PNG sotto la project_root (path-safe) e ritorna il path
/// relativo pulito.
async fn save_screenshot(root: &Path, bytes: &[u8]) -> Result<String, Fallimento> {
    let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f");
    let rel = format!("{SCREENSHOT_SUBDIR}/screenshot_{ts}.png");
    // Il percorso lo compone questo modulo, non l'agente: se non e' valido non
    // c'e' nessun parametro della chiamata da correggere.
    let (clean_rel, abs_target) = resolve_workspace_target(root, &rel)
        .map_err(|_| Fallimento::di_sistema("path screenshot non valido"))?;

    if let Some(parent) = abs_target.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| Fallimento::da_io(format!("creazione dir screenshot fallita: {e}"), &e))?;
    }
    tokio::fs::write(&abs_target, bytes)
        .await
        .map_err(|e| Fallimento::da_io(format!("scrittura screenshot fallita: {e}"), &e))?;
    Ok(clean_rel)
}

/// Esito del confronto visivo.
///
/// E' un TIPO e non un `Value` anonimo: prima la funzione impacchettava un JSON
/// e il chiamante lo ri-estraeva con le stesse chiavi scritte a mano una
/// seconda volta. Ogni nome di campo compare ora una volta sola, nel `json!`
/// finale del tool (regola L).
struct VisualComparison {
    /// `Null` quando il modello non ha prodotto un punteggio: assente e zero
    /// non sono la stessa cosa.
    similarity_score: Value,
    differences: Value,
    model_used: String,
    /// Valorizzato SOLO se la risposta non era nel formato richiesto.
    parse_error: Option<String>,
}

impl VisualComparison {
    fn from_model(model_used: &str, parsed: &Value) -> Self {
        Self {
            similarity_score: parsed.get("similarity_score").cloned().unwrap_or(Value::Null),
            differences: parsed.get("differences").cloned().unwrap_or_else(|| json!([])),
            model_used: model_used.to_string(),
            parse_error: None,
        }
    }

    /// Il modello ha risposto, ma non nel formato imposto: nessun punteggio
    /// inventato, si dichiara il fatto e si allega la risposta grezza troncata.
    fn unparsable(model_used: &str, raw: &str) -> Self {
        Self {
            similarity_score: Value::Null,
            differences: json!([]),
            model_used: model_used.to_string(),
            parse_error: Some(format!(
                "il modello non ha risposto col JSON richiesto; risposta grezza: {}",
                raw.chars().take(RAW_ANSWER_EXCERPT_CHARS).collect::<String>()
            )),
        }
    }
}

/// Il confronto vero e proprio, dallo screenshot gia' salvato.
async fn confronto_vision(
    ctx: &AgentToolContext,
    shot: &[u8],
    reference: &ReferenceImage,
    screenshot_path: String,
) -> RispostaTool {
    let shot_b64 = B64.encode(shot);
    let ref_b64 = B64.encode(&reference.bytes);
    let compare =
        compare_via_gateway(&ctx.db, &shot_b64, "image/png", &ref_b64, &reference.mime).await;
    let v = match compare {
        Ok(v) => v,
        Err(f) => return fallimento_confronto(&f, &screenshot_path, reference),
    };
    if let Some(motivo) = &v.parse_error {
        return risposta_non_interpretabile(motivo, &v, &screenshot_path, reference);
    }
    RispostaTool::riuscito(manifest_confronto(&v, &screenshot_path, &reference.source).to_string())
}

/// Il manifest del confronto RIUSCITO.
///
/// Estratto perche' la misura lo raggiunga per la stessa strada del tool
/// (regola O): il test che ne verificava la forma ricopiava le chiavi a mano,
/// quindi sarebbe rimasto verde anche rinominandone una qui.
fn manifest_confronto(v: &VisualComparison, screenshot_path: &str, reference_source: &str) -> Value {
    json!({
        "similarity_score": v.similarity_score,
        "differences": v.differences,
        "screenshot_path": screenshot_path,
        "reference_source": reference_source,
        "model_used": v.model_used,
    })
}

/// Il confronto non e' stato eseguito. Lo screenshot resta, e il payload lo
/// dice: e' l'unica cosa recuperabile di questa chiamata.
fn fallimento_confronto(
    f: &Fallimento,
    screenshot_path: &str,
    reference: &ReferenceImage,
) -> RispostaTool {
    let payload = json!({
        "error": format!("confronto vision fallito: {}", f.messaggio),
        "screenshot_path": screenshot_path,
        "reference_source": reference.source,
        "hint": "Lo screenshot e' stato salvato: puoi proseguire senza il punteggio invece di \
                 ripetere la cattura.",
    });
    fallito(payload, f.natura)
}

/// RAMO NUDO CHIUSO: il modello vision ha risposto FUORI dal formato imposto,
/// quindi nessun punteggio e' stato prodotto — e il tool usciva RIUSCITO, con
/// `similarity_score: null` e il motivo relegato in un campo `parse_error` che
/// nessun consumatore guarda. Per una chiamata con `reference` il confronto E'
/// il compito: non averlo fatto e' un fallimento, non una nota a margine. Il
/// gate che legge l'ultimo `similarity_score` dalla history (final_gate P5)
/// scartava gia' quel `null`, quindi l'unico a leggere «riuscito» era il modello.
///
/// TRANSITORIO come la completion vuota del gemello
/// `nexus_describe_image_attachment`: ripetere identica la chiamata la fa
/// ripassare da routing e failover del gateway, che e' cio' che serve a una
/// risposta malformata.
fn risposta_non_interpretabile(
    motivo: &str,
    v: &VisualComparison,
    screenshot_path: &str,
    reference: &ReferenceImage,
) -> RispostaTool {
    let payload = json!({
        "error": motivo,
        "screenshot_path": screenshot_path,
        "reference_source": reference.source,
        "model_used": v.model_used,
    });
    fallito(payload, NaturaFallimento::Transitorio)
}

/// Confronta le due immagini con una chiamata multimodale al gateway LLM.
///
/// Gemella di `vision_tools::nexus_describe_image_attachment`: stesso punto
/// unico [`gateway_vision_complete`] (regola L), stesso modo di risolvere il
/// modello (VIA TIER dal purpose, regola G — nessun nome modello qui) e stessa
/// lettura delle due nature. Il purpose non risolvibile e' una configurazione di
/// piattaforma che manca (DEL SISTEMA, e il messaggio nomina la migrazione); il
/// fallimento della CHIAMATA e' TRANSITORIO, perche' routing, cooldown e
/// failover li possiede gia' il gateway e cio' che arriva fin qui e' il loro
/// esaurimento.
///
/// Il brain Python esponeva `/vision/compare` e restituiva gia' il JSON. Ora il
/// confronto lo fa il modello vision: il prompt impone il formato, e la risposta
/// viene estratta col punto unico `llm_json::extract_json_block`. Un modello che
/// non rispetta il formato produce `parse_error`, non un punteggio inventato.
async fn compare_via_gateway(
    db: &sqlx::PgPool,
    screenshot_b64: &str,
    screenshot_mime: &str,
    reference_b64: &str,
    reference_mime: &str,
) -> Result<VisualComparison, Fallimento> {
    let (provider, model) =
        crate::internal_routing::resolve_purpose_model_db(db, VISUAL_COMPARE_PURPOSE)
            .await
            .into_model(VISUAL_COMPARE_PURPOSE)
            .map_err(|e| {
                Fallimento::di_sistema(format!(
                    "modello vision non risolvibile (purpose '{VISUAL_COMPARE_PURPOSE}'): {e}. \
                     Verifica nexus_purpose_model.visual_compare (mig 0214)."
                ))
            })?;

    let shot_uri = format!("data:{screenshot_mime};base64,{screenshot_b64}");
    let ref_uri = format!("data:{reference_mime};base64,{reference_b64}");
    let content_blocks = json!([
        { "type": "text", "text": VISUAL_COMPARE_PROMPT },
        { "type": "image_url", "image_url": { "url": shot_uri } },
        { "type": "image_url", "image_url": { "url": ref_uri } },
    ]);

    let result = nexus_agent_tools::gateway_client::gateway_vision_complete(
        db,
        &provider,
        &model,
        content_blocks,
        VISION_MAX_TOKENS,
        VISUAL_COMPARE_PURPOSE,
    )
    .await
    .map_err(Fallimento::transitorio)?;

    // Il punteggio non si inventa: se il modello non ha risposto nel formato
    // richiesto lo si DICHIARA (parse_error) e il chiamante lo rigira all'agente.
    let Some(parsed) = nexus_types::llm_json::extract_json_block(&result.content) else {
        return Ok(VisualComparison::unparsable(
            &result.model_used,
            &result.content,
        ));
    };
    Ok(VisualComparison::from_model(&result.model_used, &parsed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn make_zip_with(thumb: Option<&[u8]>) -> Vec<u8> {
        let mut buf: Vec<u8> = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut zw = zip::ZipWriter::new(cursor);
            let opts: SimpleFileOptions =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            zw.start_file("ai_chat.json", opts).unwrap();
            zw.write_all(br#"{"threads":[]}"#).unwrap();
            if let Some(t) = thumb {
                zw.start_file("thumbnail.png", opts).unwrap();
                zw.write_all(t).unwrap();
            }
            zw.finish().unwrap();
        }
        buf
    }

    fn riferimento_finto() -> ReferenceImage {
        ReferenceImage {
            bytes: vec![1, 2, 3],
            mime: "image/png".to_string(),
            source: "thumbnail".to_string(),
        }
    }

    #[test]
    fn err_dichiara_esito_e_natura_nei_campi() {
        // Chiama il PRODUTTORE reale usato da tutti i rami di errore semplici
        // del tool (permesso assente, url non valido, reference inutilizzabile).
        let out = err("motivo del fallimento", NaturaFallimento::Rimediabile);
        assert!(out.esito.e_fallito());
        assert_eq!(out.natura, Some(NaturaFallimento::Rimediabile));
        assert!(out.testo.contains("motivo del fallimento"), "{out:?}");
        // Senza marker in testa il corpo torna a essere un JSON integro.
        let parsed: Value = serde_json::from_str(&out.testo).expect("payload JSON valido");
        assert_eq!(parsed["error"], "motivo del fallimento");
    }

    #[test]
    fn url_rifiutato_nomina_il_campo_ed_e_rimediabile() {
        let vuoto = url_validato("   ").unwrap_err();
        assert!(vuoto.esito.e_fallito());
        assert_eq!(vuoto.natura, Some(NaturaFallimento::Rimediabile));
        assert!(vuoto.testo.contains("'url'"), "{vuoto:?}");

        let senza_schema = url_validato("localhost:3000").unwrap_err();
        assert_eq!(senza_schema.natura, Some(NaturaFallimento::Rimediabile));
        assert!(senza_schema.testo.contains("http://"), "{senza_schema:?}");

        assert_eq!(url_validato("  http://localhost:3000/ ").unwrap(), "http://localhost:3000/");
    }

    /// DIVERGENZA CHIUSA: con `as_u64` un viewport negativo o zero spariva e la
    /// chiamata proseguiva col default. Ora e' un rifiuto che nomina il campo.
    #[test]
    fn viewport_non_positivo_e_rifiutato_non_ignorato() {
        let zero = dimensione_viewport("width", 0, MAX_VIEWPORT_WIDTH).unwrap_err();
        assert_eq!(zero.natura, Some(NaturaFallimento::Rimediabile));
        assert!(zero.testo.contains("viewport.width"), "{zero:?}");

        let negativo = dimensione_viewport("height", -10, MAX_VIEWPORT_HEIGHT).unwrap_err();
        assert!(negativo.testo.contains("viewport.height"), "{negativo:?}");

        assert_eq!(dimensione_viewport("width", 1920, MAX_VIEWPORT_WIDTH).unwrap(), 1920);
        // Il tetto e' applicato, non e' un errore.
        assert_eq!(
            dimensione_viewport("width", 99_999, MAX_VIEWPORT_WIDTH).unwrap(),
            MAX_VIEWPORT_WIDTH as u32
        );
    }

    #[test]
    fn attesa_negativa_rifiutata_zero_ammesso() {
        let negativa = attesa_richiesta(-1).unwrap_err();
        assert_eq!(negativa.natura, Some(NaturaFallimento::Rimediabile));
        assert!(negativa.testo.contains("wait_ms"), "{negativa:?}");
        assert_eq!(attesa_richiesta(0).unwrap(), 0);
        assert_eq!(attesa_richiesta(10_000_000).unwrap(), MAX_WAIT_MS);
    }

    /// Il suggerimento deve CAMBIARE con la natura: era uno solo, e nominava
    /// insieme il dev server e l'installazione di Playwright.
    #[test]
    fn suggerimento_dipende_dalla_natura() {
        let rimediabile = suggerimento_scatto(NaturaFallimento::Rimediabile);
        let transitorio = suggerimento_scatto(NaturaFallimento::Transitorio);
        let sistema = suggerimento_scatto(NaturaFallimento::DelSistema);
        assert_ne!(rimediabile, transitorio);
        assert_ne!(transitorio, sistema);
        assert!(sistema.contains("Chromium"), "{sistema}");
        assert!(transitorio.contains("wait_ms"), "{transitorio}");
    }

    #[test]
    fn dettaglio_script_preferisce_il_marcatore() {
        let stderr = "rumore di npm\nSHOT_ERROR: net::ERR_CONNECTION_REFUSED\naltro";
        assert_eq!(dettaglio_errore_script(stderr), "net::ERR_CONNECTION_REFUSED");
        assert_eq!(dettaglio_errore_script("   \n  \n"), "errore sconosciuto");
        assert_eq!(dettaglio_errore_script(""), "errore sconosciuto");
    }

    #[test]
    fn thumbnail_extracted_when_present() {
        let png = b"\x89PNG\r\n\x1a\n_fake_png_body";
        let zip = make_zip_with(Some(png));
        let got = extract_thumbnail_bytes(&zip).expect("estrazione ok");
        assert_eq!(got.as_deref(), Some(&png[..]));
    }

    #[test]
    fn thumbnail_none_when_absent() {
        let zip = make_zip_with(None);
        let got = extract_thumbnail_bytes(&zip).expect("estrazione ok");
        assert!(got.is_none());
    }

    #[test]
    fn thumbnail_errors_on_non_zip() {
        let raw = b"not a zip at all";
        let err = extract_thumbnail_bytes(raw).unwrap_err();
        assert!(err.contains("ZIP"));
    }

    #[test]
    fn is_image_kind_classifies() {
        assert!(is_image_kind("png"));
        assert!(is_image_kind("jpeg"));
        assert!(is_image_kind("image"));
        assert!(!is_image_kind("figma"));
        assert!(!is_image_kind("pdf"));
    }

    /// Serializzazione del manifest di output (successo): i campi attesi dallo
    /// schema FASE 2 devono essere tutti presenti e ben formati. Passa dal
    /// PRODUTTORE reale (`manifest_confronto`), non da un json! ricopiato a mano.
    #[test]
    fn output_manifest_success_shape() {
        let vision = VisualComparison::from_model(
            "google/gemini-2.0-flash-exp",
            &json!({
                "similarity_score": 78,
                "differences": [
                    {"category": "colore", "severity": "alta", "description": "palette diversa"}
                ],
            }),
        );
        let out = manifest_confronto(&vision, ".nexus/visual_compare/screenshot_x.png", "thumbnail");

        // Round-trip stringa -> Value per simulare il contratto del tool.
        let s = out.to_string();
        let parsed: Value = serde_json::from_str(&s).expect("manifest JSON valido");
        assert_eq!(parsed["similarity_score"], 78);
        assert_eq!(parsed["reference_source"], "thumbnail");
        assert_eq!(parsed["model_used"], "google/gemini-2.0-flash-exp");
        assert_eq!(parsed["differences"][0]["category"], "colore");
        assert_eq!(parsed["differences"][0]["severity"], "alta");
        assert!(parsed["screenshot_path"].as_str().unwrap().ends_with(".png"));
    }

    /// Il manifest di errore non deve contenere le immagini base64 e deve
    /// preservare il path dello screenshot se gia' salvato.
    #[test]
    fn output_manifest_error_has_no_images() {
        let out = fallimento_confronto(
            &Fallimento::transitorio("brain down"),
            ".nexus/visual_compare/screenshot_y.png",
            &riferimento_finto(),
        );
        assert!(out.esito.e_fallito());
        assert!(!out.testo.contains("base64"), "{out:?}");
        let parsed: Value = serde_json::from_str(&out.testo).expect("payload JSON valido");
        assert!(parsed.get("error").is_some());
        assert_eq!(parsed["reference_source"], "thumbnail");
        assert!(parsed["screenshot_path"].as_str().unwrap().ends_with(".png"));
    }

    /// RAMO NUDO: una risposta vision fuori formato usciva come SUCCESSO, con
    /// `similarity_score: null`. Rimettere `RispostaTool::riuscito` qui fa
    /// rosseggiare questo test.
    #[test]
    fn risposta_fuori_formato_e_un_fallimento_transitorio() {
        let vision = VisualComparison::unparsable("google/gemini-2.0-flash-exp", "ecco il mio parere: le immagini sono simili");
        let motivo = vision.parse_error.clone().expect("unparsable dichiara il motivo");
        let out = risposta_non_interpretabile(
            &motivo,
            &vision,
            ".nexus/visual_compare/screenshot_z.png",
            &riferimento_finto(),
        );
        assert!(out.esito.e_fallito(), "{out:?}");
        assert_eq!(out.natura, Some(NaturaFallimento::Transitorio));
        let parsed: Value = serde_json::from_str(&out.testo).expect("payload JSON valido");
        // Nessun punteggio inventato, e nessun `similarity_score` nel payload:
        // il gate P5 legge l'ULTIMO che trova nella history.
        assert!(parsed.get("similarity_score").is_none(), "{parsed}");
        assert_eq!(parsed["model_used"], "google/gemini-2.0-flash-exp");
        assert!(parsed["error"].as_str().unwrap().contains("JSON richiesto"));
    }
}
