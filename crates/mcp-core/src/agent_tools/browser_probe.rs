//! Il CONFINE col browser reale: carica una pagina in Chromium headless e
//! riporta i fatti osservati (richieste di rete, errori di console ed
//! esecuzione, DOM reso). Non giudica: i criteri sono i punti unici puri
//! [`nexus_agent_graph::decisions::browser_dialogue`] («la pagina ottiene i
//! propri dati?») e [`nexus_agent_graph::decisions::static_render`] («la pagina
//! mostra il proprio contenuto?»), che ricevono questi fatti.
//!
//! Le due domande condividono il confine e non lo script: uno solo, una sola
//! esecuzione, due interpreti sui campi che a ciascuna competono. Due script
//! divergerebbero, e la divergenza si vedrebbe come due criteri che misurano
//! cose leggermente diverse senza che nessuno sappia dire quali (regola L).
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
use nexus_agent_graph::decisions::static_render::{EsitoContenitore, ProveResa};

/// Marcatore del payload JSON sullo stdout dello script Node.
const MARCATORE: &str = "NEXUS_BROWSER_JSON:";

/// I campi del payload: il CONTRATTO fra lo script che li scrive e i due
/// interpreti che li leggono.
///
/// Stanno qui perche' quel contratto e' una giunzione fra due linguaggi, dove
/// nessun compilatore puo' accorgersi di un nome cambiato da un lato solo: e'
/// la stessa forma di difetto misurata su `agent_steps`, dove il produttore
/// scriveva `tool_name` e il consumatore leggeva `name` per 8860 righe. Qui il
/// nome vive in un posto, e il test `lo_script_e_gli_interpreti_usano_gli_stessi_campi`
/// verifica che lo script generato li contenga davvero — un letterale che
/// nessuno controlla non sarebbe piu' sicuro di prima.
mod campo {
    pub const RICHIESTE: &str = "requests";
    pub const ERRORI_CONSOLE: &str = "consoleErrors";
    pub const ERRORI_PAGINA: &str = "pageErrors";
    pub const CARICATA: &str = "loaded";
    pub const ELEMENTI: &str = "elementCount";
    pub const CONTENITORE: &str = "container";

    /// Tutti, per il test che li confronta con lo script.
    pub const TUTTI: [&str; 6] = [
        RICHIESTE,
        ERRORI_CONSOLE,
        ERRORI_PAGINA,
        CARICATA,
        ELEMENTI,
        CONTENITORE,
    ];
}

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
    let payload = raccogli(root, url, attesa_ms, timeout_s, None).await?;
    interpreta(&payload)
}

/// Apre `url` in Chromium headless e riporta cosa la pagina ha davvero RESO.
///
/// Stessa strada di [`osserva_pagina`] — stesso binario, stesso script, stessa
/// esecuzione — perche' e' la stessa operazione: aprire una pagina e guardarla.
/// Cambia la domanda, non il confine, e cio' che cambia sta negli INTERPRETI.
/// Due script separati divergerebbero, e la prima divergenza si vedrebbe come
/// un criterio che misura qualcosa di leggermente diverso dall'altro senza che
/// nessuno sappia dire cosa (regola L).
///
/// `selettore` e' il contenitore dichiarato, quando c'e': senza, restano i due
/// segnali che non richiedono dichiarazioni (eccezioni non gestite e body reso).
pub async fn osserva_resa(
    root: &Path,
    url: &str,
    selettore: Option<&str>,
    attesa_ms: u64,
    timeout_s: u64,
) -> Result<ProveResa, String> {
    let payload = raccogli(root, url, attesa_ms, timeout_s, selettore).await?;
    interpreta_resa(&payload)
}

/// Il confine vero: risolve il browser, esegue lo script, estrae il payload
/// marcato. Una sola volta per entrambe le domande.
async fn raccogli(
    root: &Path,
    url: &str,
    attesa_ms: u64,
    timeout_s: u64,
    selettore: Option<&str>,
) -> Result<String, String> {
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
        selettore,
    );

    let complessivo = Duration::from_secs(timeout_s.saturating_add(MARGINE_AVVIO_S))
        .saturating_add(Duration::from_millis(attesa_ms));
    let (out, err) = esegui_script(root, &script, complessivo).await?;

    out.find(MARCATORE)
        .map(|p| out[p + MARCATORE.len()..].trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            let dettaglio = err
                .lines()
                .last()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("nessun dettaglio");
            format!("lo script non ha prodotto fatti osservabili: {dettaglio}")
        })
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
        .get(campo::RICHIESTE)
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
    // Il dialogo non distingue un'eccezione da un avviso: per la sua domanda
    // sono entrambi contorno dell'evidenza, e li teneva gia' in un'unica lista.
    // La distinzione la fa `interpreta_resa`, dove FA la differenza.
    let mut errori_console = lista(&v, campo::ERRORI_CONSOLE);
    errori_console.extend(lista(&v, campo::ERRORI_PAGINA));
    Ok(ProveBrowser {
        richieste,
        errori_console,
        pagina_caricata: caricata(&v),
    })
}

/// Traduce lo STESSO JSON nei fatti della resa. Gemella di [`interpreta`], e
/// separata da essa perche' le due domande leggono campi diversi degli stessi
/// fatti: qui l'eccezione non gestita e' il segnale principale, li' era
/// contorno.
pub fn interpreta_resa(payload: &str) -> Result<ProveResa, String> {
    let v: serde_json::Value = serde_json::from_str(payload)
        .map_err(|e| format!("fatti del browser non leggibili: {e}"))?;
    // Assente = non contato. NON zero: uno zero direbbe «pagina vuota», e la
    // differenza fra «non ho guardato» e «ho guardato e non c'era niente» e'
    // proprio cio' che separa un inconcludente da una bocciatura.
    let elementi_resi = v
        .get(campo::ELEMENTI)
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as usize);
    let contenitore = v.get(campo::CONTENITORE).and_then(|c| {
        let trovato = c.get("found").and_then(serde_json::Value::as_bool)?;
        Some(if trovato {
            EsitoContenitore::Trovato {
                figli: c
                    .get("children")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as usize,
            }
        } else {
            EsitoContenitore::Assente
        })
    });
    Ok(ProveResa {
        pagina_caricata: caricata(&v),
        elementi_resi,
        contenitore,
        errori_esecuzione: lista(&v, campo::ERRORI_PAGINA),
        errori_console: lista(&v, campo::ERRORI_CONSOLE),
    })
}

/// Una lista di stringhe del payload, vuota se assente.
fn lista(v: &serde_json::Value, campo: &str) -> Vec<String> {
    v.get(campo)
        .and_then(|c| c.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| m.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// `loaded` assente = pagina NON caricata: il default e' il caso prudente,
/// perche' un campo mancante non e' un successo.
fn caricata(v: &serde_json::Value) -> bool {
    v.get(campo::CARICATA)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// Lo script Node che osserva: il TRONCO comune alle due domande — lancia il
/// browser, registra gli ascoltatori ([`ASCOLTATORI`]), naviga, attende che la
/// rete si calmi, misura il DOM ([`MISURA_DOM`]), chiude.
///
/// I due frammenti stanno fuori perche' ciascuno risponde a una domanda sola,
/// e qui resterebbero due blocchi di JavaScript dentro cui l'ordine delle
/// operazioni — che e' l'unica cosa che questa funzione decide — non si legge
/// piu'. L'ordine conta: gli ascoltatori PRIMA della navigazione (una richiesta
/// partita durante il caricamento non deve sfuggire), il conteggio DOPO
/// l'attesa (contarlo prima misurerebbe l'HTML sorgente, cioe' proprio cio' che
/// il gate gia' sapeva guardare).
fn script_osservazione(
    chromium: &str,
    url: &str,
    attesa_ms: u64,
    nav_timeout_ms: u64,
    selettore: Option<&str>,
) -> String {
    format!(
        r#"
const {{ chromium }} = require('playwright');
(async () => {{
  const fatti = {{ requests: [], consoleErrors: [], pageErrors: [], loaded: false }};
  const SEL = {sel};
  let browser;
  try {{
    browser = await chromium.launch({{ headless: true, executablePath: {exe}, args: ['--no-sandbox'] }});
    const page = await browser.newPage();
{ascoltatori}
    const resp = await page.goto({url}, {{ waitUntil: 'domcontentloaded', timeout: {nav} }});
    fatti.loaded = !!resp;
    // Le chiamate dati partono dopo il primo render: si attende che la rete si
    // calmi, con un tetto, e non un istante fisso.
    try {{ await page.waitForLoadState('networkidle', {{ timeout: {attesa} }}); }} catch (_) {{}}
{misura}
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
        sel = selettore
            .map(|s| serde_json::to_string(s).unwrap_or_else(|_| "null".into()))
            .unwrap_or_else(|| "null".into()),
        nav = nav_timeout_ms,
        attesa = attesa_ms.max(1000),
        ascoltatori = ASCOLTATORI,
        misura = MISURA_DOM,
        marcatore = MARCATORE,
    )
}

/// Gli ascoltatori degli eventi della pagina, registrati PRIMA della
/// navigazione: una richiesta partita durante il caricamento non deve sfuggire.
///
/// Le richieste della NAVIGAZIONE stessa (il documento) restano fuori: qui si
/// misura cio' che la pagina chiede per conto proprio, e il documento e' gia'
/// coperto da chi ha stabilito che il servizio risponde.
///
/// `pageerror` ha una lista PROPRIA, separata dai `console.error`: per il
/// dialogo la distinzione non serve (nessuna delle due entra nel suo verdetto),
/// per la resa E' il verdetto — un'eccezione ha interrotto l'esecuzione, un
/// avviso di libreria no.
const ASCOLTATORI: &str = r#"
    page.on('console', (m) => {
      if (m.type() === 'error') fatti.consoleErrors.push(String(m.text()).slice(0, 500));
    });
    page.on('pageerror', (e) => {
      fatti.pageErrors.push(String(e && e.message ? e.message : e).slice(0, 500));
    });
    page.on('requestfailed', (r) => {
      if (r.resourceType() === 'document') return;
      const f = r.failure();
      fatti.requests.push({ url: r.url(), error: (f && f.errorText) ? f.errorText : 'richiesta fallita' });
    });
    page.on('response', (r) => {
      if (r.request().resourceType() === 'document') return;
      fatti.requests.push({ url: r.url(), status: r.status() });
    });"#;

/// Il frammento che misura il DOM RESO, dopo che il JS ha girato.
///
/// Restano fuori gli elementi che non sono contenuto (script, stili,
/// metadati): contarli direbbe «pagina piena» di una pagina che non mostra
/// nulla. Se una delle due `evaluate` fallisce il campo resta ASSENTE, non
/// zero: «non ho potuto contare» e «ho contato zero» sono risposte diverse, e
/// il criterio le tratta diversamente.
///
/// Costante e non inline nel template: e' l'unica parte dello script che
/// riguarda la sola domanda della resa, e tenerla a parte lascia leggibile il
/// tronco comune alle due misure.
const MISURA_DOM: &str = r#"
    if (fatti.loaded) {
      try {
        fatti.elementCount = await page.evaluate(() => document.body
          ? document.body.querySelectorAll('*:not(script):not(style):not(link):not(meta):not(template):not(noscript)').length
          : 0);
      } catch (_) {}
      if (SEL) {
        try {
          fatti.container = await page.evaluate((s) => {
            const el = document.querySelector(s);
            return el ? { found: true, children: el.children.length } : { found: false, children: 0 };
          }, SEL);
        } catch (_) {}
      }
    }"#;

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
        let s = script_osservazione("/opt/chrome", "http://x", 2000, 30000, None);
        let pos_listener = s.find("page.on('response'").expect("ascoltatore risposte");
        let pos_goto = s.find("page.goto").expect("navigazione");
        assert!(
            pos_listener < pos_goto,
            "gli ascoltatori vanno registrati prima della navigazione"
        );
        assert!(s.contains("requestfailed"), "le richieste fallite sono il segnale principale");
        assert!(s.contains("\"/opt/chrome\""), "il binario risolto entra nello script");
    }

    /// Il conteggio del DOM viene DOPO l'attesa che la pagina si calmi: contarlo
    /// prima misurerebbe l'HTML sorgente, cioe' proprio cio' che il gate gia'
    /// sapeva guardare e che non risponde alla domanda.
    ///
    /// MUTAZIONE: spostare `elementCount` prima del `waitForLoadState` -> questo
    /// test cade, e col difetto reale (si conterebbe il DOM pre-generazione,
    /// identico per una pagina che funziona e per una che ha lanciato).
    #[test]
    fn il_conteggio_segue_l_attesa() {
        let s = script_osservazione("/opt/chrome", "http://x", 2000, 30000, Some("#griglia"));
        let pos_attesa = s.find("waitForLoadState").expect("attesa");
        let pos_conteggio = s.find("elementCount").expect("conteggio");
        assert!(
            pos_attesa < pos_conteggio,
            "il DOM si conta dopo che il JS ha girato, non prima"
        );
        assert!(s.contains("\"#griglia\""), "il selettore dichiarato entra nello script");
        // Senza dichiarazione il browser non cerca nulla: un selettore vuoto
        // farebbe cercare un elemento che nessuno ha chiesto.
        assert!(script_osservazione("/opt/chrome", "http://x", 2000, 30000, None)
            .contains("const SEL = null"));
    }

    /// Il CONTRATTO fra i due linguaggi: ogni campo che gli interpreti leggono
    /// deve essere un campo che lo script scrive.
    ///
    /// Nessun compilatore guarda questa giunzione — da un lato c'e' Rust,
    /// dall'altro una stringa di JavaScript — ed e' la stessa forma del difetto
    /// misurato su `agent_steps`, dove il produttore scriveva `tool_name`, il
    /// consumatore leggeva `name`, e 8860 righe su 8860 sono uscite vuote senza
    /// che nulla fallisse.
    ///
    /// MUTAZIONE: rinominare un campo in `mod campo` senza toccare lo script
    /// (o viceversa) -> questo test cade, col nome esatto del campo scollegato.
    #[test]
    fn lo_script_e_gli_interpreti_usano_gli_stessi_campi() {
        let s = script_osservazione("/opt/chrome", "http://x", 2000, 30000, Some("#g"));
        for nome in campo::TUTTI {
            assert!(
                s.contains(nome),
                "il campo `{nome}` e' letto dagli interpreti ma lo script non lo scrive"
            );
        }
    }

    /// I fatti della RESA attraversano il produttore vero, ed e' la giunzione
    /// col criterio: `pageErrors` e `consoleErrors` sono liste DISTINTE, e
    /// scambiarle o fonderle qui renderebbe un avviso di libreria una
    /// bocciatura (o un'eccezione un dettaglio ignorato).
    #[test]
    fn i_fatti_della_resa_attraversano_il_produttore() {
        use nexus_agent_graph::decisions::static_render::{
            classifica_resa, VerdettoResa,
        };
        let payload = r#"{"loaded":true,"elementCount":48,
          "container":{"found":true,"children":0},
          "consoleErrors":["[Violation] handler took 62ms"],
          "pageErrors":["ReferenceError: courses is not defined"]}"#;
        let p = interpreta_resa(payload).expect("fatti leggibili");
        assert!(p.pagina_caricata);
        assert_eq!(p.elementi_resi, Some(48));
        assert_eq!(p.contenitore, Some(EsitoContenitore::Trovato { figli: 0 }));
        assert_eq!(p.errori_esecuzione.len(), 1, "l'eccezione sta nella sua lista");
        assert_eq!(p.errori_console.len(), 1, "l'avviso resta contorno");

        // E il criterio, sugli stessi fatti, dichiara il difetto.
        assert!(matches!(
            classifica_resa(&p, 5),
            VerdettoResa::NonResa { .. }
        ));
    }

    /// Un conteggio ASSENTE non e' uno zero: la pagina non e' stata misurata, e
    /// il criterio deve poterlo dire. MUTAZIONE: `unwrap_or(0)` al posto di
    /// `map` -> il criterio boccerebbe come «pagina vuota» cio' che non ha
    /// guardato.
    #[test]
    fn il_conteggio_assente_non_diventa_zero() {
        let p = interpreta_resa(r#"{"loaded":true}"#).expect("leggibile");
        assert_eq!(p.elementi_resi, None);
        assert_eq!(p.contenitore, None, "nessuna dichiarazione, nessun contenitore");
        assert!(interpreta_resa("non-json").is_err());
    }

    /// Lo stesso payload serve DUE domande, e il dialogo non cambia
    /// comportamento: continua a vedere entrambe le famiglie di errori nella
    /// sua unica lista, come prima della separazione.
    #[test]
    fn il_dialogo_vede_ancora_entrambe_le_famiglie() {
        let payload = r#"{"loaded":true,"requests":[],
          "consoleErrors":["avviso"],"pageErrors":["eccezione"]}"#;
        let d = interpreta(payload).expect("leggibile");
        assert_eq!(d.errori_console.len(), 2);
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

    /// La prova sul caso che ha originato il criterio: una pagina STATICA il
    /// cui contenuto non sta nel proprio HTML.
    ///   cargo test --bin mcp-core -- --ignored --nocapture osserva_pagina_statica
    /// URL da `NEXUS_PROVA_URL`, selettore opzionale da `NEXUS_PROVA_SELETTORE`.
    ///
    /// Il test di MUTAZIONE si fa sull'oggetto reale: si introduce un `throw`
    /// prima della chiamata che genera il contenuto e si rilancia — il verdetto
    /// deve passare da `Resa` a `NonResa`.
    #[tokio::test]
    #[ignore]
    async fn osserva_pagina_statica_reale() {
        use nexus_agent_graph::decisions::static_render::{
            cause_con_selettore, classifica_resa,
        };
        let url = std::env::var("NEXUS_PROVA_URL").unwrap_or_else(|_| {
            "http://127.0.0.1:4000/preview/e4d446ce-28a4-44a9-bcab-d7a78b0541b4/landing/index.html"
                .to_string()
        });
        let sel = std::env::var("NEXUS_PROVA_SELETTORE").ok();
        let root = std::env::current_dir().expect("cwd");
        let prove = osserva_resa(&root, &url, sel.as_deref(), 2500, 30)
            .await
            .unwrap_or_else(|e| panic!("osservazione non riuscita: {e}"));
        println!(
            "caricata={} elementi={:?} contenitore={:?}",
            prove.pagina_caricata, prove.elementi_resi, prove.contenitore
        );
        for e in &prove.errori_esecuzione {
            println!("  eccezione: {e}");
        }
        for c in prove.errori_console.iter().take(5) {
            println!("  console: {c}");
        }
        let v = classifica_resa(&prove, 5);
        let v = match sel.as_deref() {
            Some(s) => cause_con_selettore(v, s),
            None => v,
        };
        println!("VERDETTO: {v:?}");
    }
}
