//! IL CENSIMENTO del registro delle porte — ADR 0042, passo P0(b).
//!
//! Misura, su TUTTO il parco progetti, quante identita' di servizio contiene
//! `nexus_port_allocations` e quante di esse descrivono qualcosa che esiste
//! davvero. E' il metro del prima-dopo: lo stesso strumento che oggi conta le
//! nove identita' di `bacheca-attivita` dovra' dire, a piano concluso, una riga
//! per servizio reale (P8).
//!
//! # Come si esegue
//!
//! ```text
//! cargo test --bin mcp-core censimento -- --ignored --nocapture
//! ```
//!
//! Legge `DATABASE_URL` (DB meta) da ambiente o da `.env` risalendo dalla CWD.
//!
//! # Perche' vive dentro mcp-core e non in `xtask`
//!
//! La domanda «chi ascolta ADESSO» ha un punto unico —
//! [`crate::project_workspace::port_recovery::scan_listening_ports`] — e quel
//! punto unico e' `pub(crate)` dentro un crate che NON ha target `lib`: mcp-core
//! e' un binario puro (solo `src/main.rs`). Un sottocomando `xtask` non potrebbe
//! raggiungerlo e dovrebbe ricostruire l'enumerazione dei listener per conto
//! proprio: sarebbe la regola O violata dallo strumento che esiste per
//! applicarla — una misura che interroga il SO per una strada che la produzione
//! non percorre, e che quindi non puo' vedere cio' che la produzione vede.
//!
//! Il caso concreto non e' teorico: `scan_listening_ports` enumera ENTRAMBE le
//! famiglie di indirizzi, e su questa macchina alcune porte del bucket progetti
//! sono in ascolto SOLO su IPv6. Un censimento con un probe proprio le
//! dichiarerebbe mute, e il numero «quante ascoltano adesso» — cioe' il numero
//! su cui l'intero ADR fonda la diagnosi — sarebbe sistematicamente basso.
//!
//! # Cosa NON fa
//!
//! Non scrive niente. Non ferma niente. Non adotta niente. E' una lettura.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::project_workspace::port_recovery::ListenerScan;

/// Il verdetto sulla coppia `(label, service_unit)` di una riga.
///
/// L'unit si costruisce DALLA label
/// ([`crate::project_workspace::services::service_unit_name`]): finche' nessuno
/// riscrive, `service_unit == {slug}-{label}.service` e' un'identita', non una
/// coincidenza. Una divergenza e' percio' la PROVA che la label e' cambiata dopo
/// che l'unit era stata scritta — e l'unico percorso che lo fa e'
/// `register_detected_port`, la cui `ON CONFLICT (port) DO UPDATE SET label`
/// riscrive l'identita' del servizio senza toccare l'unit (ADR 0042, "Il furto
/// di identita'").
///
/// Enum chiuso e non `bool` (regola Q): «non c'e' unit da confrontare» e «non
/// so quale slug userebbe la produzione» non sono ne' un si' ne' un no, e
/// contarli come coerenti nasconderebbe righe che nessuno ha mai potuto giudicare.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "verdetto", rename_all = "snake_case")]
pub enum VerdettoCoppia {
    /// `service_unit` e' esattamente cio' che la label produce oggi.
    Coerente,
    /// `service_unit` non e' derivabile dalla label: qualcuno ha riscritto la
    /// label dopo. Porta l'unit che la label PRODURREBBE, che e' il valore del
    /// difetto (senza, il numero non si sa spiegare a nessuno).
    Riscritta { unit_attesa: String },
    /// La riga non ha `service_unit`: non c'e' nessuna coppia da giudicare.
    /// Non e' una riga sana, e' una riga muta.
    UnitAssente,
}

/// Quante porte del progetto risultano in ascolto, con l'ignoto dichiarato.
///
/// Gemello di [`ListenerScan`], da cui deriva: se la tabella dei listener non
/// e' stata letta per intero, «zero in ascolto» sarebbe la stessa risposta che
/// darebbe un parco spento — e su quella differenza si decide se il registro e'
/// sporco o se e' la macchina a essere ferma.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "osservazione", rename_all = "snake_case")]
pub enum PorteInAscolto {
    Contate { porte: usize, pid_distinti: usize },
    NonOsservabile { motivo: String },
}

impl PorteInAscolto {
    /// Il numero da mettere in una tabella, quando c'e'. `None` non e' zero.
    pub fn quante(&self) -> Option<usize> {
        match self {
            Self::Contate { porte, .. } => Some(*porte),
            Self::NonOsservabile { .. } => None,
        }
    }
}

/// Una riga di `nexus_port_allocations`, con tutto cio' che il censimento le ha
/// chiesto.
#[derive(Debug, Clone, Serialize)]
pub struct RigaCensita {
    pub port: u16,
    pub label: String,
    pub service_unit: Option<String>,
    pub allocation_mode: String,
    pub creata: String,
    #[serde(flatten)]
    pub verdetto: VerdettoCoppia,
    /// La label non porta nessuna parola che dica uno scopo
    /// (`is_generic_service_label`), oppure e' esattamente la label di ripiego
    /// che l'ancoraggio all'uuid produce per QUESTO progetto.
    pub label_generica: bool,
    /// La label e' l'identita' di ancoraggio (`service-{uuid[..8]}`): non e'
    /// priva di senso, dichiara che nessun segnale disse il ruolo.
    pub label_di_ancoraggio: bool,
    /// La label contiene uno spazio: nessun percorso che normalizzi puo' averla
    /// prodotta, quindi e' arrivata verbatim dall'esterno.
    pub label_con_spazio: bool,
    /// La label ripete lo slug del progetto: l'unit che ne discende dice due
    /// volte il progetto e mai il ruolo. E' auto-amplificante.
    pub label_slug_duplicato: bool,
    /// `None` = la tabella dei listener non e' stata interrogabile. Mai `false`
    /// per l'ignoto (regola Q).
    pub in_ascolto: Option<bool>,
    pub listener_pid: Option<u32>,
    pub listener_program: Option<String>,
}

/// Il censimento di un progetto.
#[derive(Debug, Clone, Serialize)]
pub struct CensimentoProgetto {
    pub project_id: Uuid,
    pub slug: String,
    /// Lo slug con cui si costruiscono le unit: `project_service_slug(name)`,
    /// che NON coincide necessariamente con `projects.slug`. Dichiarato perche'
    /// e' la premessa di ogni verdetto sulle coppie (regola O).
    pub slug_unit: String,
    pub bucket: (u16, u16),
    pub righe: usize,
    pub in_ascolto: PorteInAscolto,
    pub coppie_incoerenti: usize,
    pub coppie_senza_unit: usize,
    /// Righe la cui `service_unit` e' condivisa con almeno un'altra riga dello
    /// stesso progetto. E' la CONSEGUENZA del furto, distinta dalla sua prova:
    /// il ladro (riscritta) e la vittima (nata dopo, con la stessa unit) sono
    /// due righe, e un conteggio che ne guardi una sola non racconta il danno.
    pub righe_su_unit_condivisa: usize,
    pub unit_condivise: usize,
    pub label_generiche: usize,
    pub label_di_ancoraggio: usize,
    pub label_con_spazio: usize,
    pub label_slug_duplicato: usize,
    pub dettaglio: Vec<RigaCensita>,
}

/// L'intero parco.
#[derive(Debug, Clone, Serialize)]
pub struct Censimento {
    pub misurato_il: String,
    /// Come e' andata l'unica osservazione del sistema operativo: un solo
    /// snapshot per tutto il censimento, cosi' due progetti non finiscono
    /// giudicati su due istanti diversi.
    pub osservazione: String,
    pub progetti: Vec<CensimentoProgetto>,
    pub totale_righe: usize,
}

/// Il verdetto sulla coppia, PURO: nessun I/O, cosi' si prova riga per riga
/// senza un DB e senza un sistema operativo.
///
/// `slug_unit` e' lo slug con cui la produzione costruisce le unit, non
/// `projects.slug`: la differenza e' reale (`project_service_slug` lavora sul
/// NOME) ed e' gia' stata causa di unit divergenti.
pub fn giudica_coppia(slug_unit: &str, label: &str, service_unit: Option<&str>) -> VerdettoCoppia {
    let Some(unit) = service_unit else {
        return VerdettoCoppia::UnitAssente;
    };
    let attesa = crate::project_workspace::services::service_unit_name(slug_unit, label);
    if unit == attesa {
        VerdettoCoppia::Coerente
    } else {
        VerdettoCoppia::Riscritta {
            unit_attesa: attesa,
        }
    }
}

/// La label ripete lo slug del progetto? Criterio: e' lo slug, oppure ne porta
/// il prefisso seguito dal separatore con cui l'unit si compone.
///
/// E' la firma dell'accumulo descritto nell'ADR: l'inversa unit -> label, che
/// non ha punto unico, ricade sulla stringa INTERA quando il suffisso non c'e',
/// quindi rimette il prefisso appena tolto e la label successiva nasce gia'
/// contenendo lo slug.
pub fn label_ripete_lo_slug(slug_unit: &str, label: &str) -> bool {
    if slug_unit.is_empty() {
        return false;
    }
    label == slug_unit || label.starts_with(&format!("{slug_unit}-"))
}

/// Esegue il censimento sull'intero parco. Sola lettura.
pub async fn esegui(db: &PgPool) -> anyhow::Result<Censimento> {
    // UNA sola osservazione del sistema operativo, condivisa da tutti i
    // progetti: due scan darebbero a due progetti due istanti diversi, e il
    // confronto prima-dopo perderebbe senso proprio sul numero che conta.
    let scan = crate::project_workspace::port_recovery::scan_listening_ports().await;
    let osservazione = scan.descrizione();
    let listener: HashMap<u16, (u32, String)> = match &scan {
        ListenerScan::Osservati(v) => v
            .iter()
            .map(|(porta, pid, prog)| (*porta, (*pid, prog.clone())))
            .collect(),
        ListenerScan::NonInterrogabile { .. } => HashMap::new(),
    };

    let progetti = sqlx::query("SELECT id, name, slug FROM projects ORDER BY slug")
        .fetch_all(db)
        .await?;

    let mut out = Vec::new();
    let mut totale_righe = 0usize;

    for p in progetti {
        let project_id: Uuid = p.try_get("id")?;
        let name: String = p.try_get("name")?;
        let slug: String = p.try_get("slug")?;
        let slug_unit = crate::project_workspace::services::project_service_slug(&name);
        let ancoraggio = crate::agent_tools::service::label_di_solo_ancoraggio(project_id);

        let righe = sqlx::query(
            "SELECT port, label, service_unit, allocation_mode, created_at \
             FROM nexus_port_allocations WHERE project_id = $1 ORDER BY created_at",
        )
        .bind(project_id)
        .fetch_all(db)
        .await?;

        let mut dettaglio: Vec<RigaCensita> = Vec::new();
        let mut unit_viste: HashMap<String, usize> = HashMap::new();

        for r in righe {
            let port: i32 = r.try_get("port")?;
            let port = port as u16;
            let label: String = r.try_get("label")?;
            let service_unit: Option<String> = r.try_get("service_unit")?;
            let allocation_mode: String = r.try_get("allocation_mode")?;
            let creata: chrono::DateTime<chrono::Utc> = r.try_get("created_at")?;

            if let Some(u) = service_unit.as_deref() {
                *unit_viste.entry(u.to_string()).or_insert(0) += 1;
            }

            let occupante = listener.get(&port);
            dettaglio.push(RigaCensita {
                port,
                verdetto: giudica_coppia(&slug_unit, &label, service_unit.as_deref()),
                label_generica: crate::agent_processes::is_generic_service_label(&label)
                    || label == ancoraggio,
                label_di_ancoraggio: label == ancoraggio,
                label_con_spazio: label.chars().any(char::is_whitespace),
                label_slug_duplicato: label_ripete_lo_slug(&slug_unit, &label),
                // `ascolta` e' il punto unico della domanda, e ritorna `None`
                // quando la tabella non e' stata letta: qui l'ignoto resta tale.
                //
                // PROVA DI MUTAZIONE (regola O), eseguita il 02/08/2026:
                // sostituendo questa riga con `Some(false)` — cioe' uno
                // strumento che dichiara mute tutte le porte —
                // `una_porta_con_listener_vivo_risulta_in_ascolto` fallisce con
                // `left: Some(false)` / `right: Some(true)`, il valore del
                // difetto. Senza quella prova, un censimento che riporta «0 in
                // ascolto» a parco spento sarebbe indistinguibile da uno cieco.
                in_ascolto: scan.ascolta(port),
                listener_pid: occupante.map(|(pid, _)| *pid),
                listener_program: occupante.map(|(_, prog)| prog.clone()),
                label,
                service_unit,
                allocation_mode,
                creata: creata.to_rfc3339(),
            });
        }

        let porte_in_ascolto: Vec<&RigaCensita> = dettaglio
            .iter()
            .filter(|r| r.in_ascolto == Some(true))
            .collect();
        let in_ascolto = if scan.osservazione_avvenuta() {
            let pid: HashSet<u32> = porte_in_ascolto.iter().filter_map(|r| r.listener_pid).collect();
            PorteInAscolto::Contate {
                porte: porte_in_ascolto.len(),
                pid_distinti: pid.len(),
            }
        } else {
            PorteInAscolto::NonOsservabile {
                motivo: osservazione.clone(),
            }
        };

        let unit_condivise = unit_viste.values().filter(|n| **n > 1).count();
        let righe_su_unit_condivisa = dettaglio
            .iter()
            .filter(|r| {
                r.service_unit
                    .as_deref()
                    .and_then(|u| unit_viste.get(u))
                    .is_some_and(|n| *n > 1)
            })
            .count();

        totale_righe += dettaglio.len();
        out.push(CensimentoProgetto {
            project_id,
            slug,
            slug_unit,
            bucket: nexus_tool_kit::ports::project_bucket_range(&project_id),
            righe: dettaglio.len(),
            in_ascolto,
            coppie_incoerenti: dettaglio
                .iter()
                .filter(|r| matches!(r.verdetto, VerdettoCoppia::Riscritta { .. }))
                .count(),
            coppie_senza_unit: dettaglio
                .iter()
                .filter(|r| matches!(r.verdetto, VerdettoCoppia::UnitAssente))
                .count(),
            righe_su_unit_condivisa,
            unit_condivise,
            label_generiche: dettaglio.iter().filter(|r| r.label_generica).count(),
            label_di_ancoraggio: dettaglio.iter().filter(|r| r.label_di_ancoraggio).count(),
            label_con_spazio: dettaglio.iter().filter(|r| r.label_con_spazio).count(),
            label_slug_duplicato: dettaglio.iter().filter(|r| r.label_slug_duplicato).count(),
            dettaglio,
        });
    }

    Ok(Censimento {
        misurato_il: chrono::Utc::now().to_rfc3339(),
        osservazione,
        progetti: out,
        totale_righe,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lo slug delle unit viene dal NOME del progetto, non da `projects.slug`:
    /// il test lo attraversa dal punto unico, invece di fissarlo a mano.
    fn slug_unit(nome: &str) -> String {
        crate::project_workspace::services::project_service_slug(nome)
    }

    /// LE DUE RIGHE DEL FURTO, coi valori reali misurati il 02/08/2026 su
    /// `bacheca-attivita`. Se `service_unit_name` cambia forma, questo test
    /// rosseggia sul difetto vero e non su una stringa ricopiata.
    #[test]
    fn una_unit_non_derivabile_dalla_label_e_una_riscrittura() {
        let s = slug_unit("bacheca-attivita");

        // 24826: la label e' stata riscritta a "Service" sopra un'unit nata da
        // "backend". E' la firma di `register_detected_port`.
        assert_eq!(
            giudica_coppia(&s, "Service", Some("bacheca-attivita-backend.service")),
            VerdettoCoppia::Riscritta {
                unit_attesa: "bacheca-attivita-Service.service".to_string()
            }
        );

        // 24806: stessa forma sul frontend.
        assert_eq!(
            giudica_coppia(
                &s,
                "frontend-preview",
                Some("bacheca-attivita-frontend.service")
            ),
            VerdettoCoppia::Riscritta {
                unit_attesa: "bacheca-attivita-frontend-preview.service".to_string()
            }
        );
    }

    /// Le righe SANE non devono contarsi come riscritte, comprese quelle brutte:
    /// uno spazio nella label e uno slug ripetuto sono difetti di IDENTITA', non
    /// prove di riscrittura, e confonderli gonfierebbe il numero che l'ADR usa
    /// per attribuire la causa a un produttore preciso.
    #[test]
    fn una_label_brutta_ma_coerente_non_e_una_riscrittura() {
        let s = slug_unit("bacheca-attivita");
        for (label, unit) in [
            ("frontend dev", "bacheca-attivita-frontend dev.service"),
            ("backend", "bacheca-attivita-backend.service"),
            (
                "bacheca-attivita-frontend",
                "bacheca-attivita-bacheca-attivita-frontend.service",
            ),
            (
                "bacheca-attivita-frontend dev.service",
                "bacheca-attivita-bacheca-attivita-frontend dev.service.service",
            ),
        ] {
            assert_eq!(
                giudica_coppia(&s, label, Some(unit)),
                VerdettoCoppia::Coerente,
                "label '{label}' contro unit '{unit}'"
            );
        }
    }

    /// L'assenza di unit non e' una coerenza: non c'e' coppia da giudicare.
    #[test]
    fn senza_unit_non_si_giudica() {
        assert_eq!(
            giudica_coppia("bacheca-attivita", "frontend", None),
            VerdettoCoppia::UnitAssente
        );
    }

    /// Lo slug ripetuto si riconosce sul prefisso col separatore, non su
    /// `contains`: un progetto `api` non deve vedere `api-gateway` come una
    /// ripetizione del proprio nome quando lo slug e' `gateway-api`.
    #[test]
    fn lo_slug_ripetuto_e_un_prefisso_non_una_sottostringa() {
        assert!(label_ripete_lo_slug(
            "bacheca-attivita",
            "bacheca-attivita-frontend"
        ));
        assert!(label_ripete_lo_slug("bacheca-attivita", "bacheca-attivita"));
        assert!(!label_ripete_lo_slug("bacheca-attivita", "frontend"));
        // Sottostringa in mezzo: non e' la firma dell'accumulo.
        assert!(!label_ripete_lo_slug("api", "gestione-api-esterne"));
        assert!(!label_ripete_lo_slug("", "qualunque"));
    }

    /// L'ancoraggio si riconosce dalla formula del PRODUTTORE, non da una regex.
    #[test]
    fn l_ancoraggio_si_riconosce_dal_produttore() {
        let id = Uuid::parse_str("66f4bf72-3975-4bb0-bc38-5e1107bf1d94").expect("uuid");
        assert_eq!(
            crate::agent_tools::service::label_di_solo_ancoraggio(id),
            "service-66f4bf72"
        );
    }

    /// «Non ho potuto guardare» non diventa mai «nessuno ascolta».
    #[test]
    fn l_ignoto_non_si_conta_come_zero() {
        let non_osservabile = PorteInAscolto::NonOsservabile {
            motivo: "tabella IPv6 non letta".to_string(),
        };
        assert_eq!(non_osservabile.quante(), None);
        assert_eq!(
            PorteInAscolto::Contate {
                porte: 0,
                pid_distinti: 0
            }
            .quante(),
            Some(0)
        );
    }

    /// LA CATENA INTERA, su schema reale: una riga di registro la cui porta ha
    /// un listener VIVO deve risultare in ascolto.
    ///
    /// Senza questo test, a parco spento, «0 in ascolto» sarebbe indistinguibile
    /// da uno strumento cieco — ed e' esattamente la forma di difetto che la
    /// regola O descrive: la misura funziona, non tocca il suo oggetto, e non
    /// fallisce mai. Qui il listener e' reale (il SO gli assegna la porta), la
    /// riga di registro e' reale (schema dalla migrazione META, non da un
    /// `CREATE TABLE` ricopiato) e l'osservazione passa dal punto unico che usa
    /// la produzione. Rompendo `esegui` — ignorando lo scan, o attribuendo
    /// l'ascolto alla riga sbagliata — questo test rosseggia.
    ///
    /// Nella stessa esecuzione, la firma misurata su `bacheca-attivita` viene
    /// riprodotta da zero: due riscritture e due unit condivise. Cosi' i due
    /// numeri del censimento restano verificabili anche quando il DB di sviluppo
    /// sara' stato ripulito, che e' precisamente lo scopo del passo P8.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn una_porta_con_listener_vivo_risulta_in_ascolto(pool: PgPool) {
        let (_utente, progetto) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        // Il nome del progetto e' cio' da cui la produzione deriva lo slug delle
        // unit: si passa da li', non dalla stringa "progetto di test".
        let nome: String = sqlx::query_scalar("SELECT name FROM projects WHERE id = $1")
            .bind(progetto)
            .fetch_one(&pool)
            .await
            .expect("nome del progetto");
        let s = crate::project_workspace::services::project_service_slug(&nome);

        // Il listener e' vivo per tutta la durata del censimento: la porta e'
        // sua, e nessun altro processo puo' avercela tolta nel frattempo.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind di prova");
        let viva = listener.local_addr().expect("local_addr").port();

        let con_slug = format!("{s}-frontend");
        // Le altre porte servono solo a dare corpo alle righe: si prendono
        // fuori dal pool effimero e non si ascolta su nessuna.
        let righe: [(i32, &str, String); 5] = [
            // Viva e coerente.
            (
                viva as i32,
                "frontend",
                crate::project_workspace::services::service_unit_name(&s, "frontend"),
            ),
            // La firma del furto: label riscritta sopra l'unit di un altro.
            (
                20001,
                "Service",
                crate::project_workspace::services::service_unit_name(&s, "backend"),
            ),
            // La vittima, nata dopo con la stessa unit.
            (
                20002,
                "backend",
                crate::project_workspace::services::service_unit_name(&s, "backend"),
            ),
            // Secondo furto, sul frontend.
            (
                20003,
                "frontend-preview",
                crate::project_workspace::services::service_unit_name(&s, "frontend"),
            ),
            // Label che ripete lo slug: l'accumulo, che NON e' una riscrittura.
            (
                20004,
                con_slug.as_str(),
                crate::project_workspace::services::service_unit_name(&s, &con_slug),
            ),
        ];

        for (porta, label, unit) in &righe {
            sqlx::query(
                "INSERT INTO nexus_port_allocations \
                 (project_id, port, label, service_unit, allocation_mode) \
                 VALUES ($1, $2, $3, $4, 'adopted')",
            )
            .bind(progetto)
            .bind(porta)
            .bind(label)
            .bind(unit)
            .execute(&pool)
            .await
            .expect("insert allocazione");
        }

        let c = esegui(&pool).await.expect("censimento");
        let p = c
            .progetti
            .iter()
            .find(|p| p.project_id == progetto)
            .expect("il progetto seminato deve comparire");

        assert_eq!(p.righe, 5);

        // IL PUNTO: la porta con il listener vivo e' vista. Se lo scan non
        // arrivasse, o l'attribuzione perdesse la porta, qui ci sarebbe
        // `Some(false)` — cioe' il valore del difetto, non un errore generico.
        let riga_viva = p
            .dettaglio
            .iter()
            .find(|r| r.port == viva)
            .expect("la riga della porta viva");
        assert_eq!(
            riga_viva.in_ascolto,
            Some(true),
            "listener reale su 127.0.0.1:{viva} non attribuito alla sua riga"
        );
        assert!(
            riga_viva.listener_pid.is_some(),
            "una porta in ascolto ha un occupante da nominare"
        );
        // Il conteggio comprende la porta viva. NON si pretende `porte == 1`:
        // le altre quattro sono numeri fissi in un range che appartiene ai
        // progetti, e un servizio avviato da un'altra sessione mentre il test
        // gira le renderebbe legittimamente vive. Un assert su cio' che il test
        // non possiede misura la macchina, non il codice.
        assert!(
            matches!(p.in_ascolto, PorteInAscolto::Contate { porte, .. } if porte >= 1),
            "l'osservazione deve essere avvenuta e contare la porta viva: {:?}",
            p.in_ascolto
        );

        // Nessuna riga resta nell'ignoto: l'osservazione e' avvenuta per tutte.
        // E' la proprieta' che rende pubblicabile uno zero, e non dipende da chi
        // altro stia ascoltando su questa macchina.
        assert!(
            p.dettaglio.iter().all(|r| r.in_ascolto.is_some()),
            "una riga con `in_ascolto: None` significa che lo scan non e' arrivato"
        );

        // La firma del registro sporco, riprodotta da zero.
        assert_eq!(p.coppie_incoerenti, 2, "le due riscritture");
        assert_eq!(p.unit_condivise, 2);
        assert_eq!(
            p.righe_su_unit_condivisa, 4,
            "ogni furto coinvolge due righe: il ladro e la vittima nata dopo"
        );
        assert_eq!(p.label_generiche, 1, "'Service'");
        assert_eq!(p.label_slug_duplicato, 1);

        drop(listener);
    }

    /// LO STRUMENTO, contro il DB vero. `#[ignore]` perche' tocca il DB meta:
    ///   cargo test --bin mcp-core censimento_del_parco -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn censimento_del_parco() {
        let _ = dotenvy::dotenv();
        let url = std::env::var("DATABASE_URL")
            .expect("DATABASE_URL non impostata (ne' in ambiente ne' in .env)");
        let db = PgPool::connect(&url).await.expect("connessione al DB meta");

        let c = esegui(&db).await.expect("censimento");

        // La premessa del numero (regola O): uno «0 in ascolto» vale solo se la
        // tabella dei listener e' stata letta. Se non lo e', il censimento non
        // ha misurato lo stato e dirlo e' meglio che pubblicarlo.
        assert!(
            !c.osservazione.contains("non interrogabile"),
            "osservazione del SO non riuscita: {} — i conteggi 'in ascolto' non \
             sono pubblicabili",
            c.osservazione
        );

        eprintln!("\n===== CENSIMENTO PORTE (ADR 0042 P0(b)) =====");
        eprintln!("misurato: {}", c.misurato_il);
        eprintln!("osservazione del SO: {}", c.osservazione);
        for p in &c.progetti {
            eprintln!(
                "\n-- {} ({}) bucket {}-{} slug_unit='{}'",
                p.slug, p.project_id, p.bucket.0, p.bucket.1, p.slug_unit
            );
            eprintln!(
                "   righe={} in_ascolto={:?} riscritte={} senza_unit={} unit_condivise={} righe_su_unit_condivisa={}",
                p.righe,
                p.in_ascolto,
                p.coppie_incoerenti,
                p.coppie_senza_unit,
                p.unit_condivise,
                p.righe_su_unit_condivisa
            );
            eprintln!(
                "   label: generiche={} ancoraggio={} con_spazio={} slug_duplicato={}",
                p.label_generiche,
                p.label_di_ancoraggio,
                p.label_con_spazio,
                p.label_slug_duplicato
            );
            for r in &p.dettaglio {
                eprintln!(
                    "   {:>5} {:<40} unit={:<62} ascolta={:?} pid={:?} {:?}",
                    r.port,
                    r.label,
                    r.service_unit.as_deref().unwrap_or("(assente)"),
                    r.in_ascolto,
                    r.listener_pid,
                    r.verdetto
                );
            }
        }
        eprintln!(
            "\n----- JSON -----\n{}",
            serde_json::to_string_pretty(&c).expect("json")
        );
    }
}
