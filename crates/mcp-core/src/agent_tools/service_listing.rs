//! Elenco dei servizi di un progetto: i CAMPI (regola Q) e il PUNTO UNICO della
//! loro resa in testo (regola L).
//!
//! # Il difetto che questo modulo toglie
//!
//! `list_active_services` componeva la riga a mano, un `push_str` per colonna,
//! con l'ordine dettato dallo SCHEMA della tabella invece che dall'importanza
//! per chi legge. Misurato il 02/08/2026 (progetto bacheca-attivita), una riga
//! di elenco era:
//!
//! ```text
//! [?] service-66f4bf72 (id: e3711047-bbf0-4792-8640-feb5e7c89a72, pid: 24220,
//! status: failed, exit: 1, avviato: 2026-08-02T07:42:13.968450+00:00)
//!   cmd: ps aux | grep -E "24802|bacheca-attivita-service-66f4bf72|pnpm run dev"
//! ```
//!
//! Tre servizi in questa forma riempiono il riquadro del nastro attivita', che
//! manda a capo (`white-space: pre-wrap`) e taglia a 500 caratteri. Le tre cose
//! che si vogliono sapere — QUALE servizio, se e' VIVO, su quale PORTA —
//! annegavano, e la terza non c'era affatto.
//!
//! Tre cause distinte, tutte nella composizione:
//! - l'ordine dei campi seguiva le colonne del `SELECT`, quindi l'uuid precedeva
//!   lo stato;
//! - `created_at` arrivava gia' appiattito in stringa RFC3339 e veniva stampato
//!   tale e quale: microsecondi e fuso orario, cioe' 32 caratteri per dire
//!   "stamattina". Il rimedio non e' formattare meglio la stringa, e' non
//!   appiattirla alla fonte: [`crate::agent_processes::ProcessSummary`] porta un
//!   `DateTime<Utc>`, e chi rende calcola l'eta' dai due istanti;
//! - lo stato non riconosciuto diventava `[?]`, un marcatore senza vocabolario:
//!   chi legge non poteva sapere se fosse peggio o meglio di `[ATTIVO]`. Qui
//!   [`StatoServizio::Sconosciuto`] porta il valore GREZZO e lo dichiara.
//!
//! # Perche' la resa sta qui e non nel tool
//!
//! Il testo lo leggono DUE consumatori: il modello (che dall'elenco ricava gli
//! `process_id` da passare a `stop_service` / `read_service_output` /
//! `tail_service_logs`) e l'utente, nel nastro attivita'. Sono lo stesso testo,
//! e questo vincola la resa: l'uuid non si puo' togliere — al modello serve
//! intero e copiabile — ma non deve stare dove ruba lo sguardo. Da qui le due
//! righe: la prima dice cosa importa, la seconda porta gli identificatori.
//!
//! I campi vivono in una struttura e il testo si compone DA quella (regola Q):
//! non c'e' nessun punto in cui qualcuno debba rileggere la prosa per sapere se
//! un servizio e' vivo.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::agent_processes::ProcessSummary;
use crate::port_registry::PortAllocation;

/// Quanto del comando entra nella riga secondaria prima di essere abbreviato.
/// Il comando non e' un dettaglio d'implementazione da nascondere: e' cio' che
/// SPIEGA la riga (un `ps aux | grep` registrato come servizio si riconosce solo
/// da li'). Ma non e' nemmeno la cosa piu' importante, quindi va in coda e
/// abbreviato, con la parte omessa dichiarata invece che persa in silenzio.
const COMANDO_MAX_CARATTERI: usize = 96;

/// Larghezza della colonna di stato: la piu' lunga delle etichette note
/// ("in esecuzione"). Le righe si scorrono con l'occhio sulla prima colonna, e
/// il nastro rende il risultato in font monospaziato.
const COLONNA_STATO: usize = 13;

/// Stato di un servizio, dal vocabolario di `agent_processes.status`.
///
/// [`Sconosciuto`](Self::Sconosciuto) porta la stringa grezza invece di
/// collassare in un marcatore muto: se domani il vocabolario del DB cresce, chi
/// legge vede il valore nuovo e sa che il vocabolario e' rimasto indietro. Un
/// `[?]` diceva soltanto che il codice non aveva saputo rispondere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatoServizio {
    /// `starting`: processo creato, PID non ancora registrato.
    InAvvio,
    /// `running`: PID registrato, nessuna uscita osservata.
    InEsecuzione,
    /// `stopped`/`exited`/`finished`: terminato senza errore o fermato.
    Fermo,
    /// `failed`: terminato con exit code diverso da 0.
    Fallito,
    /// Valore che il vocabolario non copre, riportato com'e'.
    Sconosciuto(String),
}

impl StatoServizio {
    /// Traduzione dal vocabolario DB. PUNTO UNICO: prima lo stesso `match`
    /// viveva dentro la composizione della riga, e ogni stato non elencato
    /// finiva in `[?]`.
    pub fn da_stato_db(status: &str) -> Self {
        match status.trim() {
            "starting" => Self::InAvvio,
            "running" => Self::InEsecuzione,
            "stopped" | "exited" | "finished" => Self::Fermo,
            "failed" => Self::Fallito,
            altro => Self::Sconosciuto(altro.to_string()),
        }
    }

    /// Etichetta leggibile. E' la legenda: non c'e' un marcatore da decifrare.
    pub fn etichetta(&self) -> String {
        match self {
            Self::InAvvio => "in avvio".to_string(),
            Self::InEsecuzione => "in esecuzione".to_string(),
            Self::Fermo => "fermo".to_string(),
            Self::Fallito => "fallito".to_string(),
            Self::Sconosciuto(grezzo) => format!("stato '{grezzo}'"),
        }
    }

    /// Il servizio e' (o dovrebbe essere) in ascolto. Stesso criterio con cui
    /// `agent_processes` conta i servizi attivi (`status IN ('running',
    /// 'starting')`).
    pub fn e_vivo(&self) -> bool {
        matches!(self, Self::InAvvio | Self::InEsecuzione)
    }
}

/// Un servizio dell'elenco, coi suoi fatti in campi.
#[derive(Debug, Clone)]
pub struct ServizioElencato {
    /// Identita' del servizio (`agent_processes.label`): la prima cosa che si
    /// cerca leggendo, e la chiave con cui la porta gli e' registrata.
    pub label: String,
    pub stato: StatoServizio,
    /// Porta del bucket registrata a QUESTA label. `None` = nessuna riga in
    /// `nexus_port_allocations`, mai una porta indovinata.
    pub porta: Option<u16>,
    /// `process_id` da passare a `stop_service`/`read_service_output`: e' il
    /// motivo per cui l'uuid resta nel testo, per intero.
    pub id: Uuid,
    pub pid: Option<i32>,
    pub exit_code: Option<i32>,
    pub avviato: DateTime<Utc>,
    pub comando: String,
}

/// L'elenco completo, con l'istante rispetto a cui si misurano le eta'.
#[derive(Debug, Clone)]
pub struct ElencoServizi {
    pub servizi: Vec<ServizioElencato>,
    /// Istante di riferimento per l'eta', PASSATO dal chiamante: la resa resta
    /// una funzione dei suoi input, quindi verificabile senza congelare
    /// l'orologio (regola O).
    pub ora: DateTime<Utc>,
    /// `Some(n)` se il DB ha restituito il massimo di righe e l'elenco potrebbe
    /// essere incompleto. Un troncamento taciuto si legge come "questo e'
    /// tutto".
    pub troncato_a: Option<usize>,
}

/// Costruisce l'elenco dai fatti: le righe di `agent_processes` come le produce
/// [`crate::agent_processes::list_processes`] e le allocazioni del progetto come
/// le tiene [`crate::port_registry`].
pub fn elenco_da_processi(
    processi: &[ProcessSummary],
    porte: &[PortAllocation],
    ora: DateTime<Utc>,
    limite: usize,
) -> ElencoServizi {
    let servizi = processi
        .iter()
        .map(|p| ServizioElencato {
            label: p.label.clone(),
            stato: StatoServizio::da_stato_db(&p.status),
            porta: porta_del_servizio(porte, &p.label),
            id: p.id,
            pid: p.pid,
            exit_code: p.exit_code,
            avviato: p.created_at,
            comando: p.command.clone(),
        })
        .collect::<Vec<_>>();
    ElencoServizi {
        troncato_a: (servizi.len() >= limite).then_some(limite),
        servizi,
        ora,
    }
}

/// Porta registrata a un servizio: uguaglianza della label, mai una somiglianza.
///
/// L'accoppiamento e' STRUTTURALE, non un'euristica: `find_or_allocate` chiava
/// l'allocazione su `(project_id, label)` con la STESSA label con cui il
/// processo e' registrato, quindi due righe con quella label sono lo stesso
/// servizio per costruzione. Il vocabolario largo di
/// `agent_processes::similar_service_labels` (che considera "frontend" e "web"
/// lo stesso scopo) qui sarebbe sbagliato: risponde a un'altra domanda — "queste
/// due label valgono lo stesso ruolo?" — e usarlo attribuirebbe a un servizio la
/// porta di un altro, che e' l'incidente gia' misurato sull'adozione dei
/// processi. Nessuna corrispondenza -> nessuna porta dichiarata.
fn porta_del_servizio(porte: &[PortAllocation], label: &str) -> Option<u16> {
    let label = label.trim();
    porte
        .iter()
        .find(|a| a.label.trim().eq_ignore_ascii_case(label))
        .map(|a| a.port)
}

impl ElencoServizi {
    /// La resa: intestazione coi conteggi, poi due righe per servizio.
    ///
    /// PUNTO UNICO (regola L): il testo si compone qui, dai campi, e nessun
    /// altro punto del sistema formatta un elenco di servizi.
    pub fn testo(&self) -> String {
        if self.servizi.is_empty() {
            return "Nessun servizio registrato per questo progetto.".to_string();
        }
        let mut out = self.intestazione();
        out.push_str("\n\n");
        for s in &self.servizi {
            out.push_str(&self.riga_principale(s));
            out.push('\n');
            out.push_str(&riga_identificatori(s));
            out.push('\n');
        }
        out
    }

    /// Conteggi per stato, nell'ordine in cui interessano: prima cio' che e'
    /// vivo. Solo gli stati presenti: elencare "0 falliti" e' rumore.
    fn intestazione(&self) -> String {
        let mut voci: Vec<String> = Vec::new();
        for (stato, singolare, plurale) in [
            (StatoServizio::InEsecuzione, "in esecuzione", "in esecuzione"),
            (StatoServizio::InAvvio, "in avvio", "in avvio"),
            (StatoServizio::Fermo, "fermo", "fermi"),
            (StatoServizio::Fallito, "fallito", "falliti"),
        ] {
            let n = self.servizi.iter().filter(|s| s.stato == stato).count();
            if n > 0 {
                voci.push(format!("{n} {}", if n == 1 { singolare } else { plurale }));
            }
        }
        let sconosciuti = self
            .servizi
            .iter()
            .filter(|s| matches!(s.stato, StatoServizio::Sconosciuto(_)))
            .count();
        if sconosciuti > 0 {
            voci.push(format!("{sconosciuti} in stato non riconosciuto"));
        }
        let mut testa = format!("Servizi del progetto: {}", voci.join(", "));
        if let Some(limite) = self.troncato_a {
            // DA DOVE guarda l'elenco: senza questa riga il numero sopra si
            // legge come "tutti i servizi del progetto" (regola O).
            testa.push_str(&format!(
                " (elenco limitato ai {limite} avviati piu' di recente)"
            ));
        }
        testa.push('.');
        testa
    }

    /// Cio' che serve per decidere: stato, chi e', dove ascolta, da quanto.
    fn riga_principale(&self, s: &ServizioElencato) -> String {
        let mut campi = vec![
            format!("{:<larghezza$}", s.stato.etichetta(), larghezza = COLONNA_STATO),
            s.label.clone(),
        ];
        match s.porta {
            Some(p) => campi.push(format!("porta {p}")),
            // Il silenzio va dichiarato solo dove significa qualcosa: un
            // servizio VIVO senza porta registrata e' un fatto (il pannello non
            // ha un indirizzo da mostrargli); uno fermo senza porta e' normale.
            None if s.stato.e_vivo() => campi.push("porta non registrata".to_string()),
            None => {}
        }
        if let Some(code) = s.exit_code {
            if code != 0 || s.stato == StatoServizio::Fallito {
                campi.push(format!("uscita {code}"));
            }
        }
        campi.push(format!("da {}", eta_leggibile(s.avviato, self.ora)));
        campi.join("  ")
    }
}

/// Riga secondaria: gli identificatori tecnici e il comando, in secondo piano.
/// L'uuid resta INTERO perche' e' il `process_id` che il modello deve passare ai
/// tool di servizio; abbreviarlo lo renderebbe inutilizzabile.
fn riga_identificatori(s: &ServizioElencato) -> String {
    let mut campi = vec![format!("id {}", s.id)];
    if let Some(pid) = s.pid {
        campi.push(format!("pid {pid}"));
    }
    campi.push(format!("avvio: {}", comando_breve(&s.comando)));
    format!("    {}", campi.join("  "))
}

/// Comando su una riga sola e abbreviato, con la parte omessa DICHIARATA.
///
/// L'appiattimento non e' cosmetico: un comando multi-riga spezzerebbe la
/// struttura a due righe per servizio, e da li' in poi nessuna riga
/// corrisponderebbe piu' a cio' che dice di essere.
fn comando_breve(comando: &str) -> String {
    let piatto = comando.split_whitespace().collect::<Vec<_>>().join(" ");
    let totale = piatto.chars().count();
    if totale <= COMANDO_MAX_CARATTERI {
        return piatto;
    }
    let testa: String = piatto.chars().take(COMANDO_MAX_CARATTERI).collect();
    format!("{testa}... (+{} car.)", totale - COMANDO_MAX_CARATTERI)
}

/// Eta' leggibile fra due istanti. Sostituisce il timestamp RFC3339 con
/// microsecondi: "da 2h 14m" e' cio' che si voleva sapere guardando la data.
///
/// Un istante nel FUTURO non e' un errore da nascondere ne' un'eta' negativa da
/// stampare: e' un orologio che non concorda (righe scritte dal DB, resa
/// calcolata dal processo), e degrada al gradino piu' piccolo.
fn eta_leggibile(da: DateTime<Utc>, a: DateTime<Utc>) -> String {
    let secondi = a.signed_duration_since(da).num_seconds();
    if secondi < 60 {
        return "meno di 1m".to_string();
    }
    let minuti = secondi / 60;
    if minuti < 60 {
        return format!("{minuti}m");
    }
    let ore = minuti / 60;
    if ore < 24 {
        return format!("{ore}h {:02}m", minuti % 60);
    }
    format!("{}g {}h", ore / 24, ore % 24)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn istante(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .expect("istante valido")
            .with_timezone(&Utc)
    }

    /// Riga di `agent_processes` nella forma ESATTA in cui `list_processes` la
    /// produce: e' il tipo che attraversa il confine col DB, quindi partire da
    /// li' e' l'unico modo per misurare la resa vera (regola O).
    fn processo(
        label: &str,
        status: &str,
        command: &str,
        pid: Option<i32>,
        exit_code: Option<i32>,
        avviato: &str,
    ) -> ProcessSummary {
        ProcessSummary {
            id: Uuid::parse_str("e3711047-bbf0-4792-8640-feb5e7c89a72").expect("uuid"),
            label: label.to_string(),
            command: command.to_string(),
            pid,
            status: status.to_string(),
            exit_code,
            created_at: istante(avviato),
        }
    }

    fn allocazione(label: &str, port: u16) -> PortAllocation {
        PortAllocation {
            id: Uuid::nil(),
            project_id: Uuid::nil(),
            port,
            label: label.to_string(),
            allocation_mode: "auto".to_string(),
            run_config_id: None,
            service_unit: None,
        }
    }

    /// IL test del difetto misurato il 02/08/2026: la riga dello screenshot,
    /// ricostruita dai campi che il DB portava, e cio' che si deve poter leggere
    /// senza scorrere.
    ///
    /// MUTAZIONE: rimettere l'uuid davanti allo stato in `riga_principale` ->
    /// l'asserzione sull'inizio della riga rosseggia.
    #[test]
    fn la_riga_dice_prima_cio_che_conta() {
        let elenco = elenco_da_processi(
            &[processo(
                "frontend",
                "running",
                "npm run dev",
                Some(24220),
                None,
                "2026-08-02T07:42:13.968450+00:00",
            )],
            &[allocazione("frontend", 24801)],
            istante("2026-08-02T09:56:13.968450+00:00"),
            20,
        );
        let testo = elenco.testo();
        let riga = testo
            .lines()
            .find(|l| l.starts_with("in esecuzione"))
            .unwrap_or_else(|| panic!("nessuna riga principale in:\n{testo}"));

        // Le tre cose che si volevano sapere, tutte prima degli identificatori.
        assert!(riga.contains("frontend"), "manca il QUALE: {riga}");
        assert!(riga.contains("porta 24801"), "manca la PORTA: {riga}");
        assert!(riga.contains("da 2h 14m"), "manca il DA QUANTO: {riga}");
        // L'uuid non e' sparito: e' sceso di riga, perche' al modello serve.
        assert!(
            !riga.contains("e3711047"),
            "l'uuid e' tornato sulla riga principale: {riga}"
        );
        assert!(
            testo.contains("id e3711047-bbf0-4792-8640-feb5e7c89a72"),
            "l'uuid deve restare INTERO: e' il process_id dei tool di servizio"
        );
        // Il timestamp con microsecondi e fuso non compare piu' da nessuna parte.
        assert!(
            !testo.contains("968450"),
            "microsecondi di nuovo nel testo:\n{testo}"
        );
    }

    /// Lo stato non riconosciuto DICHIARA il valore grezzo. Il vecchio `[?]` non
    /// diceva nemmeno se fosse peggio di `[ATTIVO]`.
    ///
    /// MUTAZIONE: `altro => Self::Fermo` in `da_stato_db` -> rosso, perche' un
    /// vocabolario incompleto tornerebbe a mentire invece di dichiararsi.
    #[test]
    fn uno_stato_fuori_vocabolario_si_dichiara() {
        let stato = StatoServizio::da_stato_db("zombie");
        assert_eq!(stato, StatoServizio::Sconosciuto("zombie".to_string()));
        assert!(stato.etichetta().contains("zombie"));
        assert!(!stato.e_vivo(), "l'ignoto non si conta fra i vivi");
    }

    /// Un servizio fallito porta il PERCHE' (uscita) in prima riga, e il comando
    /// che lo definisce resta leggibile in seconda: e' l'unica cosa che spiega
    /// perche' quella riga esista.
    #[test]
    fn un_fallito_dice_l_uscita_e_conserva_il_comando() {
        let elenco = elenco_da_processi(
            &[processo(
                "service-66f4bf72",
                "failed",
                "ps aux | grep -E \"24802|bacheca-attivita-service-66f4bf72|pnpm run dev\"",
                Some(24220),
                Some(1),
                "2026-08-02T07:42:13+00:00",
            )],
            &[],
            istante("2026-08-02T09:00:13+00:00"),
            20,
        );
        let testo = elenco.testo();
        assert!(testo.contains("fallito"), "{testo}");
        assert!(testo.contains("uscita 1"), "{testo}");
        assert!(testo.contains("avvio: ps aux"), "{testo}");
        // Un fallito non e' vivo: nessuna dichiarazione di porta mancante.
        assert!(
            !testo.contains("porta non registrata"),
            "porta dichiarata mancante su un servizio fermo: {testo}"
        );
        // Due righe per servizio, sempre: l'intestazione, la vuota, le due.
        assert_eq!(
            testo.lines().filter(|l| !l.is_empty()).count(),
            3,
            "struttura a due righe per servizio persa:\n{testo}"
        );
    }

    /// Un servizio VIVO senza allocazione e' un fatto, non un silenzio: il
    /// pannello non ha un indirizzo da mostrargli.
    #[test]
    fn un_vivo_senza_porta_lo_dichiara() {
        let elenco = elenco_da_processi(
            &[processo(
                "backend",
                "starting",
                "npm start",
                None,
                None,
                "2026-08-02T09:00:00+00:00",
            )],
            &[allocazione("frontend", 24801)],
            istante("2026-08-02T09:00:30+00:00"),
            20,
        );
        let testo = elenco.testo();
        assert!(testo.contains("porta non registrata"), "{testo}");
        assert!(
            !testo.contains("24801"),
            "porta di un ALTRO servizio attribuita a questo: {testo}"
        );
        assert!(testo.contains("da meno di 1m"), "{testo}");
        assert!(!testo.contains("pid"), "pid assente, niente da stampare: {testo}");
    }

    /// La label decide da sola: nessuna somiglianza, nessun ripiego.
    ///
    /// MUTAZIONE: usare `similar_service_labels` al posto dell'uguaglianza ->
    /// "web" prenderebbe la porta di "frontend", che e' l'incidente di
    /// attribuzione gia' misurato sull'adozione dei processi.
    #[test]
    fn la_porta_si_lega_alla_label_esatta() {
        let porte = [allocazione("frontend", 24801), allocazione("api", 24802)];
        assert_eq!(porta_del_servizio(&porte, "frontend"), Some(24801));
        assert_eq!(porta_del_servizio(&porte, " api "), Some(24802));
        assert_eq!(
            porta_del_servizio(&porte, "web"),
            None,
            "'web' e 'frontend' sono lo stesso RUOLO, non lo stesso servizio"
        );
    }

    /// Il troncamento dell'elenco si dichiara: senza, i conteggi si leggono come
    /// "tutti i servizi del progetto".
    #[test]
    fn l_elenco_incompleto_lo_dice() {
        let righe: Vec<ProcessSummary> = (0..3)
            .map(|i| {
                processo(
                    &format!("s{i}"),
                    "stopped",
                    "npm start",
                    None,
                    Some(0),
                    "2026-08-02T09:00:00+00:00",
                )
            })
            .collect();
        let pieno = elenco_da_processi(&righe, &[], istante("2026-08-02T09:10:00+00:00"), 3);
        assert!(pieno.testo().contains("elenco limitato ai 3"));
        let parziale = elenco_da_processi(&righe, &[], istante("2026-08-02T09:10:00+00:00"), 20);
        assert!(
            !parziale.testo().contains("elenco limitato"),
            "troncamento dichiarato senza esserci"
        );
        // exit 0 su un servizio fermo non e' una notizia.
        assert!(!parziale.testo().contains("uscita 0"));
    }

    /// Un comando multi-riga non deve poter spezzare la struttura.
    #[test]
    fn il_comando_resta_su_una_riga_e_dichiara_cio_che_omette() {
        let breve = comando_breve("npm  run\n  dev");
        assert_eq!(breve, "npm run dev");
        let lungo = comando_breve(&"x".repeat(COMANDO_MAX_CARATTERI + 12));
        assert!(lungo.ends_with("... (+12 car.)"), "{lungo}");
        assert!(!lungo.contains('\n'));
    }

    /// Un orologio che non concorda non produce un'eta' negativa.
    #[test]
    fn un_istante_futuro_degrada_al_gradino_piu_piccolo() {
        let ora = istante("2026-08-02T09:00:00+00:00");
        let futuro = istante("2026-08-02T09:05:00+00:00");
        assert_eq!(eta_leggibile(futuro, ora), "meno di 1m");
        assert_eq!(eta_leggibile(istante("2026-07-30T05:00:00+00:00"), ora), "3g 4h");
    }

    // ── Dal DB reale: la resa nasce dalla query di produzione ────────────────

    /// Elenco costruito passando dalla LETTURA di produzione
    /// ([`crate::agent_processes::list_processes_from`]) sullo schema portato
    /// dalla migrazione vera. E' la giunzione che i test puri sopra non toccano:
    /// che `created_at` arrivi come istante (e non come stringa gia' formattata)
    /// e che l'ordine sia quello che l'utente vede.
    ///
    /// MUTAZIONE: rimettere `.to_rfc3339()` nella mappatura di `ProcessSummary`
    /// -> il crate non compila piu', che e' il modo in cui questo difetto smette
    /// di poter tornare.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn dal_db_reale_l_elenco_ordina_e_calcola_l_eta(pool: sqlx::PgPool) {
        let project_id = Uuid::new_v4();
        for (label, status, command, pid, exit, minuti_fa) in [
            ("frontend", "running", "npm run dev", Some(24220), None, 134_i32),
            (
                "sonda",
                "failed",
                "ps aux | grep -E \"24802|pnpm run dev\"",
                Some(24802),
                Some(1),
                10,
            ),
        ] {
            sqlx::query(
                "INSERT INTO agent_processes \
                 (project_id, label, command, pid, status, exit_code, created_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, NOW() - make_interval(mins => $7))",
            )
            .bind(project_id)
            .bind(label)
            .bind(command)
            .bind(pid)
            .bind(status)
            .bind(exit)
            .bind(minuti_fa)
            .execute(&pool)
            .await
            .expect("insert processo");
        }

        let righe = crate::agent_processes::list_processes_from(&pool, project_id)
            .await
            .expect("lettura processi");
        assert_eq!(righe.len(), 2);
        let elenco = elenco_da_processi(
            &righe,
            &[allocazione("frontend", 24801)],
            Utc::now(),
            crate::agent_processes::LIMITE_ELENCO_PROCESSI,
        );
        let testo = elenco.testo();

        assert!(
            testo.starts_with("Servizi del progetto: 1 in esecuzione, 1 fallito."),
            "intestazione inattesa:\n{testo}"
        );
        // ORDER BY created_at DESC: il piu' recente per primo.
        let ordine: Vec<&str> = testo
            .lines()
            .filter(|l| l.starts_with("fallito") || l.starts_with("in esecuzione"))
            .collect();
        assert_eq!(ordine.len(), 2, "righe principali attese: 2\n{testo}");
        assert!(ordine[0].contains("sonda"), "ordine invertito: {:?}", ordine);
        assert!(ordine[1].contains("frontend"), "ordine invertito: {:?}", ordine);
        // L'eta' e' CALCOLATA dall'istante letto dal DB: 134 minuti = 2h 14m.
        assert!(ordine[1].contains("da 2h 14m"), "{}", ordine[1]);
        assert!(ordine[1].contains("porta 24801"), "{}", ordine[1]);
        // Nessuna data assoluta, in nessuna forma: ne' l'RFC3339 col fuso ne' il
        // Display di `DateTime`. L'anno e' il segno comune ai due.
        let anno = Utc::now().format("%Y").to_string();
        assert!(
            !testo.contains(&anno),
            "un istante assoluto e' tornato nel testo:\n{testo}"
        );
    }

    /// Il LIMIT della query e il limite dichiarato nella resa sono lo STESSO
    /// numero: se divergessero, l'elenco direbbe "limitato ai 20" mostrandone
    /// altri, o tacerebbe un troncamento avvenuto.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn il_troncamento_dichiarato_e_quello_applicato(pool: sqlx::PgPool) {
        let project_id = Uuid::new_v4();
        let extra = 3_i64;
        sqlx::query(
            // `i` nasce bigint dal bind di $2: `make_interval` vuole un int, e
            // senza il cast la fixture fallisce per il tipo, non per il fatto
            // che misura.
            "INSERT INTO agent_processes (project_id, label, command, status, created_at) \
             SELECT $1, 'svc-' || i, 'npm start', 'stopped', NOW() - make_interval(mins => i::int) \
             FROM generate_series(1, $2) AS i",
        )
        .bind(project_id)
        .bind(crate::agent_processes::LIMITE_ELENCO_PROCESSI as i64 + extra)
        .execute(&pool)
        .await
        .expect("insert processi");

        let righe = crate::agent_processes::list_processes_from(&pool, project_id)
            .await
            .expect("lettura processi");
        assert_eq!(righe.len(), crate::agent_processes::LIMITE_ELENCO_PROCESSI);
        let testo = elenco_da_processi(
            &righe,
            &[],
            Utc::now(),
            crate::agent_processes::LIMITE_ELENCO_PROCESSI,
        )
        .testo();
        assert!(
            testo.contains(&format!(
                "elenco limitato ai {} avviati piu' di recente",
                crate::agent_processes::LIMITE_ELENCO_PROCESSI
            )),
            "troncamento non dichiarato:\n{testo}"
        );
    }
}
