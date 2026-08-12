//! Il CONFINE col browser reale: carica una pagina in Chromium headless e
//! riporta i fatti osservati (richieste di rete, errori di console ed
//! esecuzione, DOM reso). Non giudica: i criteri sono i punti unici puri
//! [`nexus_agent_graph::decisions::browser_dialogue`] («la pagina ottiene i
//! propri dati?»), [`nexus_agent_graph::decisions::static_render`] («la pagina
//! mostra il proprio contenuto?») e
//! [`nexus_agent_graph::decisions::risorse_pagina`] («cio' che referenzia e'
//! arrivato?»), che ricevono questi fatti.
//!
//! Le tre domande condividono il confine e non lo script: uno solo, una sola
//! esecuzione, piu' interpreti sui campi che a ciascuna competono. Due script
//! divergerebbero, e la divergenza si vedrebbe come due criteri che misurano
//! cose leggermente diverse senza che nessuno sappia dire quali (regola L). La
//! terza domanda e' arrivata dopo le altre due e non ha aggiunto un'apertura di
//! pagina: ha aggiunto due campi al payload (il TIPO su ogni richiesta e l'URL
//! finale della pagina), che e' la prova che il taglio del confine reggeva.
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
use nexus_agent_graph::decisions::risorse_pagina::{
    classifica_elemento, ElementoPortante, RisorsaOsservata,
};
use nexus_agent_graph::decisions::static_render::{EccezionePagina, EsitoContenitore, ProveResa};

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
    /// Il tipo di risorsa dichiarato dal browser, su ogni richiesta.
    ///
    /// Si chiama `resourceKind` e non `type` per una ragione che riguarda il
    /// GUARD, non il gusto: il test che confronta i campi con lo script cerca il
    /// nome come sottostringa, e `type` comparirebbe comunque (in `m.type()`)
    /// anche dopo averlo tolto dal payload — cioe' il guard resterebbe verde su
    /// un contratto rotto, che e' esattamente il difetto che quel test esiste
    /// per impedire.
    pub const TIPO_RISORSA: &str = "resourceKind";
    /// L'URL su cui la pagina si e' fermata, per attribuire la provenienza.
    pub const URL_PAGINA: &str = "pageUrl";
    /// Gli ELEMENTI che portano una risorsa, con l'esito della loro RESA.
    ///
    /// Canale distinto da [`RICHIESTE`] e non un suo doppione: la rete risponde
    /// «e' arrivato?», l'elemento «si e' visto?». Per gli URL `data:` Chromium
    /// non emette alcun evento di rete — MISURATO il 10/08/2026 eseguendo
    /// questo stesso script sulla pagina di vetrina-statica: `requests: []` su
    /// sei immagini rotte — quindi senza questo canale la sola risposta
    /// possibile e' «la pagina non referenzia nulla».
    pub const ELEMENTI_RISORSA: &str = "resourceElements";
    /// I tipi per cui il canale degli elementi ha effettivamente guardato.
    ///
    /// Serve a distinguere «zero elementi di questo tipo nella pagina» da «non
    /// ho guardato questo tipo»: il primo giustifica un verdetto, il secondo
    /// no (regola Q). Nome scelto perche' non sia sottostringa di
    /// [`TIPO_RISORSA`] ne' di [`ELEMENTI`], o il guard resterebbe verde su un
    /// contratto rotto.
    pub const TIPI_CON_ELEMENTO: &str = "elementKinds";

    /// Tutti, per il test che li confronta con lo script.
    pub const TUTTI: [&str; 10] = [
        RICHIESTE,
        ERRORI_CONSOLE,
        ERRORI_PAGINA,
        CARICATA,
        ELEMENTI,
        CONTENITORE,
        TIPO_RISORSA,
        URL_PAGINA,
        ELEMENTI_RISORSA,
        TIPI_CON_ELEMENTO,
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
        .map(|a| a.iter().filter_map(richiesta_da).collect())
        .unwrap_or_default();
    // Il dialogo non distingue un'eccezione da un avviso: per la sua domanda
    // sono entrambi contorno dell'evidenza, e li teneva gia' in un'unica lista.
    // La distinzione la fa `interpreta_resa`, dove FA la differenza.
    let mut errori_console = lista(&v, campo::ERRORI_CONSOLE);
    // Le eccezioni viaggiano come OGGETTI (vedi `eccezioni`): qui si appiattiscono
    // al testo perche' il dialogo non ha dove metterne la posizione e non ne fa
    // un verdetto. Leggerle con `lista` — che tiene solo `as_str()` — le farebbe
    // sparire IN SILENZIO da questa evidenza, ed e' il difetto che il tipo nuovo
    // rischiava di introdurre proprio nel lettore che non lo chiedeva.
    errori_console.extend(eccezioni(&v, campo::ERRORI_PAGINA).iter().map(EccezionePagina::testo));
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
    // Assente = non riportate. NON una lista vuota: quella direbbe «la pagina
    // non ha chiesto nulla», e il criterio delle risorse tratta i due casi in
    // modo opposto (uno e' una pagina autosufficiente, l'altro un'osservazione
    // che non ha guardato).
    let risorse = v
        .get(campo::RICHIESTE)
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(risorsa_da).collect());
    // Stessa disciplina: assente = il canale degli elementi non ha guardato.
    // Il campo si legge SOLO se lo script ha dichiarato per quali tipi ha
    // guardato: una lista di elementi senza la dichiarazione di copertura
    // direbbe «questi sono tutti», che e' un'affermazione che quello script
    // non e' in grado di fare.
    let elementi = v
        .get(campo::TIPI_CON_ELEMENTO)
        .and_then(|t| t.as_array())
        .and(v.get(campo::ELEMENTI_RISORSA))
        .and_then(|e| e.as_array())
        .map(|a| a.iter().filter_map(elemento_da).collect());
    Ok(ProveResa {
        pagina_caricata: caricata(&v),
        elementi_resi,
        contenitore,
        errori_esecuzione: eccezioni(&v, campo::ERRORI_PAGINA),
        errori_console: lista(&v, campo::ERRORI_CONSOLE),
        risorse,
        elementi,
        origine: v
            .get(campo::URL_PAGINA)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    })
}

/// UNA richiesta del payload. Punto unico della lettura per i due interpreti:
/// se il dialogo e le risorse la ricostruissero ognuno per conto proprio, un
/// campo rinominato nello script resterebbe letto correttamente da uno dei due
/// e in silenzio dall'altro.
fn richiesta_da(r: &serde_json::Value) -> Option<RichiestaOsservata> {
    Some(RichiestaOsservata {
        url: r.get("url")?.as_str()?.to_string(),
        status: r.get("status").and_then(|s| s.as_u64()).map(|s| s as u16),
        errore: r
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

/// La stessa richiesta, piu' il TIPO che il browser le ha attribuito. Assente
/// = non dichiarato, mai una stringa vuota: il criterio distingue «di che tipo
/// e' questa risorsa non lo so» da «e' di un tipo che non governo».
fn risorsa_da(r: &serde_json::Value) -> Option<RisorsaOsservata> {
    Some(RisorsaOsservata {
        richiesta: richiesta_da(r)?,
        tipo: r
            .get(campo::TIPO_RISORSA)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string),
    })
}

/// I nomi dei campi INTERNI di un elemento portante: stesso contratto fra due
/// linguaggi dei campi esterni, e nessun compilatore lo controlla.
mod campo_elemento {
    pub const TIPO: &str = "kind";
    pub const URL: &str = "url";
    pub const DICHIARA: &str = "declared";
    pub const CONCLUSO: &str = "settled";
    pub const UTILIZZABILE: &str = "usable";

    /// Tutti, per il test che li confronta con lo script.
    pub const TUTTI: [&str; 5] = [TIPO, URL, DICHIARA, CONCLUSO, UTILIZZABILE];
}

/// Un elemento portante dal payload.
///
/// Il verdetto NON arriva dallo script: arriva dal punto unico
/// [`nexus_agent_graph::decisions::classifica_elemento`], che lo deriva dai tre
/// fatti grezzi. Se lo classificasse il JavaScript, il criterio vivrebbe in due
/// posti e uno dei due non sarebbe testabile.
fn elemento_da(e: &serde_json::Value) -> Option<ElementoPortante> {
    let tipo = e
        .get(campo_elemento::TIPO)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())?;
    let flag = |k: &str| {
        e.get(k)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    Some(ElementoPortante {
        tipo: tipo.to_string(),
        url: e
            .get(campo_elemento::URL)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        resa: classifica_elemento(
            flag(campo_elemento::DICHIARA),
            flag(campo_elemento::CONCLUSO),
            flag(campo_elemento::UTILIZZABILE),
        ),
    })
}

/// Le eccezioni del payload, coi campi che lo script ha dichiarato.
///
/// Tollera la forma STRINGA per una ragione che non e' retrocompatibilita' (lo
/// script e l'interprete viaggiano nello stesso binario): un payload prodotto a
/// mano da una fixture, o da una versione futura dello script che tornasse a
/// mandare testo, non deve far sparire l'eccezione dall'evidenza — sparirebbe
/// il VERDETTO, non un dettaglio. Cio' che manca resta `None` e lo dichiara.
fn eccezioni(v: &serde_json::Value, campo: &str) -> Vec<EccezionePagina> {
    let numero = |o: &serde_json::Value, k: &str| -> Option<u32> {
        o.get(k)
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
    };
    let testo = |o: &serde_json::Value, k: &str| -> Option<String> {
        o.get(k)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    v.get(campo)
        .and_then(|c| c.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    // Forma stringa: il messaggio e nient'altro.
                    if let Some(s) = e.as_str() {
                        return Some(EccezionePagina {
                            messaggio: s.to_string(),
                            ..Default::default()
                        });
                    }
                    // Forma oggetto: il messaggio e' l'unico campo obbligatorio.
                    // Senza, la voce non e' un'eccezione e si scarta — e' il caso
                    // delle risorse fallite, che il canale in-page filtra gia'
                    // alla fonte ma che nessuno garantisce a questo lato.
                    let messaggio = testo(e, "message")?;
                    Some(EccezionePagina {
                        messaggio,
                        classe: testo(e, "name"),
                        file: testo(e, "file"),
                        riga: numero(e, "line"),
                        colonna: numero(e, "column"),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
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
  // I tagli hanno un nome: entrano in campi che viaggiano fino all'evidenza del
  // gate, e un numero ripetuto in cinque punti diverge al primo che lo ritocca.
  const CAP_TESTO = 500;
  const CAP_PERCORSO = 300;
  const CAP_CLASSE = 60;
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
    // DOPO le redirezioni: e' l'origine rispetto a cui una risorsa e' locale o
    // di terzi, e prenderla dall'URL richiesto direbbe la cosa sbagliata su una
    // pagina che e' stata rediretta altrove.
    try {{ fatti.pageUrl = page.url(); }} catch (_) {{}}
{posizioni}
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
        posizioni = POSIZIONI_ECCEZIONI,
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
    await page.addInitScript(() => {
      // Questa funzione viene serializzata ed eseguita DENTRO LA PAGINA: le
      // costanti del processo Node qui non esistono, e usarle produce un
      // `ReferenceError` che il criterio riporterebbe come difetto della pagina
      // osservata — cioe' la misura inventerebbe un guasto suo. Misurato in
      // esercizio il 12/08/2026, subito dopo averle estratte.
      const CAP_TESTO = 500;
      const CAP_PERCORSO = 300;
      const CAP_CLASSE = 60;
      window.__nexusEcc = [];
      window.addEventListener('error', (ev) => {
        // Le RISORSE fallite (una <img> 404) usano lo STESSO evento, e senza
        // `message`: MISURATO il 12/08/2026, due voci vuote per due immagini
        // rotte. Senza questo filtro un'immagine mancante diventerebbe
        // «esecuzione interrotta», cioe' il criterio della resa invaderebbe
        // quello delle risorse e boccerebbe per la causa sbagliata.
        if (!ev || !ev.message) return;
        window.__nexusEcc.push({
          message: String(ev.message).slice(0, CAP_TESTO),
          name: (ev.error && ev.error.name) ? String(ev.error.name).slice(0, CAP_CLASSE) : null,
          file: ev.filename ? String(ev.filename).slice(0, CAP_PERCORSO) : null,
          line: Number.isFinite(ev.lineno) ? ev.lineno : null,
          column: Number.isFinite(ev.colno) ? ev.colno : null,
        });
      }, true);
    });
    page.on('console', (m) => {
      if (m.type() === 'error') fatti.consoleErrors.push(String(m.text()).slice(0, CAP_TESTO));
    });
    page.on('pageerror', (e) => {
      fatti.pageErrors.push({
        message: String(e && e.message ? e.message : e).slice(0, CAP_TESTO),
        name: (e && e.name) ? String(e.name).slice(0, CAP_CLASSE) : null,
        file: null, line: null, column: null,
      });
    });
    page.on('requestfailed', (r) => {
      if (r.resourceType() === 'document') return;
      const f = r.failure();
      fatti.requests.push({ url: r.url(), resourceKind: r.resourceType(), error: (f && f.errorText) ? f.errorText : 'richiesta fallita' });
    });
    page.on('response', (r) => {
      if (r.request().resourceType() === 'document') return;
      fatti.requests.push({ url: r.url(), resourceKind: r.request().resourceType(), status: r.status() });
    });"#;

/// Attribuisce a ogni eccezione la sua POSIZIONE, letta dal canale in-page.
///
/// Non e' un secondo canale sullo stesso fatto: e' l'unico che quel fatto lo
/// porta. MISURATO il 12/08/2026 riproducendo il difetto reale (`const products
/// = [ @@ROTTO@@` a riga 75): `pageerror` consegna `message` «Invalid or
/// unexpected token» e `stack` VUOTO, perche' per un errore di PARSING V8 non
/// emette call frame e `exceptionToError` di Playwright compone lo stack dai
/// soli call frame, scartando `exceptionDetails.lineNumber/columnNumber/url`
/// che il CDP pure porta. Il listener `error` in fase di cattura, sulla stessa
/// pagina, dava `listino.html:75:20`.
///
/// La correlazione e' per MESSAGGIO e consuma la voce usata: due eccezioni con
/// lo stesso testo restano due, e la seconda non eredita la posizione della
/// prima. Un'eccezione vista dal solo canale in-page non si perde — viene
/// aggiunta — perche' il verdetto della resa non deve dipendere da quale dei
/// due canali l'ha vista.
///
/// Gira DOPO l'attesa: uno script che lancia a fine caricamento non sarebbe
/// ancora nell'array al momento del `goto`.
const POSIZIONI_ECCEZIONI: &str = r#"
    try {
      const ecc = await page.evaluate(() => window.__nexusEcc || []);
      for (const e of fatti.pageErrors) {
        const i = ecc.findIndex((x) => x && !x.__usata && x.message === e.message);
        if (i >= 0) {
          ecc[i].__usata = true;
          e.file = ecc[i].file; e.line = ecc[i].line; e.column = ecc[i].column;
          if (!e.name) e.name = ecc[i].name;
        }
      }
      for (const x of ecc) {
        if (x && !x.__usata) fatti.pageErrors.push({ message: x.message, name: x.name, file: x.file, line: x.line, column: x.column });
      }
    } catch (_) {}"#;

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
      try {
        fatti.elementKinds = ['image', 'media'];
        fatti.resourceElements = await page.evaluate(() => {
          const diRete = (u) => /^https?:/i.test(u);
          const resa = (u) => (diRete(u) ? u : String(u || '').slice(0, 120));
          const img = Array.from(document.images).map((i) => {
            const dichiara = !!(i.getAttribute('src') || i.getAttribute('srcset')
              || (i.parentElement && i.parentElement.tagName === 'PICTURE'));
            return { kind: 'image', url: resa(i.currentSrc), declared: dichiara,
                     settled: !!i.complete, usable: i.naturalWidth > 0 };
          });
          const media = Array.from(document.querySelectorAll('video,audio')).map((m) => {
            const dichiara = !!(m.getAttribute('src') || m.querySelector('source'));
            return { kind: 'media', url: resa(m.currentSrc), declared: dichiara,
                     settled: !!(m.error || m.readyState >= 1),
                     usable: !m.error && m.readyState >= 1 };
          });
          return img.concat(media);
        });
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
    use nexus_agent_graph::decisions::risorse_pagina::PoliticaRisorse;

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

    /// La POSIZIONE dell'eccezione attraversa il wire e arriva ai fatti della
    /// resa. E' il ponte fra i due lati del confine: il payload qui e' nella
    /// forma che lo script produce dopo la fusione dei due canali.
    ///
    /// MUTAZIONE: far leggere `errori_esecuzione` con `lista` (la forma
    /// precedente) -> gli oggetti non sono stringhe, la lista esce VUOTA e il
    /// criterio perde il proprio verdetto, non solo la posizione.
    #[test]
    fn la_posizione_dell_eccezione_arriva_ai_fatti_della_resa() {
        let payload = r#"{"loaded":true,"elementCount":3,
            "pageErrors":[{"message":"Invalid or unexpected token","name":"SyntaxError",
                           "file":"http://127.0.0.1:4000/preview/p/listino.html","line":75,"column":20}]}"#;
        let prove = interpreta_resa(payload).expect("leggibile");
        let ecc = prove
            .errori_esecuzione
            .first()
            .expect("l'eccezione non deve sparire");
        assert_eq!(ecc.riga, Some(75));
        assert_eq!(ecc.colonna, Some(20));
        assert_eq!(ecc.classe.as_deref(), Some("SyntaxError"));
        // Il percorso completo non aiuta chi deve aprire il file.
        assert_eq!(ecc.posizione().as_deref(), Some("listino.html:75:20"));
        assert!(
            ecc.descrizione().contains("listino.html:75:20"),
            "la riga che l'agente legge deve portare la posizione: {}",
            ecc.descrizione()
        );
    }

    /// L'ALTRO lettore dello stesso campo. Il dialogo browser non ha dove
    /// mettere una posizione e non fa dell'eccezione un verdetto, ma l'eccezione
    /// deve restare nella sua evidenza.
    ///
    /// E' la trappola del cambiamento: `pageErrors` ha DUE consumatori, e
    /// tipizzare il campo pensando al solo criterio della resa avrebbe fatto
    /// sparire le eccezioni di qui IN SILENZIO — nessun tipo se ne sarebbe
    /// accorto, perche' `lista` su oggetti ritorna semplicemente vuoto.
    ///
    /// MUTAZIONE: riportare la riga 261 a `lista(&v, campo::ERRORI_PAGINA)` ->
    /// questo test cade con l'evidenza del dialogo priva dell'eccezione.
    #[test]
    fn il_dialogo_non_perde_l_eccezione_quando_diventa_un_oggetto() {
        let payload = r#"{"loaded":true,"requests":[],"consoleErrors":["avviso di libreria"],
            "pageErrors":[{"message":"courses is not defined","name":"ReferenceError","line":12}]}"#;
        let prove = interpreta(payload).expect("leggibile");
        assert!(
            prove
                .errori_console
                .iter()
                .any(|e| e.contains("courses is not defined")),
            "l'eccezione deve restare nell'evidenza del dialogo: {:?}",
            prove.errori_console
        );
        assert!(
            prove
                .errori_console
                .iter()
                .any(|e| e.contains("ReferenceError")),
            "la classe entra nel testo quando il messaggio non la porta gia'"
        );
        assert!(prove.errori_console.iter().any(|e| e == "avviso di libreria"));
    }

    /// La forma STRINGA resta leggibile. Non e' retrocompatibilita' di wire (lo
    /// script e l'interprete stanno nello stesso binario): e' che una fixture o
    /// una versione futura che mandasse testo non deve far sparire il VERDETTO.
    #[test]
    fn un_eccezione_come_stringa_resta_un_eccezione() {
        let prove = interpreta_resa(r#"{"loaded":true,"elementCount":1,"pageErrors":["boom"]}"#)
            .expect("leggibile");
        let ecc = prove.errori_esecuzione.first().expect("presente");
        assert_eq!(ecc.messaggio, "boom");
        assert_eq!(ecc.posizione(), None, "senza posizione non si inventa nulla");
    }

    /// Lo script apre il canale che porta la posizione, PRIMA della navigazione
    /// (un `addInitScript` dopo il `goto` non verrebbe mai eseguito), e filtra
    /// le voci senza `message`.
    ///
    /// Il filtro non e' prudenza teorica: MISURATO il 12/08/2026, una `<img>`
    /// 404 emette sullo stesso canale una voce priva di messaggio, e senza il
    /// filtro un'immagine rotta diventerebbe «esecuzione interrotta» — il
    /// criterio della resa boccerebbe per la causa di un altro criterio.
    #[test]
    fn lo_script_apre_il_canale_della_posizione_e_scarta_le_risorse() {
        let s = script_osservazione("/opt/chrome", "http://x", 2000, 30000, None);
        let pos_init = s.find("addInitScript").expect("canale della posizione");
        let pos_goto = s.find("page.goto").expect("navigazione");
        assert!(
            pos_init < pos_goto,
            "addInitScript dopo il goto non verrebbe eseguito"
        );
        assert!(s.contains("if (!ev || !ev.message) return;"), "filtro sulle risorse fallite");
        // Cio' che gira DENTRO la pagina non vede le costanti del processo Node.
        // MISURATO in esercizio il 12/08/2026: `ReferenceError: CAP_TESTO is not
        // defined` riportato dal criterio come difetto della pagina osservata —
        // la misura si era inventata un guasto suo. Il test guarda la porzione
        // di script che il browser riceve, non l'intero file.
        let init = s
            .split("addInitScript")
            .nth(1)
            .and_then(|d| d.split("page.on('console'").next())
            .expect("corpo dell'init script");
        for c in ["CAP_TESTO", "CAP_PERCORSO", "CAP_CLASSE"] {
            if init.contains(c) {
                assert!(
                    init.contains(&format!("const {c} =")),
                    "{c} e' usata nella pagina ma dichiarata solo in Node: \
                     produrrebbe un ReferenceError attribuito al progetto osservato"
                );
            }
        }
        assert!(s.contains("ev.lineno"), "la riga entra nel payload");
        assert!(s.contains("ev.colno"), "la colonna entra nel payload");
        // La fusione dei due canali viene DOPO l'attesa: un'eccezione lanciata a
        // fine caricamento non sarebbe ancora nell'array.
        let pos_attesa = s.find("waitForLoadState").expect("attesa");
        let pos_fusione = s.find("__nexusEcc || []").expect("fusione");
        assert!(pos_attesa < pos_fusione, "la fusione segue l'attesa");
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
        // Anche i campi INTERNI di un elemento portante: verificare i soli nomi
        // esterni lascerebbe verde un rinominamento di `usable`, cioe' proprio
        // il campo da cui dipende se un'immagine risulta resa o rotta.
        for nome in campo_elemento::TUTTI {
            assert!(
                s.contains(nome),
                "il campo `{nome}` di un elemento portante e' letto da \
                 `elemento_da` ma lo script non lo scrive"
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
            classifica_resa(&p, 5, &PoliticaRisorse::default()),
            VerdettoResa::NonResa { .. }
        ));
    }

    /// IL CASO REALE del 09/08/2026, e il test attraversa il PRODUTTORE: il
    /// payload ha la forma che lo script emette (i suoi campi sono gli stessi
    /// che `lo_script_e_gli_interpreti_usano_gli_stessi_campi` verifica contro
    /// il JavaScript), e da li' i fatti arrivano al criterio per la strada della
    /// produzione — nessuna lista di risorse fabbricata a mano.
    ///
    /// Sei card, sei `<img>` verso `via.placeholder.com` irraggiungibile, DOM
    /// pieno, contenitore popolato, nessuna eccezione: e' la pagina che il gate
    /// ha approvato.
    ///
    /// MUTAZIONE: togliere `resourceKind` dallo script (o dal payload) -> il
    /// tipo non arriva, il verdetto delle risorse diventa `NonOsservabile` e la
    /// pagina torna `Resa`. E' la stessa cosa che accade togliendo il ramo delle
    /// risorse dal criterio: il difetto reale ricompare in entrambi i casi.
    #[test]
    fn le_immagini_rotte_arrivano_al_criterio_dal_produttore() {
        use nexus_agent_graph::decisions::risorse_pagina::VerdettoRisorse;
        use nexus_agent_graph::decisions::static_render::{
            classifica_resa, risorse_della_pagina, CausaNonResa, VerdettoResa,
        };
        let immagini: Vec<String> = (1..=6)
            .map(|n| {
                format!(
                    r#"{{"url":"https://via.placeholder.com/300x200?text=Prodotto+{n}",
                        "resourceKind":"image","error":"net::ERR_NAME_NOT_RESOLVED"}}"#
                )
            })
            .collect();
        let payload = format!(
            r#"{{"loaded":true,"elementCount":118,
              "pageUrl":"http://127.0.0.1:4000/preview/e4d446ce/index.html",
              "container":{{"found":true,"children":6}},
              "consoleErrors":[],"pageErrors":[],
              "requests":[{},
                {{"url":"http://127.0.0.1:4000/preview/e4d446ce/style.css",
                 "resourceKind":"stylesheet","status":200}}]}}"#,
            immagini.join(",")
        );

        let prove = interpreta_resa(&payload).expect("fatti leggibili");
        assert_eq!(
            prove.risorse.as_ref().map(Vec::len),
            Some(7),
            "sei immagini piu' il foglio di stile"
        );
        assert_eq!(
            prove.origine.as_deref(),
            Some("http://127.0.0.1:4000/preview/e4d446ce/index.html")
        );

        let politica = PoliticaRisorse::nuova(
            vec!["image".into(), "stylesheet".into(), "script".into(), "media".into()],
            Some(1.0),
        );
        assert!(risorse_della_pagina(&prove, &politica).e_bloccante());

        let VerdettoResa::NonResa { cause } = classifica_resa(&prove, 5, &politica) else {
            panic!("una pagina le cui immagini non arrivano non mostra il proprio contenuto");
        };
        assert!(matches!(
            cause.as_slice(),
            [CausaNonResa::RisorseNonCaricate { .. }]
        ));

        // LA MUTAZIONE, esercitata: senza il tipo dichiarato dallo script il
        // criterio non risponde e la pagina ripassa — il verde del 09/08.
        let senza_tipo = payload.replace("\"resourceKind\":", "\"ignorato\":");
        let cieche = interpreta_resa(&senza_tipo).expect("fatti leggibili");
        assert!(matches!(
            risorse_della_pagina(&cieche, &politica),
            VerdettoRisorse::NonOsservabile { .. }
        ));
        assert_eq!(
            classifica_resa(&cieche, 5, &politica),
            VerdettoResa::Resa { elementi: 118 }
        );
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
        assert_eq!(
            p.risorse, None,
            "richieste non riportate: non e' una pagina senza risorse"
        );
        assert_eq!(p.origine, None);
        // Le richieste riportate e VUOTE sono un fatto diverso, e resta tale.
        let muta = interpreta_resa(r#"{"loaded":true,"requests":[]}"#).expect("leggibile");
        assert_eq!(muta.risorse, Some(Vec::new()));
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

    /// IL CASO MISURATO il 10/08/2026, e il payload ha la forma che lo script
    /// EMETTE DAVVERO su quella pagina: `requests` VUOTO — Chromium non emette
    /// eventi di rete per gli URL incorporati — e sei elementi con la sorgente
    /// dichiarata, il caricamento concluso e il contenuto inutilizzabile.
    ///
    /// Il test attraversa il produttore (`interpreta_resa`) e arriva al
    /// VERDETTO, non a una stringa: prima del canale degli elementi la stessa
    /// pagina usciva `NessunaDichiarata`, cioe' «non referenzia risorse di
    /// alcun tipo governato», e il gate la dichiarava resa.
    ///
    /// MUTAZIONE: togliere `resourceElements` dal payload (o `elementKinds`,
    /// che ne autorizza la lettura) -> `prove.elementi` torna `None`, il
    /// verdetto torna `NessunaDichiarata` e la pagina torna `Resa`. E' lo
    /// stesso rosso che si ottiene togliendo il ramo degli elementi dal
    /// criterio: il difetto reale ricompare in entrambi i casi.
    #[test]
    fn le_immagini_incorporate_rotte_arrivano_al_criterio_dal_produttore() {
        use nexus_agent_graph::decisions::risorse_pagina::{PoliticaRisorse, VerdettoRisorse};
        use nexus_agent_graph::decisions::static_render::{
            classifica_resa, risorse_della_pagina, CausaNonResa, VerdettoResa,
        };
        let elementi: Vec<String> = (0..6)
            .map(|_| {
                r#"{"kind":"image","url":"data:image/svg+xml;utf8,<svg xmlns=",
                    "declared":true,"settled":true,"usable":false}"#
                    .to_string()
            })
            .collect();
        let payload = format!(
            r#"{{"loaded":true,"elementCount":38,
              "pageUrl":"http://127.0.0.1:4000/preview/76d9b79e/index.html",
              "container":{{"found":true,"children":6}},
              "consoleErrors":[],"pageErrors":[],
              "requests":[],
              "elementKinds":["image","media"],
              "resourceElements":[{}]}}"#,
            elementi.join(",")
        );

        let prove = interpreta_resa(&payload).expect("fatti leggibili");
        assert_eq!(
            prove.risorse.as_ref().map(Vec::len),
            Some(0),
            "il canale di rete ha guardato e non ha visto nulla: e' un fatto, non un'assenza"
        );
        assert_eq!(prove.elementi.as_ref().map(Vec::len), Some(6));

        let politica = PoliticaRisorse::nuova(
            vec![
                "image".into(),
                "stylesheet".into(),
                "script".into(),
                "media".into(),
            ],
            Some(1.0),
        );
        let VerdettoRisorse::TipiCompromessi { tipi, .. } =
            risorse_della_pagina(&prove, &politica)
        else {
            panic!("sei immagini su sei che non rendono compromettono il tipo");
        };
        assert_eq!((tipi[0].falliti, tipi[0].osservati), (6, 6));
        assert_eq!(tipi[0].incorporate, 6);

        let VerdettoResa::NonResa { cause } = classifica_resa(&prove, 5, &politica) else {
            panic!("una pagina le cui immagini non si vedono non mostra il proprio contenuto");
        };
        assert!(
            cause
                .iter()
                .any(|c| matches!(c, CausaNonResa::RisorseNonCaricate { .. })),
            "il DOM ha 38 elementi e il contenitore ha sei figli: senza il canale \
             degli elementi nessuna causa nascerebbe"
        );
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
        use nexus_agent_graph::decisions::risorse_pagina::PoliticaRisorse;
        use nexus_agent_graph::decisions::static_render::{
            cause_con_selettore, classifica_resa, risorse_della_pagina,
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
            println!("  eccezione: {}", e.descrizione());
        }
        for c in prove.errori_console.iter().take(5) {
            println!("  console: {c}");
        }
        // La politica com'e' scritta dalla mig 0692: la prova dal vivo deve
        // usare la stessa configurazione dell'esercizio, o misurerebbe un
        // criterio che nessuno esegue.
        let politica = PoliticaRisorse::nuova(
            vec!["image".into(), "stylesheet".into(), "script".into(), "media".into()],
            Some(1.0),
        );
        println!("RISORSE: {:?}", risorse_della_pagina(&prove, &politica));
        let v = classifica_resa(&prove, 5, &politica);
        let v = match sel.as_deref() {
            Some(s) => cause_con_selettore(v, s),
            None => v,
        };
        println!("VERDETTO: {v:?}");
    }
}
