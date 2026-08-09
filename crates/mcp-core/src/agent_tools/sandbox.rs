//! Tool sandbox: lettura e scrittura della configurazione sandbox del progetto.
//!
//! Esito nei CAMPI (regola Q): i due tool ritornano [`RispostaTool`]. Prima il
//! solo fallimento previsto — il salvataggio in DB — viaggiava come marker
//! anteposto al testo, e tutto il resto usciva come successo indistinto.
//!
//! # I tre casi che uscivano come «aggiornata» senza esserlo
//!
//! `set_sandbox_config` caricava la configurazione, applicava i campi presenti e
//! annunciava «Configurazione sandbox aggiornata» in ogni caso in cui la scrittura
//! non fallisse. Ma tre chiamate diverse arrivavano a quella stessa frase senza
//! che nulla di cio' che l'agente aveva chiesto fosse entrato in configurazione:
//!
//! - **nessun campo dichiarato**: tutti e quattro sono opzionali nel contratto,
//!   quindi `{}` e' una chiamata valida che salva la configurazione IDENTICA a
//!   come l'ha letta. Dirle «aggiornata» afferma un cambiamento che non c'e';
//! - **`memory_mb` fuori dai valori possibili**, con DUE esiti opposti dallo
//!   stesso campo: un NEGATIVO veniva scartato in silenzio (`as_u64` su un
//!   numero negativo ritorna `None`) e la chiamata usciva «aggiornata» senza
//!   aver toccato la memoria; uno ZERO invece passava (`as_u64` di `0` e'
//!   `Some(0)`) e finiva davvero in configurazione, dove diventa `--memory=0m`
//!   — che non e' un limite. Nessuno dei due era cio' che l'agente aveva
//!   chiesto, e nessuno dei due lo diceva;
//! - **valore non-stringa in `extra_env`**: il ciclo faceva `if let Some(vs) =
//!   v.as_str()`, quindi `{"PORT": 3000}` (numero, non stringa) spariva senza una
//!   riga di risposta. Docker riceve le variabili come `chiave=valore` testuale:
//!   la conversione la deve dichiarare chi chiama, non indovinarla noi.
//!
//! Sono tutti RIMEDIABILI e il messaggio nomina il campo e la correzione: e'
//! l'unica cosa che rende quella natura una promessa mantenuta.
//!
//! # Il quarto caso: la lettura che non era avvenuta
//!
//! Restava fuori perche' non nasce qui. `load_project_sandbox_config`
//! inghiottiva l'errore del DB (`.ok().flatten().flatten()`) e restituiva la
//! configurazione VUOTA sia quando il progetto non aveva override sia quando
//! non aveva potuto leggere. Su `get_sandbox_config` questo dichiarava «memoria:
//! 1024 (default)» come se avesse guardato; su `set_sandbox_config` era peggio,
//! perche' la patch veniva applicata sopra quel vuoto e RISALVATA — un blip
//! della connessione cancellava gli override esistenti, e la risposta era un
//! successo. L'helper ora distingue i due casi (vedi `nexus_tool_kit::sandbox`)
//! e qui il fallimento e' DEL SISTEMA: l'agente non ha nulla da correggere
//! nella propria chiamata.

use super::*;
use crate::sandbox::{
    default_network_mode, load_project_sandbox_config, save_project_sandbox_config,
    ProjectSandboxConfig, DEFAULT_CPUS, DEFAULT_MEMORY_MB,
};
use nexus_agent_tools::{
    input_contract::InputTool,
    tool_inputs::{GetSandboxConfigInput, SetSandboxConfigInput},
};
use nexus_types::tool_outcome::RispostaTool;

/// Il fallimento della LETTURA, per entrambi i tool.
///
/// Natura DEL SISTEMA e non rimediabile: la causa arriva gia' appiattita in una
/// `String` dall'helper, e nessuna delle sue forme (DB muto, JSON in colonna
/// non deserializzabile) dipende da cio' che l'agente ha scritto nella
/// chiamata — riformularla non cambia l'esito. Il messaggio dichiara anche che
/// nulla e' stato modificato, cosi' chi legge non prosegue credendo il
/// contrario.
fn lettura_non_riuscita(e: String) -> RispostaTool {
    RispostaTool::fallito_di_sistema(format!(
        "[Errore: lettura della configurazione sandbox non riuscita: {e}. \
         Nessuna modifica applicata: scrivere sopra una configurazione che non \
         si e' potuta leggere cancellerebbe gli override esistenti.]"
    ))
}

/// La rete in vigore quando il progetto non dichiara nulla, come testo.
///
/// Non e' un letterale `"none"`: il default lo decide
/// [`nexus_tool_kit::sandbox::default_network_mode`], e con la bandiera di
/// rollback attiva e' la rete di Docker. Etichettare comunque `none (default)`
/// avrebbe dichiarato isolamento totale a container che non ce l'hanno.
fn rete_di_default() -> String {
    match default_network_mode() {
        Some(n) => n,
        None => "predefinita di Docker".to_string(),
    }
}

/// Il limite di memoria dichiarato, in MB.
///
/// Separata dal resto perche' il verso della conversione e' il difetto: il campo
/// arriva come `i64` dal contratto e la configurazione lo tiene `u64`, quindi un
/// negativo non ha dove andare. Prima non arrivava affatto — `as_u64` lo
/// scartava — e la chiamata riusciva senza aver impostato niente.
fn memoria_valida(mb: i64) -> Result<u64, RispostaTool> {
    if mb <= 0 {
        return Err(RispostaTool::fallito_rimediabile(format!(
            "[Errore: 'memory_mb' deve essere un intero positivo, ricevuto {mb}. \
             Riprova con un valore in megabyte, es: 512, 1024, 2048, 4096.]"
        )));
    }
    Ok(mb as u64)
}

/// Il limite di CPU dichiarato, in core.
///
/// Docker riceve `--cpus={n}`: zero, negativo, `NaN` o infinito non sono limiti,
/// e il container non partirebbe. Il rifiuto qui nomina il campo, quindi l'agente
/// puo' correggere; lasciarlo passare avrebbe prodotto un fallimento molto piu'
/// a valle, al primo processo lanciato, con un messaggio di Docker.
fn cpu_valide(cpus: f64) -> Result<f64, RispostaTool> {
    if !cpus.is_finite() || cpus <= 0.0 {
        return Err(RispostaTool::fallito_rimediabile(format!(
            "[Errore: 'cpus' deve essere un numero positivo di core, ricevuto {cpus}. \
             Riprova con un valore come 0.5, 1.0, 2.0, 4.0.]"
        )));
    }
    Ok(cpus)
}

/// Le variabili extra dichiarate, tutte quante o nessuna.
///
/// Il rifiuto e' in blocco e nomina TUTTE le chiavi fuori contratto: correggerne
/// una per volta costringerebbe l'agente a tanti giri quanti sono gli errori, e
/// li scoprirebbe uno alla volta. Lo schema del campo dichiara un oggetto, non i
/// tipi dei suoi valori: e' l'handler l'unico che puo' porre questo vincolo.
fn variabili_extra(
    dichiarate: &serde_json::Map<String, Value>,
) -> Result<Vec<(String, String)>, RispostaTool> {
    let mut lette = Vec::new();
    let mut fuori_contratto = Vec::new();
    for (chiave, valore) in dichiarate {
        match valore.as_str() {
            Some(testo) => lette.push((chiave.clone(), testo.to_string())),
            None => fuori_contratto.push(chiave.clone()),
        }
    }
    if !fuori_contratto.is_empty() {
        return Err(RispostaTool::fallito_rimediabile(format!(
            "[Errore: 'extra_env' ammette solo valori stringa; questi non lo sono: {}. \
             Riscrivili fra virgolette, es. {{\"PORT\": \"3000\"}}.]",
            fuori_contratto.join(", ")
        )));
    }
    Ok(lette)
}

/// Applica alla configurazione caricata i soli campi DICHIARATI nella chiamata,
/// e restituisce i loro nomi.
///
/// I nomi non sono decorazione del messaggio: sono la misura che distingue una
/// chiamata che ha cambiato qualcosa da una che ha riscritto la configurazione
/// identica a se stessa, e senza di essi le due sono la stessa `Ok(())`.
///
/// Una patch VUOTA non e' una patch, ed e' qui che viene rifiutata invece che
/// nel chiamante: cosi' l'intera decisione — quali campi sono stati dichiarati,
/// se i loro valori stanno in piedi, se ne resta almeno uno — si prova senza un
/// DB, che e' la sola condizione perche' venga provata.
fn applica_patch(
    cfg: &mut ProjectSandboxConfig,
    params: SetSandboxConfigInput,
) -> Result<Vec<&'static str>, RispostaTool> {
    let mut toccati = Vec::new();
    if let Some(mb) = params.memory_mb {
        cfg.memory_mb = Some(memoria_valida(mb)?);
        toccati.push("memory_mb");
    }
    if let Some(cpus) = params.cpus {
        cfg.cpus = Some(cpu_valide(cpus)?);
        toccati.push("cpus");
    }
    if let Some(rete) = params.network_mode {
        // Il valore canonico viene dall'enum del contratto (regola N): la
        // colonna JSONB tiene una stringa, ma quale stringa lo decide il tipo.
        cfg.network_mode = Some(rete.come_stringa().to_string());
        toccati.push("network_mode");
    }
    if let Some(env) = params.extra_env {
        let lette = variabili_extra(&env)?;
        // Un `extra_env: {}` NON conta come campo impostato, ed e' la stessa
        // regola della chiamata vuota in scala ridotta: le variabili si sommano,
        // quindi un oggetto vuoto lascia la configurazione dov'era e annunciarlo
        // fra i «campi impostati» sarebbe di nuovo dichiarare un cambiamento che
        // non c'e'. Da solo, fa cadere la chiamata nel rifiuto qui sotto.
        if !lette.is_empty() {
            // Le variabili si SOMMANO a quelle gia' configurate: era il
            // comportamento di prima e resta, perche' il tool non ha un modo per
            // dichiarare una rimozione e sostituire in blocco perderebbe dati.
            let mut map = cfg.extra_env.take().unwrap_or_default();
            map.extend(lette);
            cfg.extra_env = Some(map);
            toccati.push("extra_env");
        }
    }
    if toccati.is_empty() {
        return Err(RispostaTool::fallito_rimediabile(
            "[Errore: 'set_sandbox_config' non ha ricevuto nessun campo da impostare. \
             Indica almeno uno fra memory_mb, cpus, network_mode, extra_env; \
             per LEGGERE la configurazione corrente usa get_sandbox_config.]",
        ));
    }
    Ok(toccati)
}

/// Il riepilogo per il modello, composto DAI campi della configurazione salvata.
///
/// I valori non impostati non si ricopiano a mano: memoria e cpu vengono dalle
/// costanti di `nexus_tool_kit::sandbox` e la rete da `rete_di_default`, cioe'
/// dagli stessi valori che il builder Docker usera' davvero.
fn riepilogo(cfg: &ProjectSandboxConfig, toccati: &[&str]) -> String {
    let rete = cfg
        .network_mode
        .clone()
        .unwrap_or_else(rete_di_default);
    let memoria = cfg.memory_mb.unwrap_or(DEFAULT_MEMORY_MB);
    let cpus = cfg.cpus.unwrap_or(DEFAULT_CPUS);
    format!(
        "Configurazione sandbox aggiornata (campi impostati: {}): \
         memoria={memoria}MB, cpu={cpus}, rete={rete}. Attiva dalla prossima esecuzione.",
        toccati.join(", ")
    )
}

pub(super) async fn tool_set_sandbox_config(
    ctx: &AgentToolContext,
    input: &Value,
) -> RispostaTool {
    let params = match SetSandboxConfigInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };

    let mut cfg = match load_project_sandbox_config(&ctx.db, ctx.project_id).await {
        Ok(c) => c,
        Err(e) => return lettura_non_riuscita(e),
    };
    let toccati = match applica_patch(&mut cfg, params) {
        Ok(t) => t,
        Err(risposta) => return risposta,
    };

    match save_project_sandbox_config(&ctx.db, ctx.project_id, &cfg).await {
        Ok(()) => RispostaTool::riuscito(riepilogo(&cfg, &toccati)),
        // L'helper appiattisce l'errore in `String`, quindi qui il tipo del
        // guasto (serializzazione o scrittura sul DB) non e' piu' leggibile:
        // nessuno dei due dipende comunque da cio' che l'agente ha chiesto, e
        // ripetere la stessa chiamata non lo cambia. La configurazione resta
        // quella di prima, e il messaggio lo dice.
        Err(e) => RispostaTool::fallito_di_sistema(format!(
            "[Errore: salvataggio della configurazione sandbox non riuscito: {e}. \
             La configurazione del progetto resta invariata.]"
        )),
    }
}

pub(super) async fn tool_get_sandbox_config(
    ctx: &AgentToolContext,
    input: &Value,
) -> RispostaTool {
    // Il tool non ha parametri, e la lettura del contratto serve proprio a
    // rifiutare cio' che il catalogo non promette invece di eseguire
    // ignorandolo (stessa ragione di `nexus_list_ports`). Un `project_id`
    // scartato in silenzio farebbe credere al modello di aver letto la
    // configurazione di un ALTRO progetto, e riceverebbe questa.
    if let Err(risposta) = GetSandboxConfigInput::leggi(input) {
        return risposta;
    }
    let cfg = match load_project_sandbox_config(&ctx.db, ctx.project_id).await {
        Ok(c) => c,
        Err(e) => return lettura_non_riuscita(e),
    };
    let rete = cfg
        .network_mode
        .clone()
        .unwrap_or_else(|| format!("{} (default)", rete_di_default()));
    let memoria = cfg
        .memory_mb
        .map(|m| m.to_string())
        .unwrap_or_else(|| format!("{DEFAULT_MEMORY_MB} (default)"));
    let cpus = cfg
        .cpus
        .map(|c| c.to_string())
        .unwrap_or_else(|| format!("{DEFAULT_CPUS} (default)"));
    // Mappa presente ma VUOTA e mappa assente dicono la stessa cosa al modello —
    // nessuna variabile — e vanno rese uguali: il ramo `Some` da solo produceva
    // una riga «variabili extra:» seguita dal nulla, che si legge come una
    // risposta troncata.
    let variabili = cfg
        .extra_env
        .as_ref()
        .filter(|e| !e.is_empty())
        .map(|e| {
            e.iter()
                .map(|(k, v)| format!("  {k}={v}"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| "  (nessuna)".to_string());
    // Un progetto senza override e' un progetto che gira coi default, non un
    // errore: la lettura e' RIUSCITA e lo dichiara marcando ogni valore come
    // "(default)". E' lo stesso criterio della directory vuota.
    RispostaTool::riuscito(format!(
        "Configurazione sandbox progetto:\n- memoria: {memoria} MB\n- cpu: {cpus} core\n\
         - rete: {rete}\n- variabili extra:\n{variabili}"
    ))
}

/// Le prove della PATCH: quali campi la chiamata dichiara, se i loro valori
/// stanno in piedi, e che cosa esce quando non ne resta nessuno.
///
/// Partono tutte da `SetSandboxConfigInput::leggi` e non dalla struct costruita
/// a mano (regola O): il contratto e' il produttore, e un campo che serde
/// rifiuta prima di arrivare qui non sarebbe misurato da un test che lo salta.
#[cfg(test)]
mod prove_patch {
    use super::*;
    use nexus_types::tool_outcome::{EsitoTool, NaturaFallimento};
    use serde_json::json;

    fn letti(input: serde_json::Value) -> SetSandboxConfigInput {
        SetSandboxConfigInput::leggi(&input).expect("input conforme al contratto")
    }

    fn rifiutato(input: serde_json::Value) -> RispostaTool {
        let mut cfg = ProjectSandboxConfig::default();
        applica_patch(&mut cfg, letti(input)).expect_err("doveva rifiutare")
    }

    /// MUTAZIONE: togliendo il rifiuto della patch vuota, questa diventa un
    /// `Ok(vec![])` e il tool torna ad annunciare «Configurazione sandbox
    /// aggiornata» per una chiamata che non ha impostato niente.
    #[test]
    fn chiamata_senza_campi_non_e_un_aggiornamento() {
        let risposta = rifiutato(json!({}));
        assert_eq!(risposta.esito, EsitoTool::Fallito);
        assert_eq!(risposta.natura, Some(NaturaFallimento::Rimediabile));
        // Rimediabile obbliga a dire COME: i quattro campi sono l'informazione
        // con cui l'agente corregge.
        for campo in ["memory_mb", "cpus", "network_mode", "extra_env"] {
            assert!(
                risposta.testo.contains(campo),
                "il rifiuto non nomina '{campo}': {}",
                risposta.testo
            );
        }
    }

    /// I due valori che `as_u64` trattava in modi OPPOSTI — il negativo
    /// scartato in silenzio, lo zero accettato e scritto come `--memory=0m` —
    /// ora sono lo stesso rifiuto dichiarato.
    #[test]
    fn memoria_non_positiva_rifiutata() {
        for valore in [json!(-1), json!(0)] {
            let risposta = rifiutato(json!({ "memory_mb": valore }));
            assert_eq!(risposta.natura, Some(NaturaFallimento::Rimediabile));
            assert!(
                risposta.testo.contains("memory_mb"),
                "il rifiuto non nomina il campo: {}",
                risposta.testo
            );
        }
    }

    /// `--cpus=0` non e' un limite: il container non parte, e senza questo
    /// rifiuto il guasto arrivava al primo processo lanciato, con un messaggio
    /// di Docker che non nomina il tool che aveva scritto il valore.
    #[test]
    fn cpu_non_positive_rifiutate() {
        for valore in [json!(0), json!(-2.5)] {
            let risposta = rifiutato(json!({ "cpus": valore }));
            assert_eq!(risposta.natura, Some(NaturaFallimento::Rimediabile));
            assert!(risposta.testo.contains("cpus"), "{}", risposta.testo);
        }
    }

    /// Il rifiuto e' in BLOCCO: entrambe le chiavi fuori contratto sono nel
    /// messaggio, o l'agente le scoprirebbe una per giro.
    #[test]
    fn extra_env_non_stringa_rifiutata_nominando_tutte_le_chiavi() {
        let risposta = rifiutato(json!({ "extra_env": { "PORT": 3000, "DEBUG": true } }));
        assert_eq!(risposta.natura, Some(NaturaFallimento::Rimediabile));
        assert!(risposta.testo.contains("PORT"), "{}", risposta.testo);
        assert!(risposta.testo.contains("DEBUG"), "{}", risposta.testo);
    }

    /// Un `extra_env: {}` non cambia niente (le variabili si sommano), quindi
    /// non e' un campo impostato: da solo cade nello stesso rifiuto della
    /// chiamata vuota.
    #[test]
    fn extra_env_vuoto_non_e_un_campo_impostato() {
        let risposta = rifiutato(json!({ "extra_env": {} }));
        assert_eq!(risposta.natura, Some(NaturaFallimento::Rimediabile));
    }

    /// MUTAZIONE: se `toccati` tornasse a essere ignorato, il riepilogo di un
    /// aggiornamento PARZIALE si rileggerebbe come se avesse fissato anche
    /// memoria e rete — che qui restano quelle di default.
    #[test]
    fn il_riepilogo_nomina_i_soli_campi_impostati() {
        let mut cfg = ProjectSandboxConfig::default();
        let toccati = applica_patch(&mut cfg, letti(json!({ "cpus": 4.0 }))).expect("patch valida");
        assert_eq!(toccati, vec!["cpus"]);

        let testo = riepilogo(&cfg, &toccati);
        assert!(testo.contains("campi impostati: cpus"), "{testo}");
        assert!(!testo.contains("memory_mb"), "{testo}");
        assert!(!testo.contains("network_mode"), "{testo}");
    }

    /// Le variabili si SOMMANO a quelle gia' configurate: sostituire in blocco
    /// cancellerebbe quelle che l'agente non ha nominato.
    #[test]
    fn extra_env_si_somma_a_quelle_esistenti() {
        let mut cfg = ProjectSandboxConfig {
            extra_env: Some([("NODE_ENV".to_string(), "development".to_string())].into()),
            ..Default::default()
        };
        let toccati = applica_patch(&mut cfg, letti(json!({ "extra_env": { "PORT": "3000" } })))
            .expect("patch valida");
        assert_eq!(toccati, vec!["extra_env"]);

        let env = cfg.extra_env.expect("variabili presenti");
        assert_eq!(env.get("NODE_ENV").map(String::as_str), Some("development"));
        assert_eq!(env.get("PORT").map(String::as_str), Some("3000"));
    }

    /// Il valore scritto in colonna viene dall'ENUM del contratto (regola N), e
    /// cio' che il vocabolario non contiene non arriva nemmeno all'handler.
    #[test]
    fn network_mode_solo_dal_vocabolario() {
        let mut cfg = ProjectSandboxConfig::default();
        applica_patch(&mut cfg, letti(json!({ "network_mode": "bridge" }))).expect("patch valida");
        assert_eq!(cfg.network_mode.as_deref(), Some("bridge"));

        let fuori = SetSandboxConfigInput::leggi(&json!({ "network_mode": "nat" }))
            .expect_err("'nat' non e' nel vocabolario");
        assert_eq!(fuori.natura, Some(NaturaFallimento::Rimediabile));
    }

    /// `get_sandbox_config` non ha parametri, e il contratto e' cio' che rende
    /// il silenzio impossibile: un `project_id` accettato e ignorato farebbe
    /// credere al modello di aver letto la configurazione di un altro progetto.
    #[test]
    fn get_rifiuta_i_campi_che_il_catalogo_non_promette() {
        let risposta = GetSandboxConfigInput::leggi(&json!({ "project_id": "altro" }))
            .expect_err("campo non promesso");
        assert_eq!(risposta.esito, EsitoTool::Fallito);
        assert_eq!(risposta.natura, Some(NaturaFallimento::Rimediabile));

        GetSandboxConfigInput::leggi(&json!({})).expect("la chiamata senza campi e' quella giusta");
    }
}
