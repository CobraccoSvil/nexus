//! Il CONFINE col browser reale: carica una pagina in Chromium headless e
//! riporta i fatti osservati (richieste di rete, errori di console). Non
//! giudica: il criterio e' il punto unico puro
//! [`nexus_agent_graph::decisions::browser_dialogue`], che riceve questi fatti.
//!
//! Perche' un browser e non una richiesta lato server: una `reqwest` non manda
//! `Origin` e non applica la same-origin policy, quindi un backend senza CORS
//! le risponde 200 mentre il browser la blocca; e non esegue JS, quindi non
//! vede l'URL che il codice client costruisce davvero. Sono le due cause
//! misurate su biblioteca-scolastica il 06/08/2026, entrambe invisibili a una
//! probe HTTP per costruzione (vedi la doc del modulo di decisione).
//!
//! AGNOSTICO ALLO STACK, e non per dichiarazione: `NODE_PATH` punta alla
//! `node_modules` DI NEXUS (stessa scelta gia' in esercizio in
//! `visual_compare`), quindi il progetto osservato non deve avere playwright,
//! npm o alcuna dipendenza. Vale identico per React+Vite, Next.js, .NET o
//! Django: si guarda la pagina servita, mai lo stack che la serve.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use nexus_agent_graph::decisions::browser_dialogue::{ProveBrowser, RichiestaOsservata};

/// Marcatore del payload JSON sullo stdout dello script Node.
const MARCATORE: &str = "NEXUS_BROWSER_JSON:";

/// Margine oltre il timeout di navigazione, per l'avvio del browser.
const MARGINE_AVVIO_S: u64 = 15;

/// Carica `url` in Chromium headless e riporta i fatti osservati.
///
/// `Err` = la MISURA non e' stata possibile (Chromium assente, node assente,
/// timeout): il chiamante deve tradurlo in `Inconclusive`, mai in un
/// fallimento del progetto — «non ho potuto guardare» non e' un difetto.
pub async fn osserva_pagina(
    root: &Path,
    url: &str,
    attesa_ms: u64,
    timeout_s: u64,
) -> Result<ProveBrowser, String> {
    // Punto unico della risoluzione del binario (regola L), lo stesso usato da
    // visual_compare e dal server MCP @playwright/mcp.
    let chromium = crate::playwright_env::resolve_chromium_from_env().map_err(|e| {
        format!(
            "Chromium non disponibile per l'osservazione del frontend: {e}. \
             Dopo l'installazione il browser vive in \
             ~/.cache/ms-playwright/chromium-<rev>/chrome-linux64/chrome."
        )
    })?;

    let script = script_osservazione(
        &chromium.to_string_lossy(),
        url,
        attesa_ms,
        timeout_s.saturating_mul(1000),
    );

    let complessivo = Duration::from_secs(timeout_s.saturating_add(MARGINE_AVVIO_S))
        .saturating_add(Duration::from_millis(attesa_ms));
    let (out, err) = esegui_script(root, &script, complessivo).await?;

    let payload = out
        .find(MARCATORE)
        .map(|p| out[p + MARCATORE.len()..].trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            let dettaglio = err
                .lines()
                .last()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("nessun dettaglio");
            format!("lo script non ha prodotto fatti osservabili: {dettaglio}")
        })?;

    interpreta(payload)
}

/// Esegue lo script Node e ritorna `(stdout, stderr)`. Il timeout uccide il
/// processo: un browser che non chiude lascerebbe un Chromium orfano a ogni
/// invocazione del gate.
async fn esegui_script(
    root: &Path,
    script: &str,
    complessivo: Duration,
) -> Result<(String, String), String> {
    let mut cmd = Command::new("node");
    cmd.arg("-e")
        .arg(script)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // La install di playwright e' quella di NEXUS, non del progetto osservato:
    // e' questo a rendere la misura utilizzabile su qualunque stack senza
    // installare nulla nella project_root (isolamento progetti, regola E).
    if let Ok(cwd) = std::env::current_dir() {
        let nm = cwd.join("node_modules");
        if nm.join("playwright").is_dir() {
            cmd.env("NODE_PATH", &nm);
        }
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("avvio node fallito ({e}): node non disponibile"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    match tokio::time::timeout(complessivo, child.wait()).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(format!("attesa node fallita: {e}")),
        Err(_) => {
            let _ = child.start_kill();
            return Err(format!(
                "timeout {}s nell'osservazione della pagina",
                complessivo.as_secs()
            ));
        }
    }

    let mut out = String::new();
    if let Some(mut s) = stdout {
        let _ = s.read_to_string(&mut out).await;
    }
    let mut err = String::new();
    if let Some(mut s) = stderr {
        let _ = s.read_to_string(&mut err).await;
    }
    Ok((out, err))
}

/// Traduce il JSON dello script nei fatti tipizzati. Separata dall'I/O perche'
/// e' esercitabile senza browser (regola O: il test attraversa il produttore
/// vero, che qui e' questa funzione, non un letterale fabbricato a mano).
pub fn interpreta(payload: &str) -> Result<ProveBrowser, String> {
    let v: serde_json::Value = serde_json::from_str(payload)
        .map_err(|e| format!("fatti del browser non leggibili: {e}"))?;
    let richieste = v
        .get("requests")
        .and_then(|r| r.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    Some(RichiestaOsservata {
                        url: r.get("url")?.as_str()?.to_string(),
                        status: r.get("status").and_then(|s| s.as_u64()).map(|s| s as u16),
                        errore: r
                            .get("error")
                            .and_then(|e| e.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let errori_console = v
        .get("consoleErrors")
        .and_then(|c| c.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| m.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(ProveBrowser {
        richieste,
        errori_console,
        pagina_caricata: v
            .get("loaded")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

/// Lo script Node che osserva. Registra gli ascoltatori PRIMA della
/// navigazione: una richiesta partita durante il caricamento non deve
/// sfuggire. Le richieste della NAVIGAZIONE stessa (il documento) restano
/// fuori: qui si misura il dialogo della pagina coi propri dati, e il
/// documento e' gia' coperto dalla readiness del servizio.
fn script_osservazione(chromium: &str, url: &str, attesa_ms: u64, nav_timeout_ms: u64) -> String {
    format!(
        r#"
const {{ chromium }} = require('playwright');
(async () => {{
  const fatti = {{ requests: [], consoleErrors: [], loaded: false }};
  let browser;
  try {{
    browser = await chromium.launch({{ headless: true, executablePath: {exe}, args: ['--no-sandbox'] }});
    const page = await browser.newPage();
    page.on('console', (m) => {{
      if (m.type() === 'error') fatti.consoleErrors.push(String(m.text()).slice(0, 500));
    }});
    page.on('pageerror', (e) => {{
      fatti.consoleErrors.push(String(e && e.message ? e.message : e).slice(0, 500));
    }});
    page.on('requestfailed', (r) => {{
      if (r.resourceType() === 'document') return;
      const f = r.failure();
      fatti.requests.push({{ url: r.url(), error: (f && f.errorText) ? f.errorText : 'richiesta fallita' }});
    }});
    page.on('response', (r) => {{
      if (r.request().resourceType() === 'document') return;
      fatti.requests.push({{ url: r.url(), status: r.status() }});
    }});
    const resp = await page.goto({url}, {{ waitUntil: 'domcontentloaded', timeout: {nav} }});
    fatti.loaded = !!resp;
    // Le chiamate dati partono dopo il primo render: si attende che la rete si
    // calmi, con un tetto, e non un istante fisso.
    try {{ await page.waitForLoadState('networkidle', {{ timeout: {attesa} }}); }} catch (_) {{}}
    await browser.close();
  }} catch (e) {{
    if (browser) {{ try {{ await browser.close(); }} catch (_) {{}} }}
    process.stderr.write('OSSERVA_ERRORE:' + (e && e.message ? e.message : String(e)));
  }}
  process.stdout.write('{marcatore}' + JSON.stringify(fatti));
}})();
"#,
        exe = serde_json::to_string(chromium).unwrap_or_else(|_| "\"\"".into()),
        url = serde_json::to_string(url).unwrap_or_else(|_| "\"\"".into()),
        nav = nav_timeout_ms,
        attesa = attesa_ms.max(1000),
        marcatore = MARCATORE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// I fatti arrivano dal PRODUTTORE vero (`interpreta`), non da una
    /// struttura fabbricata: e' la giunzione fra lo script e il criterio, ed
    /// e' esattamente dove un rinominamento di campo passerebbe inosservato.
    #[test]
    fn i_fatti_del_browser_attraversano_il_produttore() {
        let payload = r#"{"loaded":true,
          "requests":[
            {"url":"http://localhost:35954/api/books","status":200},
            {"url":"http://127.0.0.1:35976/api/api/books","error":"net::ERR_FAILED"}
          ],
          "consoleErrors":["Access to XMLHttpRequest ... blocked by CORS policy"]}"#;
        let p = interpreta(payload).expect("fatti leggibili");
        assert!(p.pagina_caricata);
        assert_eq!(p.richieste.len(), 2);
        assert_eq!(p.richieste[0].status, Some(200));
        assert_eq!(p.richieste[1].status, None, "senza risposta = status assente");
        assert_eq!(p.richieste[1].errore, "net::ERR_FAILED");
        assert_eq!(p.errori_console.len(), 1);

        // E il criterio, sugli stessi fatti, dichiara il difetto.
        use nexus_agent_graph::decisions::browser_dialogue::{classifica_dialogo, VerdettoDialogo};
        assert!(matches!(
            classifica_dialogo(&p, &[]),
            VerdettoDialogo::Rotto { .. }
        ));
    }

    /// Un payload senza `loaded` NON diventa una pagina caricata: il default
    /// e' il caso prudente, perche' un `false` mancante non e' un successo.
    #[test]
    fn il_campo_assente_non_diventa_successo() {
        let p = interpreta(r#"{"requests":[]}"#).expect("leggibile");
        assert!(!p.pagina_caricata);
        assert!(interpreta("non-json").is_err());
    }

    /// Lo script porta gli ascoltatori PRIMA del goto e cita il binario
    /// risolto: se qualcuno spostasse la registrazione dopo la navigazione, le
    /// chiamate del primo render sparirebbero e la misura direbbe «nessuna
    /// richiesta» su una pagina rotta.
    #[test]
    fn gli_ascoltatori_precedono_la_navigazione() {
        let s = script_osservazione("/opt/chrome", "http://x", 2000, 30000);
        let pos_listener = s.find("page.on('response'").expect("ascoltatore risposte");
        let pos_goto = s.find("page.goto").expect("navigazione");
        assert!(
            pos_listener < pos_goto,
            "gli ascoltatori vanno registrati prima della navigazione"
        );
        assert!(s.contains("requestfailed"), "le richieste fallite sono il segnale principale");
        assert!(s.contains("\"/opt/chrome\""), "il binario risolto entra nello script");
    }
}

#[cfg(test)]
mod prova_dal_vivo {
    use super::*;

    /// LA PROVA CHE CONTA (regola O: lo strumento raggiunge il suo oggetto per
    /// la strada della produzione). Osserva un'origine REALE con lo stesso
    /// percorso che usera' il gate e stampa il verdetto.
    ///   cargo test --bin mcp-core -- --ignored --nocapture osserva_origine
    /// URL da `NEXUS_PROVA_URL`, altrimenti il frontend del progetto di prova.
    #[tokio::test]
    #[ignore]
    async fn osserva_origine_reale() {
        use nexus_agent_graph::decisions::browser_dialogue::classifica_dialogo;
        let url = std::env::var("NEXUS_PROVA_URL")
            .unwrap_or_else(|_| "http://localhost:35954".to_string());
        let root = std::env::current_dir().expect("cwd");
        let prove = osserva_pagina(&root, &url, 2500, 30)
            .await
            .unwrap_or_else(|e| panic!("osservazione non riuscita: {e}"));
        println!("caricata={} richieste={}", prove.pagina_caricata, prove.richieste.len());
        for r in &prove.richieste {
            println!("  {} -> {:?} {}", r.url, r.status, r.errore);
        }
        for c in prove.errori_console.iter().take(5) {
            println!("  console: {c}");
        }
        println!("VERDETTO: {:?}", classifica_dialogo(&prove, &[]));
    }
}
