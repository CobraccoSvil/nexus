//! Il confine I/O del punto unico
//! [`nexus_agent_graph::decisions::origine_frontend`]: porta i FATTI — quali
//! porte il registro assegna a questo progetto, e che cosa risponde la radice di
//! ciascuna — e non li giudica.
//!
//! Il verdetto sta tutto nel modulo puro, dove si prova senza rete e senza DB.
//! Qui restano le tre sole cose che non possono starci: la query, la prova HTTP
//! e la traduzione del vocabolario delle label in un indizio booleano.

use std::time::Duration;

use nexus_agent_graph::decisions::origine_frontend::{
    dichiara_html, origine_di, scegli_origine, CandidataOrigine, OrigineFrontend, RispostaRadice,
};
use nexus_tool_kit::ports::allocation_authorizes_port;
use sqlx::PgPool;
use uuid::Uuid;

/// Caratteri di corpo passati al predicato HTML. Il predicato guarda l'header e
/// scende al corpo solo quando manca: bastano i primi byte, dove sta il
/// `<!DOCTYPE`.
const CORPO_MAX_CHARS: usize = 512;

/// Quante redirezioni si seguono sulla radice. Un frontend che manda `/` su
/// `/login` serve comunque una pagina, ed e' quella che il browser vedrebbe.
const REDIREZIONI_MAX: usize = 3;

/// La domanda completa: qual e' l'origine del frontend di questo progetto?
///
/// Ritorna il VERDETTO, non un `Option<String>`: chi chiama deve poter
/// distinguere «non c'e' un frontend» da «non l'ho accertato» (regola Q), e un
/// `None` le confonderebbe proprio dove il gate decide se ha misurato o se ha
/// rinunciato.
///
/// `timeout_s` e' il timeout delle prove del gate
/// (`agent.final_gate.endpoint_timeout_seconds`, mig 0455): la stessa pazienza
/// che il gate concede alle sue altre chiamate HTTP, letta dal DB dal chiamante
/// (regola G) invece di una costante nuova da tenere allineata.
pub(crate) async fn risolvi(db: &PgPool, project_id: Uuid, timeout_s: f64) -> OrigineFrontend {
    let righe: Vec<(i32, String, String)> = match sqlx::query_as(
        "SELECT port, label, allocation_mode FROM nexus_port_allocations \
         WHERE project_id = $1 ORDER BY port",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            // Il registro muto non e' un progetto senza frontend: e' una misura
            // che non e' avvenuta, e va detta come tale.
            return OrigineFrontend::NonAccertata {
                motivo: format!("porte del progetto non leggibili: {e}"),
            };
        }
    };

    let client = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(REDIREZIONI_MAX))
        .timeout(Duration::from_secs_f64(timeout_s.max(0.1)))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return OrigineFrontend::NonAccertata {
                motivo: format!("client HTTP delle prove non costruibile: {e}"),
            }
        }
    };

    let prove = righe.into_iter().map(|(porta, label, modo)| {
        let client = client.clone();
        async move {
            let Ok(porta) = u16::try_from(porta) else {
                // Il CHECK della mig 0114 lo rende impossibile; se accadesse,
                // la riga non e' interrogabile e non deve passare per muta.
                return CandidataOrigine {
                    porta: 0,
                    label_dice_frontend: false,
                    label,
                    risposta: RispostaRadice::NonProvata {
                        motivo: format!("porta {porta} fuori dall'intervallo TCP"),
                    },
                };
            };
            // Una porta che il progetto non e' autorizzato a usare non viene
            // ESCLUSA in silenzio: diventa una prova mancante (regola Q). Se
            // fosse scartata e nessun'altra servisse una pagina, il verdetto
            // direbbe «nessun frontend» su un progetto che non e' stato guardato
            // per intero — la stessa forma del difetto che questo modulo chiude.
            let risposta = if allocation_authorizes_port(&project_id, porta, &modo) {
                interroga_radice(&client, porta).await
            } else {
                RispostaRadice::NonProvata {
                    motivo: format!(
                        "porta {porta} fuori dal bucket del progetto e non allocata a mano \
                         (allocation_mode '{modo}'): non e' provato che sia sua"
                    ),
                }
            };
            CandidataOrigine {
                porta,
                // Indizio, non discriminante. Vocabolario SENZA contesto di
                // progetto: quello serve a non confondere due servizi FRA loro
                // (`stop_similar_running_services`), mentre qui il confronto e'
                // con una parola fissa e leggere il nome del progetto
                // costerebbe una query per un segnale che non decide nulla.
                label_dice_frontend: crate::agent_processes::similar_service_labels(
                    &label, "frontend",
                ),
                label,
                risposta,
            }
        }
    });

    let candidate: Vec<CandidataOrigine> = futures::future::join_all(prove).await;
    scegli_origine(&candidate)
}

/// Che cosa serve la RADICE di questa porta.
///
/// Client proprio e non quello di `service_recovery::probe_port`, e la
/// differenza e' nella domanda: li' si chiede «questa porta risponde?», e
/// seguire una redirezione riporterebbe lo status di un ALTRO indirizzo; qui si
/// chiede «che cosa vedrebbe un browser aperto su questa origine?», e un browser
/// le redirezioni le segue.
async fn interroga_radice(client: &reqwest::Client, porta: u16) -> RispostaRadice {
    let url = format!("{}/", origine_di(porta));
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        // Connessione rifiutata: la porta e' registrata e non c'e' nessuno. E'
        // il caso di 34853 nel difetto misurato.
        Err(e) if e.is_connect() => return RispostaRadice::Muta,
        // Tutto il resto (timeout in testa) e' un ignoto: un server lento ad
        // avviarsi non e' un progetto senza frontend.
        Err(e) => {
            return RispostaRadice::NonProvata {
                motivo: format!("{url}: {e}"),
            }
        }
    };
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let corpo = resp.text().await.unwrap_or_default();
    let inizio: String = corpo.chars().take(CORPO_MAX_CHARS).collect();
    // Le due meta' del criterio: SUCCESSO e documento HTML. Il solo HTML
    // lascerebbe passare il `Cannot GET /` di Express (404, text/html) e le
    // pagine d'eccezione di sviluppo (500, text/html).
    if status.is_success() && dichiara_html(content_type.as_deref(), &inizio) {
        RispostaRadice::Pagina {
            status: status.as_u16(),
        }
    } else {
        RispostaRadice::NonPagina {
            status: status.as_u16(),
            content_type,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_tool_kit::ports::project_bucket_range;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Byte di richiesta letti prima di rispondere: basta la request-line.
    const RICHIESTA_MAX_BYTES: usize = 2048;

    /// Server HTTP minimale: risponde a ogni connessione con lo status, il
    /// `Content-Type` e il corpo dati, poi chiude. Serve a esercitare il
    /// PRODUTTORE vero ([`risolvi`], che apre la connessione e legge gli
    /// header) invece di fabbricare le `RispostaRadice` a mano — che
    /// fisserebbe proprio l'assunto da verificare (regola O).
    fn servitore(
        listener: tokio::net::TcpListener,
        status_line: &'static str,
        content_type: &'static str,
        corpo: &'static str,
    ) {
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; RICHIESTA_MAX_BYTES];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: {content_type}\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{corpo}",
                    corpo.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
    }

    /// `n` porte LIBERE dentro il bucket del progetto, gia' occupate da un
    /// listener (chi vuole una porta MUTA droppa il suo).
    ///
    /// Nel bucket e non effimere: le allocazioni vere nascono li' con
    /// `allocation_mode = 'auto'`, e provare con `'manual'` misurerebbe un ramo
    /// dell'autorizzazione che in produzione non si prende (regola O).
    async fn porte_del_bucket(project_id: Uuid, n: usize) -> Vec<tokio::net::TcpListener> {
        let (inizio, fine) = project_bucket_range(&project_id);
        let mut libere = Vec::new();
        for p in inizio..=fine {
            if let Ok(l) = tokio::net::TcpListener::bind(("127.0.0.1", p)).await {
                libere.push(l);
                if libere.len() == n {
                    return libere;
                }
            }
        }
        panic!("servono {n} porte libere nel bucket {inizio}-{fine}, trovate {}", libere.len());
    }

    fn porta_di(l: &tokio::net::TcpListener) -> u16 {
        l.local_addr().expect("addr").port()
    }

    async fn alloca(pool: &PgPool, project_id: Uuid, porta: u16, label: &str) {
        sqlx::query(
            "INSERT INTO nexus_port_allocations (project_id, port, label, allocation_mode) \
             VALUES ($1, $2, $3, 'auto')",
        )
        .bind(project_id)
        .bind(porta as i32)
        .bind(label)
        .execute(pool)
        .await
        .expect("allocazione di prova");
    }

    /// IL difetto, riprodotto: le TRE allocazioni reali di gestione-corsi
    /// (09/08/2026) — un backend vivo, un frontend vivo, un doppione morto — con
    /// le label vere, nessuna delle quali il vocabolario riconosce.
    ///
    /// Le porte non sono i numeri originali (34894/34859/34853) ma tre porte
    /// libere del bucket del progetto seminato: legarle a numeri fissi
    /// renderebbe il test dipendente dalla macchina, e sulla postazione di
    /// sviluppo quelle tre sono occupate dal progetto vero. Cio' che il caso
    /// misurato porta — due frontend gemelli di cui uno morto, un backend, e
    /// nessuna label riconoscibile — e' conservato per intero.
    ///
    /// MUTAZIONE: rimettere il criterio vecchio (`find` sulla label che somiglia
    /// a «frontend») -> nessuna riga corrisponde, `risolvi` non trova
    /// un'origine e il test rosseggia esattamente dove il gate era cieco.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn le_tre_allocazioni_reali_risolvono_il_frontend_vivo(pool: PgPool) {
        let (_utente, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let mut porte = porte_del_bucket(project_id, 3).await;
        let morta = porte.pop().expect("terza");
        let porta_morta = porta_di(&morta);
        drop(morta);
        let fe = porte.pop().expect("seconda");
        let api = porte.pop().expect("prima");
        let porta_fe = porta_di(&fe);
        let porta_api = porta_di(&api);

        servitore(api, "200 OK", "application/json", r#"{"corsi":[]}"#);
        servitore(
            fe,
            "200 OK",
            "text/html; charset=utf-8",
            "<!DOCTYPE html><html><body>Corsi</body></html>",
        );
        alloca(&pool, project_id, porta_api, "schoolcoursesapi").await;
        alloca(&pool, project_id, porta_fe, "schoolcoursesfe").await;
        alloca(&pool, project_id, porta_morta, "school-courses-fe").await;

        // La PREMESSA del difetto, asserita e non assunta: se un giorno il
        // vocabolario imparasse «fe», questo test passerebbe per un motivo
        // diverso da quello che misura, e nessuno se ne accorgerebbe.
        for label in ["schoolcoursesapi", "schoolcoursesfe", "school-courses-fe"] {
            assert!(
                !crate::agent_processes::similar_service_labels(label, "frontend"),
                "'{label}' non e' riconosciuta dal vocabolario: e' la premessa del difetto"
            );
        }

        let esito = risolvi(&pool, project_id, 5.0).await;
        let OrigineFrontend::Trovata {
            origine,
            porta,
            label,
            ..
        } = esito.clone()
        else {
            panic!("il frontend vivo serve una pagina: {}", esito.descrizione());
        };
        assert_eq!(porta, porta_fe, "vince chi serve la pagina, non chi ha la label");
        assert_eq!(label, "schoolcoursesfe");
        assert_eq!(origine, origine_di(porta_fe));
    }

    /// Il SECONDO difetto sovrapposto: fra due candidati il vecchio `find`
    /// prendeva il primo che incontrava, senza verificare che qualcuno
    /// ascoltasse. Qui il doppione morto ha perfino la label giusta.
    ///
    /// MUTAZIONE: far precedere la label alla pagina servita -> vince la porta
    /// morta e `criteri_integrazione_frontend` costruirebbe le sue prove su
    /// un'origine che non risponde, cioe' bocciherebbe l'app per un difetto
    /// della misura.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_doppione_morto_non_vince_nemmeno_con_la_label_giusta(pool: PgPool) {
        let (_utente, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let mut porte = porte_del_bucket(project_id, 2).await;
        let morta = porte.pop().expect("seconda");
        let porta_morta = porta_di(&morta);
        drop(morta);
        let fe = porte.pop().expect("prima");
        let porta_fe = porta_di(&fe);
        servitore(fe, "200 OK", "text/html", "<!DOCTYPE html><html></html>");

        alloca(&pool, project_id, porta_morta, "frontend").await;
        alloca(&pool, project_id, porta_fe, "schoolcoursesfe").await;

        let esito = risolvi(&pool, project_id, 5.0).await;
        assert_eq!(
            esito.origine(),
            Some(origine_di(porta_fe).as_str()),
            "la label e' un indizio, l'ascolto e' il criterio: {}",
            esito.descrizione()
        );
    }

    /// Un solo backend JSON: il verdetto e' un ACCERTAMENTO e non un ignoto.
    /// E' il caso legittimo (progetto senza interfaccia) su cui il criterio non
    /// deve nascere, ed e' anche il DISCRIMINANTE che manda `static_render` a
    /// guardare un'app senza servizio.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn un_progetto_di_solo_backend_e_nessun_frontend(pool: PgPool) {
        let (_utente, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let mut porte = porte_del_bucket(project_id, 1).await;
        let api = porte.pop().expect("prima");
        let porta_api = porta_di(&api);
        servitore(api, "200 OK", "application/json", "[]");
        alloca(&pool, project_id, porta_api, "api").await;

        assert_eq!(
            risolvi(&pool, project_id, 5.0).await,
            OrigineFrontend::NessunFrontend { porte_esaminate: 1 }
        );
    }

    /// Un 404 HTML non e' una pagina servita: `Cannot GET /` di Express ha
    /// `Content-Type: text/html`, e senza la meta' «successo» del criterio un
    /// backend Express verrebbe eletto frontend del progetto.
    ///
    /// MUTAZIONE: togliere `status.is_success()` da [`interroga_radice`] ->
    /// l'origine diventa quella del backend e il test rosseggia.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_404_html_di_express_non_e_una_pagina(pool: PgPool) {
        let (_utente, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let mut porte = porte_del_bucket(project_id, 1).await;
        let api = porte.pop().expect("prima");
        let porta_api = porta_di(&api);
        servitore(api, "404 Not Found", "text/html; charset=utf-8", "Cannot GET /");
        alloca(&pool, project_id, porta_api, "api").await;

        assert_eq!(
            risolvi(&pool, project_id, 5.0).await,
            OrigineFrontend::NessunFrontend { porte_esaminate: 1 },
            "una risposta d'errore non e' una pagina servita"
        );
    }

    /// Nessuna porta registrata: il registro e' stato letto ed e' vuoto.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn senza_allocazioni_il_registro_lo_dice(pool: PgPool) {
        let (_utente, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        assert_eq!(
            risolvi(&pool, project_id, 5.0).await,
            OrigineFrontend::NessunFrontend { porte_esaminate: 0 }
        );
    }
}
