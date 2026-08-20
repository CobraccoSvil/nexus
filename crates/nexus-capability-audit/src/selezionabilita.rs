//! «I modelli di questo fornitore possono essere SCELTI — e se nessuno puo',
//! qualcuno lo sta ancora misurando?»
//!
//! Punto unico (regola L) della selezionabilita' di un fornitore sotto il gate
//! di qualificazione. Terza domanda della famiglia, e distinta dalle altre due
//! per COSTRUZIONE:
//!
//!   - [`crate::copertura`] chiede se cio' che SAPPIAMO del fornitore basta a
//!     usarlo (ha una riga di capability?);
//!   - `mcp-core::provider_readiness` chiede se sappiamo che RISPONDE;
//!   - questa chiede se il sistema lo puo' SCEGLIERE, che e' un'altra cosa
//!     ancora: un fornitore sano, dichiarato e perfettamente funzionante puo'
//!     essere strutturalmente fuori dal routing perche' la sua qualificazione
//!     non e' mai arrivata a conclusione.
//!
//! # Il difetto (MISURATO il 20/08/2026 sul META vivo)
//!
//! `groq` risultava «il fornitore meno usato». Non era meno usato: era ASSENTE
//! dal routing per intent. Tutte e 14 le sue righe di `nexus_routing_matrix`
//! sono `is_active = false` — l'ultima toccata il 14/08/2026, con la nota
//! `[auto-cleanup: modello non disponibile nel catalog]` — e le 66 chiamate dei
//! tre giorni precedenti arrivavano tutte da altre strade (tier-chain dei
//! purpose, `nexus_provider_default_model`).
//!
//! La catena che le tiene spente non ha un anello rotto, e questo e' il punto:
//!
//!   1. `agent.model_qualification.enforce_routing_gate` vale `true`, quindi
//!      `load_catalog_with_gate` ammette fra i candidati del promote i soli
//!      modelli `qualification_state = 'qualified'` e non scaduti;
//!   2. groq non ne ha NESSUNO: `openai/gpt-oss-120b` e `openai/gpt-oss-20b`
//!      sono `unqualified` con `qualification_reason =
//!      'round_not_measuring:provider_saturated'` e `qualification_attempts = 4`;
//!      `qwen/qwen3.6-27b` e' `disqualified` per un tool-probe fallito;
//!   3. senza candidati il promote non riattiva nulla, e il cleanup — che gira
//!      dopo — non ha nulla da ripulire: le righe restano spente per sempre.
//!
//! Ogni anello si comporta come progettato. `RoundNotMeasuring` esiste apposta
//! per NON punire un modello che non si e' potuto guardare: lascia
//! `qualification_attempts` invariato e riprova con un backoff breve fisso
//! (~60 min), invece dell'esponenziale. E' la scelta giusta, e ha un effetto
//! che nessuno aveva misurato: su un fornitore la cui capacita' per minuto e'
//! troppo stretta perche' la batteria ci entri, quel giro non converge MAI.
//! L'ultima prova in `ai_model_probe_evidence` per groq e' del **15/07/2026**;
//! il backoff piu' recente scadeva alle 10:03 dello stesso giorno in cui questa
//! misura e' stata presa. **Trentasei giorni di tentativi che non hanno
//! misurato nulla, e nessun campo che lo dicesse.**
//!
//! # Perche' un criterio, e non una UPDATE che qualifica groq (regola H)
//!
//! Scrivere `qualification_state = 'qualified'` a mano e' la toppa: dichiara
//! misurato cio' che nessuno ha misurato, e alla prima riqualificazione
//! (`requalify_ttl_days = 30`) il sistema torna esattamente dov'era. Il difetto
//! non e' il valore di quella colonna: e' che «non si riesce a misurarlo» e
//! «l'abbiamo misurato ed e' inadatto» finissero nello stesso silenzio, con
//! rimedi OPPOSTI — il primo pretende un intervento (pacing della batteria,
//! finestra dedicata, quota), il secondo e' un'esclusione a ragion veduta che
//! non va toccata.
//!
//! # Perche' non e' una variante di `provider_readiness`
//!
//! Stessa ragione, misurata, per cui non lo e' la copertura: `classifica`
//! ritorna `Observed` appena esiste una misura di SALUTE, e groq di
//! osservazioni ne ha migliaia. Una variante di stallo nuova sarebbe
//! irraggiungibile proprio per il fornitore che il difetto riguarda. Il
//! fornitore E' sano — 53 chiamate finalizzate in tre giorni — e insieme
//! inselezionabile: due domande, due campi.
//!
//! # Portata
//!
//! Il criterio MISURA e non instrada: non riabilita righe, non qualifica
//! modelli, non tocca il gate. Rende dichiarato uno stato che oggi non ha nome,
//! perche' chi guarda il pannello o il censimento sappia che quel fornitore e'
//! fuori — e per quale delle due ragioni.
//!
//! Al 20/08/2026 lo stato riguarda DUE fornitori su nove: `groq`
//! ([`ProviderSelectability::StuckUnmeasured`]) e `perplexity`, che ha 2 modelli
//! abilitati con `qualification_attempts = 0` e nessuna reason
//! ([`ProviderSelectability::AwaitingMeasurement`]) — quest'ultimo si scioglie
//! aspettando, il primo no.

use crate::copertura::ModelFact;

/// Il prefisso con cui la batteria dichiara un giro che NON ha misurato il
/// modello (`mcp-core::model_qualification`, varianti `provider_saturated` e
/// `provider_cooldown`).
///
/// Vive qui perche' e' il criterio a doverlo riconoscere, e non e' una
/// sottostringa cercata nel testo (regola M): e' l'identificatore canonico
/// (regola N) che quel produttore scrive nel campo, e si confronta come
/// PREFISSO perche' la parte dopo i due punti e' la causa, che puo' crescere.
///
/// I due lati non si possono chiamare — mcp-core e' bin-only — quindi la
/// costante e' una DICHIARAZIONE, e a tenerla onesta e' il ponte
/// `la_batteria_dichiara_il_prefisso_che_il_criterio_riconosce` in
/// `mcp-core::model_qualification`, che la confronta con cio' che il produttore
/// reale emette. Senza quel ponte, rinominare la reason lascerebbe questo
/// criterio muto con tutte le sue prove verdi (regola O).
pub const PREFISSO_ROUND_NON_MISURANTE: &str = "round_not_measuring:";

/// Un modello abilitato che il gate NON ammette: perche'.
///
/// E' la meta' per-modello del verdetto di fornitore, e sta qui perche' e'
/// l'unico posto in cui la `reason` della batteria viene interpretata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EsclusioneModello {
    /// La batteria ha speso dei giri contro il fornitore senza mai guardare il
    /// modello. Aspettare non cambia nulla: il giro successivo trovera' la
    /// stessa condizione.
    NonMisurato,
    /// Nessuno lo ha ancora guardato, e nessun giro lo ha nemmeno tentato.
    /// Si scioglie aspettando che la batteria ci arrivi.
    MaiTentato,
    /// Misurato, e non promosso. Esclusione a ragion veduta: non e' un difetto
    /// e non va spenta con un intervento.
    Misurato,
}

/// Lo stato d'ingresso della batteria: nessuno lo ha ancora promosso ne'
/// bocciato. Non e' un giudizio, e' il default della colonna.
const STATO_NON_QUALIFICATO: &str = "unqualified";

/// Come si classifica UN modello abilitato e non selezionabile.
///
/// L'ordine e' dichiarato, e nessuno dei due passi e' un dettaglio:
///
///  1. la reason NON MISURANTE precede tutto. Un giro non misurante lascia
///     `qualification_attempts` invariato PER CONTRATTO, quindi un modello
///     bloccato da settimane puo' avere `attempts = 0` esattamente come uno che
///     nessuno ha mai tentato: guardare prima il contatore li confonderebbe, e i
///     due hanno rimedi opposti;
///  2. «mai tentato» pretende anche che lo STATO sia quello d'ingresso. Una
///     qualificazione SCADUTA resta `qualified` con `valid = false`: quel
///     modello e' stato misurato, e dirgli di aspettare una prima misura
///     descriverebbe un fornitore nuovo al posto di uno da riqualificare.
fn classifica_modello(m: &ModelFact) -> EsclusioneModello {
    if m.qualification_reason
        .as_deref()
        .is_some_and(|r| r.starts_with(PREFISSO_ROUND_NON_MISURANTE))
    {
        return EsclusioneModello::NonMisurato;
    }
    if m.qualification_attempts == 0
        && m.qualification_reason.is_none()
        && m.qualification_state == STATO_NON_QUALIFICATO
    {
        return EsclusioneModello::MaiTentato;
    }
    EsclusioneModello::Misurato
}

/// Selezionabilita' di un fornitore. Vocabolario chiuso: ogni variante ha un
/// rimedio diverso, ed e' per questo che sono varianti e non un booleano
/// (regola Q).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderSelectability {
    /// Nessun modello abilitato: non c'e' nulla da selezionare, e nessuna
    /// qualificazione mancante e' un difetto.
    NothingToSelect,
    /// Il gate di qualificazione non e' applicato: la qualificazione non decide
    /// la selezione, quindi su questi fatti non c'e' verdetto da dare. E' una
    /// variante e non un `Selectable` di comodo, perche' «puo' essere scelto» e
    /// «nessuno gli sta chiedendo di essere qualificato» sono cose diverse e
    /// diventerebbero indistinguibili il giorno in cui il gate si riaccende.
    GateOff { enabled: usize },
    /// Almeno un modello abilitato ha una qualificazione valida.
    Selectable { qualified: usize, enabled: usize },
    /// Nessun modello selezionabile, e almeno uno e' fermo su giri che non lo
    /// misurano. **Non si scioglie aspettando**: e' la sola variante che
    /// pretende un intervento.
    StuckUnmeasured { stuck: usize, enabled: usize },
    /// Nessun modello selezionabile, e nessuno e' mai stato tentato: la
    /// batteria non ci e' ancora arrivata. Si scioglie aspettando.
    AwaitingMeasurement { pending: usize, enabled: usize },
    /// Nessun modello selezionabile, e la misura c'e' stata: il fornitore e'
    /// escluso a ragion veduta. Non e' un difetto.
    MeasuredNotQualified { enabled: usize },
}

impl ProviderSelectability {
    /// Identificatore canonico sul wire (regola N).
    pub fn wire(&self) -> &'static str {
        match self {
            ProviderSelectability::NothingToSelect => "nothing_to_select",
            ProviderSelectability::GateOff { .. } => "gate_off",
            ProviderSelectability::Selectable { .. } => "selectable",
            ProviderSelectability::StuckUnmeasured { .. } => "stuck_unmeasured",
            ProviderSelectability::AwaitingMeasurement { .. } => "awaiting_measurement",
            ProviderSelectability::MeasuredNotQualified { .. } => "measured_not_qualified",
        }
    }

    /// Il gate ammette almeno un modello di questo fornitore.
    ///
    /// `GateOff` risponde `true` perche' li' il gate non filtra nessuno: e' una
    /// risposta sul SISTEMA, non sulla qualificazione, ed e' la sola lettura che
    /// non mente in entrambi i regimi.
    pub fn instradabile(&self) -> bool {
        matches!(
            self,
            ProviderSelectability::GateOff { .. } | ProviderSelectability::Selectable { .. }
        )
    }

    /// `true` quando lo stato pretende un intervento umano. Solo
    /// [`ProviderSelectability::StuckUnmeasured`]: le altre o vanno bene, o si
    /// sciolgono da sole, o sono esclusioni volute. E' il campo su cui un
    /// allarme decide, mai una stringa.
    pub fn richiede_intervento(&self) -> bool {
        matches!(self, ProviderSelectability::StuckUnmeasured { .. })
    }

    /// Quanti modelli abilitati sono fermi su giri che non li misurano. `0`
    /// dove non ce ne sono: qui lo zero e' una misura, non un'assenza di misura.
    pub fn stuck(&self) -> usize {
        match self {
            ProviderSelectability::StuckUnmeasured { stuck, .. } => *stuck,
            _ => 0,
        }
    }
}

/// Classifica la selezionabilita' dai fatti di catalogo. PURA: nessun I/O,
/// nessun orologio.
///
/// `require_qualified` e' il gate REALE del routing
/// (`agent.model_qualification.enforce_routing_gate`, letto da
/// `mcp-core::orchestrator::qualification_gate`): arriva come FATTO dal
/// chiamante invece di essere riletto qui, perche' un criterio che si legge da
/// solo la propria premessa non e' piu' provabile senza DB.
///
/// Contano SOLO i modelli abilitati, per la stessa ragione della copertura: un
/// modello disabilitato non viene instradato in nessun caso, quindi la sua
/// qualificazione mancante non e' un difetto e produrrebbe un allarme che
/// nessun intervento puo' spegnere.
///
/// ORDINE DELLE RISPOSTE, e non e' arbitrario:
///   1. niente da selezionare — sotto non c'e' nulla su cui decidere;
///   2. gate spento — la qualificazione non e' la domanda;
///   3. **un fatto POSITIVO precede ogni ipotesi sul resto**: dove un modello
///      qualificato c'e', il fornitore e' selezionabile e non interessa perche'
///      gli altri non lo siano;
///   4. fra i non selezionabili, il BLOCCO precede l'attesa: basta UN modello
///      fermo perche' esista qualcosa da fare, e chiedere che lo siano tutti
///      lascerebbe muto proprio groq, che ne ha due fermi su tre.
pub fn classifica_selezionabilita(
    models: &[ModelFact],
    require_qualified: bool,
) -> ProviderSelectability {
    let abilitati: Vec<&ModelFact> = models.iter().filter(|m| m.is_enabled).collect();
    let enabled = abilitati.len();
    if enabled == 0 {
        return ProviderSelectability::NothingToSelect;
    }
    if !require_qualified {
        return ProviderSelectability::GateOff { enabled };
    }
    let qualified = abilitati.iter().filter(|m| m.qualification_valid).count();
    if qualified > 0 {
        return ProviderSelectability::Selectable { qualified, enabled };
    }

    // Nessuno e' selezionabile: si contano le DUE cause che hanno un rimedio,
    // e non basta sapere quanti modelli mancano all'appello. `Misurato` non ha
    // un contatore perche' non c'e' niente da dimensionare: e' l'esclusione a
    // ragion veduta, ed e' anche il caso che resta quando gli altri due sono a
    // zero — per questo qui non serve un terzo ramo.
    let mut stuck = 0usize;
    let mut pending = 0usize;
    for m in abilitati.iter().filter(|m| !m.qualification_valid) {
        match classifica_modello(m) {
            EsclusioneModello::NonMisurato => stuck += 1,
            EsclusioneModello::MaiTentato => pending += 1,
            EsclusioneModello::Misurato => {}
        }
    }
    if stuck > 0 {
        return ProviderSelectability::StuckUnmeasured { stuck, enabled };
    }
    if pending > 0 {
        return ProviderSelectability::AwaitingMeasurement { pending, enabled };
    }
    ProviderSelectability::MeasuredNotQualified { enabled }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modello(qualification_valid: bool, attempts: i32, reason: Option<&str>) -> ModelFact {
        ModelFact {
            is_enabled: true,
            capability_source: "auto".to_string(),
            auto_disabled_reason: None,
            ha_capability: false,
            qualification_valid,
            qualification_attempts: attempts,
            qualification_reason: reason.map(str::to_string),
            qualification_state: if qualification_valid {
                "qualified".to_string()
            } else {
                STATO_NON_QUALIFICATO.to_string()
            },
        }
    }

    fn disabilitato() -> ModelFact {
        ModelFact {
            is_enabled: false,
            ..modello(false, 0, None)
        }
    }

    /// Lo stato REALE di groq il 20/08/2026: tre modelli abilitati, nessuno
    /// qualificato, due fermi su giri non misuranti e uno squalificato da una
    /// misura vera.
    #[test]
    fn groq_e_bloccato_non_semplicemente_non_qualificato() {
        let modelli = vec![
            modello(false, 4, Some("round_not_measuring:provider_saturated")),
            modello(false, 4, Some("round_not_measuring:provider_saturated")),
            modello(false, 1, Some("tool_smoke:error_class:invalid_request")),
        ];
        let s = classifica_selezionabilita(&modelli, true);
        assert_eq!(
            s,
            ProviderSelectability::StuckUnmeasured {
                stuck: 2,
                enabled: 3
            }
        );
        assert!(s.richiede_intervento());
        assert!(!s.instradabile());
        assert_eq!(s.stuck(), 2);
    }

    /// Lo stato REALE di perplexity il 20/08/2026: due modelli abilitati mai
    /// tentati. Sembra lo stesso silenzio di groq e ha il rimedio opposto —
    /// questo si scioglie aspettando.
    #[test]
    fn perplexity_aspetta_e_non_pretende_un_intervento() {
        let s = classifica_selezionabilita(&vec![modello(false, 0, None); 2], true);
        assert_eq!(
            s,
            ProviderSelectability::AwaitingMeasurement {
                pending: 2,
                enabled: 2
            }
        );
        assert!(!s.richiede_intervento());
        assert!(!s.instradabile());
        assert_ne!(
            s.wire(),
            ProviderSelectability::StuckUnmeasured {
                stuck: 2,
                enabled: 2
            }
            .wire(),
            "i due silenzi hanno rimedi opposti: non possono avere lo stesso nome"
        );
    }

    /// MUTAZIONE (regola O): il difetto misurato e' che un modello fermo puo'
    /// avere `attempts = 0` come uno mai tentato, perche' il giro non misurante
    /// non incrementa il contatore per contratto. Un criterio che guardasse
    /// prima il contatore direbbe «aspetta» a chi non sara' mai misurato.
    #[test]
    fn il_blocco_si_riconosce_dalla_reason_non_dal_contatore() {
        let s = classifica_selezionabilita(
            &[modello(
                false,
                0,
                Some("round_not_measuring:provider_cooldown"),
            )],
            true,
        );
        assert_eq!(
            s,
            ProviderSelectability::StuckUnmeasured {
                stuck: 1,
                enabled: 1
            },
            "attempts=0 con una reason non misurante e' un blocco, non un'attesa"
        );
    }

    /// Il prefisso si confronta come tale: `provider_saturated` e
    /// `provider_cooldown` sono due cause dello stesso stato, e domani ce ne
    /// puo' essere una terza.
    #[test]
    fn entrambe_le_cause_note_del_giro_non_misurante_sono_riconosciute() {
        for causa in [
            "round_not_measuring:provider_saturated",
            "round_not_measuring:provider_cooldown",
            "round_not_measuring:causa_che_non_esiste_ancora",
        ] {
            assert_eq!(
                classifica_modello(&modello(false, 3, Some(causa))),
                EsclusioneModello::NonMisurato,
                "causa non riconosciuta: {causa}"
            );
        }
        // E il giro che HA misurato non ci cade dentro: e' l'altra meta' della
        // distinzione, ed e' quella che evita l'allarme permanente.
        assert_eq!(
            classifica_modello(&modello(false, 3, Some("inconclusive_round"))),
            EsclusioneModello::Misurato
        );
    }

    /// Una qualificazione SCADUTA e' stata misurata: va riqualificata, non
    /// attesa per la prima volta. MUTAZIONE (regola O): togliere il controllo
    /// sullo stato da `classifica_modello` fa dire «awaiting_measurement» a un
    /// fornitore che il sistema ha gia' guardato — al 20/08/2026 openai ne
    /// aveva 8 modelli abilitati in quello stato.
    #[test]
    fn una_qualificazione_scaduta_non_e_una_prima_misura_da_aspettare() {
        let scaduto = ModelFact {
            qualification_valid: false,
            qualification_state: "qualified".to_string(),
            qualification_attempts: 0,
            qualification_reason: None,
            ..modello(false, 0, None)
        };
        assert_eq!(classifica_modello(&scaduto), EsclusioneModello::Misurato);
        assert_eq!(
            classifica_selezionabilita(&[scaduto], true),
            ProviderSelectability::MeasuredNotQualified { enabled: 1 }
        );
    }

    #[test]
    fn un_solo_modello_qualificato_basta_a_rendere_selezionabile() {
        // Il fatto POSITIVO precede: non interessa perche' gli altri due non lo
        // siano, il fornitore si puo' scegliere.
        let modelli = vec![
            modello(true, 2, None),
            modello(false, 4, Some("round_not_measuring:provider_saturated")),
            modello(false, 0, None),
        ];
        let s = classifica_selezionabilita(&modelli, true);
        assert_eq!(
            s,
            ProviderSelectability::Selectable {
                qualified: 1,
                enabled: 3
            }
        );
        assert!(s.instradabile());
        assert!(!s.richiede_intervento());
        assert_eq!(s.stuck(), 0);
    }

    #[test]
    fn col_gate_spento_la_qualificazione_non_decide() {
        // Gli STESSI fatti di groq: col gate spento quei modelli il routing li
        // puo' scegliere, e dichiararli bloccati sarebbe un allarme che nessun
        // intervento spegne perche' non c'e' nulla di rotto.
        let modelli = vec![modello(false, 4, Some("round_not_measuring:provider_saturated")); 3];
        let s = classifica_selezionabilita(&modelli, false);
        assert_eq!(s, ProviderSelectability::GateOff { enabled: 3 });
        assert!(s.instradabile());
        assert!(!s.richiede_intervento());
    }

    #[test]
    fn una_squalifica_misurata_non_e_un_difetto() {
        let s = classifica_selezionabilita(
            &vec![modello(false, 5, Some("tool_smoke:error_class:invalid_request")); 2],
            true,
        );
        assert_eq!(s, ProviderSelectability::MeasuredNotQualified { enabled: 2 });
        assert!(!s.richiede_intervento());
    }

    #[test]
    fn solo_i_modelli_abilitati_contano() {
        // MUTAZIONE (regola O): senza il filtro, un fornitore con un solo
        // modello vivo e qualificato risulterebbe bloccato per colpa di righe
        // storiche che nessuno instradera' mai.
        let mut modelli = vec![modello(true, 2, None)];
        modelli.extend(vec![disabilitato(); 40]);
        assert_eq!(
            classifica_selezionabilita(&modelli, true),
            ProviderSelectability::Selectable {
                qualified: 1,
                enabled: 1
            }
        );
    }

    #[test]
    fn nessun_modello_abilitato_non_e_un_difetto() {
        let s = classifica_selezionabilita(&[disabilitato()], true);
        assert_eq!(s, ProviderSelectability::NothingToSelect);
        assert!(!s.richiede_intervento());
        assert!(!s.instradabile());
        assert_eq!(
            classifica_selezionabilita(&[], true),
            ProviderSelectability::NothingToSelect
        );
    }

    /// I fatti arrivano davvero dallo schema che le migrazioni producono, e la
    /// SCADENZA conta (regola O): un modello `qualified` con
    /// `qualification_expires_at` nel passato NON e' selezionabile, ed e'
    /// esattamente la condizione che `load_catalog_with_gate` applica ai
    /// candidati del promote. Sul META vivo il caso non e' teorico: al
    /// 20/08/2026 openai aveva 8 modelli abilitati in quello stato.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_scadenza_della_qualificazione_conta(pool: sqlx::PgPool) {
        for (modello, stato, scadenza) in [
            ("zeta-valido", "qualified", "now() + interval '30 days'"),
            ("zeta-scaduto", "qualified", "now() - interval '1 day'"),
        ] {
            sqlx::query(&format!(
                "INSERT INTO ai_price_catalog \
                    (provider, model, display_name, input_cost_per_million_tokens, \
                     output_cost_per_million_tokens, currency, is_enabled, \
                     qualification_state, qualification_expires_at) \
                 VALUES ('zeta', $1, $1, 1.0, 1.0, 'USD', true, '{stato}', {scadenza})"
            ))
            .bind(modello)
            .execute(&pool)
            .await
            .expect("seed catalog");
        }
        // Il trigger del gate 0629 respinge a `is_enabled=false` ogni riga senza
        // prova di probe: si abilitano dandogli quella prova.
        sqlx::query(
            "UPDATE ai_price_catalog \
                SET is_enabled = true, last_probe_healthy_at = NOW(), \
                    auto_disabled_reason = NULL, auto_disabled_at = NULL \
              WHERE provider = 'zeta'",
        )
        .execute(&pool)
        .await
        .expect("abilita con la prova che il gate pretende");

        let fatti = crate::copertura::carica_fatti_catalogo(&pool).await;
        let modelli = fatti.get("zeta").cloned().unwrap_or_default();
        assert_eq!(modelli.len(), 2, "premessa: due modelli abilitati");
        assert_eq!(
            classifica_selezionabilita(&modelli, true),
            ProviderSelectability::Selectable {
                qualified: 1,
                enabled: 2
            },
            "solo il non scaduto conta come qualificato"
        );

        // MUTAZIONE: scade anche l'altro. Il verdetto DEVE ribaltarsi — se
        // restasse `Selectable`, il caricamento non starebbe guardando la
        // scadenza, e il criterio direbbe instradabile un fornitore che il gate
        // del promote scarta per intero.
        sqlx::query(
            "UPDATE ai_price_catalog SET qualification_expires_at = now() - interval '1 hour' \
              WHERE provider = 'zeta'",
        )
        .execute(&pool)
        .await
        .expect("scadenza");
        let fatti = crate::copertura::carica_fatti_catalogo(&pool).await;
        let modelli = fatti.get("zeta").cloned().unwrap_or_default();
        let s = classifica_selezionabilita(&modelli, true);
        assert!(!s.instradabile(), "scaduti entrambi: nessuno selezionabile");
        assert_eq!(
            s,
            ProviderSelectability::MeasuredNotQualified { enabled: 2 },
            "una qualificazione scaduta e' stata MISURATA: non e' un blocco"
        );
    }

    #[test]
    fn i_sei_verdetti_hanno_sei_nomi_distinti() {
        let tutti = [
            ProviderSelectability::NothingToSelect,
            ProviderSelectability::GateOff { enabled: 1 },
            ProviderSelectability::Selectable {
                qualified: 1,
                enabled: 1,
            },
            ProviderSelectability::StuckUnmeasured {
                stuck: 1,
                enabled: 1,
            },
            ProviderSelectability::AwaitingMeasurement {
                pending: 1,
                enabled: 1,
            },
            ProviderSelectability::MeasuredNotQualified { enabled: 1 },
        ];
        let mut nomi: Vec<&str> = tutti.iter().map(ProviderSelectability::wire).collect();
        nomi.sort_unstable();
        let quanti = nomi.len();
        nomi.dedup();
        assert_eq!(nomi.len(), quanti, "due verdetti con lo stesso nome sul wire");
    }
}
