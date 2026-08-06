//! Lettura contabile: quali vincoli sono attivi e quanto e' gia' stato consumato.
//!
//! Due domande sole, una risposta ciascuna. Prima erano quattro query in due
//! crate: `read_active_quotas` + `usage_for_quotas` in mcp-core, la SELECT
//! inline di `enforce_quota` + `usage_for_scope` nel gateway.

use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::{PgExecutor, PgPool, Row};
use uuid::Uuid;

use nexus_pricing::PriceLookup;

use crate::{scrittura::listino_degradante, Identity};

/// Un vincolo attivo, nella forma in cui lo si applica.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct QuotaPolicy {
    pub scope_type: String,
    pub user_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub token_limit: Option<i64>,
    pub cost_limit: Option<f64>,
    pub valid_from: DateTime<Utc>,
    pub valid_to: DateTime<Utc>,
}

/// Quanto uno scope ha consumato nella finestra del suo vincolo.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Consumption {
    pub tokens: i64,
    pub cost: f64,
}

/// Se le righe di quota vanno bloccate per la durata della transazione.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaLock {
    /// `FOR UPDATE`: chi sta per PRENOTARE legge e scrive nella stessa
    /// transazione, e senza lock due richieste concorrenti possono passare
    /// entrambe sull'ultimo token disponibile.
    ForUpdate,
    /// Nessun lock: chi si limita a CONTROLLARE prima di chiamare (il gateway)
    /// non scrive nulla, e prendere un lock lo farebbe solo contendere con chi
    /// prenota davvero.
    None,
}

/// Le due varianti della stessa SELECT, generate da un unico testo.
///
/// Una macro invece di `format!` per un motivo preciso: il suffisso e' l'UNICA
/// differenza fra le due, e comporla a runtime produrrebbe una concatenazione di
/// stringhe che finisce in `sqlx::query` — cioe' la forma che il detector di
/// SQL-injection (ADR 0021) segnala, giustamente, senza poter sapere che qui i
/// due pezzi sono entrambi letterali. Con `concat!` la stringa e' completa a
/// compile-time e non esiste alcuna costruzione dinamica.
///
/// `cost_limit` e' `NUMERIC(18,6)` a schema (mig 0006) e il cast a `float8` NON
/// e' cosmetico: sqlx rifiuta di decodificare un `NUMERIC` in `f64` e il
/// fallimento arriva a RUNTIME, quando la riga esiste. La copia di mcp-core il
/// cast non l'aveva: finche' nessuno ha configurato una quota di COSTO la query
/// non ha mai avuto una riga da decodificare, e la divergenza non si e' vista.
/// E' il test `una_quota_di_costo_si_legge_e_si_applica` a tenerla chiusa.
macro_rules! sql_quote_attive {
    ($suffisso:literal) => {
        concat!(
            "SELECT scope_type, user_id, project_id, ",
            "       token_limit::bigint AS token_limit, ",
            "       cost_limit::float8  AS cost_limit, ",
            "       valid_from, valid_to ",
            "  FROM ai_quota_policies ",
            " WHERE is_enabled = TRUE ",
            "   AND valid_from <= NOW() ",
            "   AND valid_to > NOW() ",
            "   AND ( ",
            "         (scope_type = 'user' AND user_id = $1) OR ",
            "         (scope_type = 'project' AND project_id = $2) OR ",
            "         (scope_type = 'user_project' AND user_id = $1 AND project_id = $2) ",
            "       ) ",
            " ORDER BY scope_type ASC",
            $suffisso
        )
    };
}

const SQL_QUOTE_ATTIVE: &str = sql_quote_attive!("");
const SQL_QUOTE_ATTIVE_LOCK: &str = sql_quote_attive!(" FOR UPDATE");

/// I vincoli attivi adesso per questa identita'.
///
/// L'ordine e' deterministico (`scope_type`) perche' e' quello che decide QUALE
/// sforo viene riportato al chiamante quando piu' vincoli sono superati insieme:
/// senza `ORDER BY` lo stesso stato poteva produrre due messaggi diversi.
pub async fn active_quotas<'e, E>(
    exec: E,
    user_id: Uuid,
    project_id: Uuid,
    lock: QuotaLock,
) -> Result<Vec<QuotaPolicy>>
where
    E: PgExecutor<'e>,
{
    let sql = match lock {
        QuotaLock::ForUpdate => SQL_QUOTE_ATTIVE_LOCK,
        QuotaLock::None => SQL_QUOTE_ATTIVE,
    };
    let quotas = sqlx::query_as::<_, QuotaPolicy>(sql)
        .bind(user_id)
        .bind(project_id)
        .fetch_all(exec)
        .await?;
    Ok(quotas)
}

/// Gli scope da interrogare, nella forma di array paralleli che `UNNEST` sa
/// srotolare in righe. Estratta da [`usage_for_quotas`] per tenerne il corpo
/// leggibile: qui non c'e' alcuna decisione, solo trasposizione.
struct ScopeArrays {
    idxs: Vec<i32>,
    scope_types: Vec<String>,
    user_ids: Vec<Uuid>,
    project_ids: Vec<Uuid>,
    valid_froms: Vec<DateTime<Utc>>,
    valid_tos: Vec<DateTime<Utc>>,
}

impl ScopeArrays {
    fn from_quotas(quotas: &[QuotaPolicy]) -> Self {
        let n = quotas.len();
        let mut a = Self {
            idxs: Vec::with_capacity(n),
            scope_types: Vec::with_capacity(n),
            user_ids: Vec::with_capacity(n),
            project_ids: Vec::with_capacity(n),
            valid_froms: Vec::with_capacity(n),
            valid_tos: Vec::with_capacity(n),
        };
        for (i, q) in quotas.iter().enumerate() {
            a.idxs.push(i as i32);
            a.scope_types.push(q.scope_type.clone());
            // user_id/project_id possono essere NULL nelle quote di scope
            // opposto; il predicato per scope li usa solo quando rilevanti,
            // quindi un valore segnaposto (nil) non altera il risultato.
            a.user_ids.push(q.user_id.unwrap_or_else(Uuid::nil));
            a.project_ids.push(q.project_id.unwrap_or_else(Uuid::nil));
            a.valid_froms.push(q.valid_from);
            a.valid_tos.push(q.valid_to);
        }
        a
    }
}

/// Il consumo di OGNI vincolo, in una query sola.
///
/// Ogni quota e' identificata posizionalmente: la riga i-esima del risultato
/// corrisponde a `quotas[i]`. La semantica per-quota e' preservata esattamente:
/// predicato di scope, finestra temporale propria della quota, status IN
/// ('reserved', 'finalized'). Il LEFT JOIN garantisce una riga per ogni quota
/// anche quando non c'e' alcun consumo, come faceva il `COALESCE(SUM(...), 0)`
/// di una query per quota.
///
/// Conta `reserved` E `finalized` perche' la quota deve vedere il consumo PRIMA
/// che la chiamata parta (altrimenti N richieste concorrenti sforano tutte) e
/// DOPO che e' avvenuta. Cio' che NON deve contare e' una prenotazione
/// RILASCIATA: e' denaro che nessuno ha speso, ed e' per questo che
/// [`crate::release`] azzera gli importi invece di limitarsi a cambiare stato.
pub async fn usage_for_quotas<'e, E>(exec: E, quotas: &[QuotaPolicy]) -> Result<Vec<Consumption>>
where
    E: PgExecutor<'e>,
{
    if quotas.is_empty() {
        return Ok(Vec::new());
    }
    let a = ScopeArrays::from_quotas(quotas);

    let rows = sqlx::query(
        r#"
        WITH q AS (
            SELECT * FROM UNNEST(
                $1::int[], $2::text[], $3::uuid[], $4::uuid[],
                $5::timestamptz[], $6::timestamptz[]
            ) AS t(idx, scope_type, user_id, project_id, valid_from, valid_to)
        )
        SELECT
            q.idx AS idx,
            COALESCE(SUM(l.total_tokens), 0)::bigint AS tokens,
            COALESCE(SUM(l.total_cost), 0)::float8 AS cost
        FROM q
        LEFT JOIN ai_usage_ledger l
            ON l.status IN ('reserved', 'finalized')
           AND l.created_at >= q.valid_from
           AND l.created_at < q.valid_to
           AND (
                (q.scope_type = 'user' AND l.user_id = q.user_id)
             OR (q.scope_type = 'project' AND l.project_id = q.project_id)
             OR (q.scope_type = 'user_project'
                 AND l.user_id = q.user_id AND l.project_id = q.project_id)
           )
        GROUP BY q.idx
        ORDER BY q.idx
        "#,
    )
    .bind(&a.idxs)
    .bind(&a.scope_types)
    .bind(&a.user_ids)
    .bind(&a.project_ids)
    .bind(&a.valid_froms)
    .bind(&a.valid_tos)
    .fetch_all(exec)
    .await?;

    let mut out = vec![Consumption::default(); quotas.len()];
    for row in rows {
        let idx = row.try_get::<i32, _>("idx").unwrap_or(0) as usize;
        if let Some(slot) = out.get_mut(idx) {
            *slot = Consumption {
                tokens: row.try_get::<i64, _>("tokens").unwrap_or(0),
                cost: row.try_get::<f64, _>("cost").unwrap_or(0.0),
            };
        }
    }
    Ok(out)
}

/// Il primo vincolo superato, o `None` se la stima ci sta tutta.
///
/// Funzione PURA, separata dall'IO apposta: e' la decisione che respinge una
/// richiesta dell'utente, e va potuta verificare senza un database. L'ordine di
/// visita e' quello di [`active_quotas`], deterministico, quindi a parita' di
/// stato il motivo riportato e' sempre lo stesso.
pub(crate) fn primo_sforo(
    quotas: &[QuotaPolicy],
    consumi: &[Consumption],
    tokens_stimati: i64,
    costo_stimato: f64,
) -> Option<crate::QuotaExceeded> {
    for (quota, consumo) in quotas.iter().zip(consumi.iter()) {
        if let Some(limit) = quota.token_limit {
            if consumo.tokens.saturating_add(tokens_stimati) > limit {
                return Some(crate::QuotaExceeded {
                    scope: quota.scope_type.clone(),
                    reason: "token_limit".to_string(),
                });
            }
        }
        if let Some(limit) = quota.cost_limit {
            if consumo.cost + costo_stimato > limit {
                return Some(crate::QuotaExceeded {
                    scope: quota.scope_type.clone(),
                    reason: "cost_limit".to_string(),
                });
            }
        }
    }
    None
}

/// Verifica i vincoli SENZA prenotare: il gate di chi controlla e poi esegue,
/// ma non scrive nulla nel ledger.
///
/// Stessa decisione di [`crate::reserve`] — stesse quote, stesso conteggio del
/// consumo, stessa funzione di sforo — senza la riga e senza il lock. Erano due
/// implementazioni parallele: questa girava N+1 query (una per quota) e non
/// aveva il cast su `cost_limit`, quella che prenota faceva una query sola.
///
/// Il prezzo si legge col DEGRADO (la stima non blocca): senza listino il costo
/// stimato e' 0, cioe' la quota di COSTO non viene consumata per questa
/// chiamata. Lo zero e' dichiarato, non implicito.
pub async fn check_quota(
    db: &PgPool,
    identity: Identity,
    provider: &str,
    model: &str,
    prompt_tokens: i64,
    completion_tokens: i64,
) -> Result<()> {
    let costo_stimato = match listino_degradante(db, provider, model).await {
        PriceLookup::Priced(p) => {
            nexus_pricing::calculate_cost(&p, prompt_tokens, completion_tokens).2
        }
        PriceLookup::Unknown => {
            tracing::warn!(
                provider = %provider, model = %model,
                "quota: prezzo IGNOTO (pricing_state='unknown') -> stima costo 0, \
                 la quota di costo non viene consumata per questa chiamata"
            );
            0.0
        }
        PriceLookup::NotInCatalog => 0.0,
    };

    let quotas = active_quotas(db, identity.user_id, identity.project_id, QuotaLock::None).await?;
    if quotas.is_empty() {
        return Ok(());
    }
    let consumi = usage_for_quotas(db, &quotas).await?;
    let stimati = prompt_tokens.saturating_add(completion_tokens);
    match primo_sforo(&quotas, &consumi, stimati, costo_stimato) {
        Some(e) => Err(anyhow::Error::new(e)),
        None => Ok(()),
    }
}

/// Il consumo di UN solo scope: il caso `n = 1` di [`usage_for_quotas`].
///
/// Delega invece di avere una SQL propria — era la seconda copia della stessa
/// domanda, e girava N+1 volte, una per quota attiva.
pub async fn usage_for_scope<'e, E>(
    exec: E,
    scope_type: &str,
    user_id: Uuid,
    project_id: Uuid,
    valid_from: DateTime<Utc>,
    valid_to: DateTime<Utc>,
) -> Result<Consumption>
where
    E: PgExecutor<'e>,
{
    let quota = QuotaPolicy {
        scope_type: scope_type.to_string(),
        user_id: Some(user_id),
        project_id: Some(project_id),
        token_limit: None,
        cost_limit: None,
        valid_from,
        valid_to,
    };
    let consumi = usage_for_quotas(exec, std::slice::from_ref(&quota)).await?;
    Ok(consumi.into_iter().next().unwrap_or_default())
}

/// Quanto e' costato un INSIEME di run: la contabilita' di una conversazione.
///
/// Sta qui, e non in una query dell'endpoint che la mostra, perche' e' la stessa
/// domanda delle quote — «quanto e' stato consumato» — posta su un altro
/// perimetro. L'endpoint di sessione la rispondeva sommando i metadata dei
/// messaggi, che portano il costo del RUN PRINCIPALE del turno: tutto il lavoro
/// DELEGATO (Consiglio, review panel, ogni figura convocata) gira su sub-run con
/// run_id propri e restava fuori. MISURATO il 06/08/2026: $0.9394 dichiarati
/// contro $3.4741 reali su agenda-medica, cioe' il 72% del lavoro invisibile.
///
/// Solo `finalized`: una prenotazione aperta e' una stima, non spesa.
///
/// Elenco vuoto -> `Consumption::default()` senza interrogare il DB: `= ANY('{}')`
/// e' una somma su zero righe, e chiederla e' un giro inutile.
pub async fn usage_for_runs<'e, E>(exec: E, run_ids: &[Uuid]) -> Result<Consumption>
where
    E: PgExecutor<'e>,
{
    if run_ids.is_empty() {
        return Ok(Consumption::default());
    }
    let row = sqlx::query(
        "SELECT COALESCE(SUM(total_tokens), 0)::bigint AS tokens, \
                COALESCE(SUM(total_cost), 0.0)::float8 AS cost \
           FROM ai_usage_ledger \
          WHERE run_id = ANY($1) AND status = 'finalized'",
    )
    .bind(run_ids)
    .fetch_one(exec)
    .await?;
    Ok(Consumption {
        tokens: row.get::<i64, _>("tokens"),
        cost: row.get::<f64, _>("cost"),
    })
}

/// Il consumo di [`usage_for_runs`] ripartito per modello, dal piu' usato.
///
/// Stessa fonte e stesso filtro del totale: due query con criteri diversi
/// darebbero una ripartizione che non somma al totale che le sta sopra.
pub async fn usage_by_model_for_runs<'e, E>(
    exec: E,
    run_ids: &[Uuid],
) -> Result<Vec<(String, Consumption)>>
where
    E: PgExecutor<'e>,
{
    if run_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query(
        "SELECT provider || '/' || model AS etichetta, \
                COALESCE(SUM(total_tokens), 0)::bigint AS tokens, \
                COALESCE(SUM(total_cost), 0.0)::float8 AS cost \
           FROM ai_usage_ledger \
          WHERE run_id = ANY($1) AND status = 'finalized' \
          GROUP BY provider, model \
          ORDER BY tokens DESC",
    )
    .bind(run_ids)
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.get::<String, _>("etichetta"),
                Consumption {
                    tokens: r.get::<i64, _>("tokens"),
                    cost: r.get::<f64, _>("cost"),
                },
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn quota(scope: &str, token_limit: Option<i64>, cost_limit: Option<f64>) -> QuotaPolicy {
        QuotaPolicy {
            scope_type: scope.to_string(),
            user_id: None,
            project_id: None,
            token_limit,
            cost_limit,
            valid_from: Utc::now() - Duration::hours(1),
            valid_to: Utc::now() + Duration::hours(1),
        }
    }

    /// La decisione che respinge una richiesta, verificata senza database.
    #[test]
    fn lo_sforo_riporta_il_primo_vincolo_superato() {
        let quotas = vec![
            quota("project", Some(3000), None),
            quota("user", None, Some(1.0)),
        ];
        let consumi = vec![
            Consumption {
                tokens: 2000,
                cost: 0.0,
            },
            Consumption {
                tokens: 0,
                cost: 0.9,
            },
        ];

        // Ci sta: 2000 + 900 <= 3000 e 0.9 + 0.05 <= 1.0.
        assert!(primo_sforo(&quotas, &consumi, 900, 0.05).is_none());

        // Sfora il primo (token): il motivo dice QUALE vincolo e QUALE asse.
        let e = primo_sforo(&quotas, &consumi, 1500, 0.0).expect("token_limit superato");
        assert_eq!(e.to_string(), "quota_exceeded:project:token_limit");

        // Sfora solo il secondo (costo).
        let e = primo_sforo(&quotas, &consumi, 10, 0.5).expect("cost_limit superato");
        assert_eq!(e.to_string(), "quota_exceeded:user:cost_limit");
    }

    /// Un vincolo senza limiti non respinge nulla, e una quota in piu' rispetto
    /// ai consumi letti non manda in panico lo zip.
    #[test]
    fn senza_limiti_non_si_sfora() {
        let quotas = vec![quota("project", None, None)];
        let consumi = vec![Consumption {
            tokens: i64::MAX,
            cost: 1e9,
        }];
        assert!(primo_sforo(&quotas, &consumi, i64::MAX, 1e9).is_none());
        // Zip corto: nessuna coppia da valutare, nessun panico.
        assert!(primo_sforo(&quotas, &[], 10, 10.0).is_none());
    }

    /// Le due varianti della SELECT differiscono solo per il lock, e il cast su
    /// `cost_limit` c'e' in entrambe: e' la colonna `NUMERIC` che sqlx non sa
    /// decodificare in `f64` senza di esso.
    #[test]
    fn le_due_varianti_della_select_restano_gemelle() {
        assert_eq!(
            SQL_QUOTE_ATTIVE_LOCK,
            format!("{SQL_QUOTE_ATTIVE} FOR UPDATE"),
            "le due varianti devono venire dallo stesso testo"
        );
        for sql in [SQL_QUOTE_ATTIVE, SQL_QUOTE_ATTIVE_LOCK] {
            assert!(
                sql.contains("cost_limit::float8"),
                "senza il cast la lettura di una quota di COSTO fallisce a runtime"
            );
        }
    }

    use crate::test_support::identita;

    /// Una chiamata contabilizzata su un run, dalla strada VERA
    /// (`reserve` -> `finalize`): e' il solo percorso che valorizza `run_id`,
    /// e ricopiarne la INSERT a mano misurerebbe un'imitazione (regola O).
    async fn chiamata_su_run(
        pool: &PgPool,
        id: Identity,
        run_id: Uuid,
        provider: &str,
        model: &str,
        prompt: i64,
        completion: i64,
    ) {
        let r = crate::reserve(
            pool,
            id,
            provider,
            model,
            prompt as i32,
            completion as i32,
            serde_json::json!({}),
        )
        .await
        .expect("prenotazione");
        let usage = crate::LedgerUsage::derived(nexus_pricing::TokenUsage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        });
        crate::finalize(pool, &r, run_id, &usage)
            .await
            .expect("finalizzazione");
    }

    /// IL difetto: il lavoro delegato conta. Un padre e due figli (i sub-run
    /// delle figure convocate) devono sommare tutti e tre.
    ///
    /// MUTAZIONE: passare al totale i soli run principali -> 1000 invece di 3000,
    /// che e' la forma esatta dello scarto misurato in produzione ($0.94
    /// dichiarati su $3.47 reali).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_lavoro_dei_sub_run_entra_nel_totale(pool: PgPool) {
        let id = identita(&pool).await;
        let padre = Uuid::new_v4();
        let figlio_a = Uuid::new_v4();
        let figlio_b = Uuid::new_v4();
        chiamata_su_run(&pool, id, padre, "mistral", "m1", 900, 100).await;
        chiamata_su_run(&pool, id, figlio_a, "deepseek", "d1", 900, 100).await;
        chiamata_su_run(&pool, id, figlio_b, "openrouter", "o1", 900, 100).await;

        let solo_padre = usage_for_runs(&pool, &[padre]).await.expect("lettura");
        assert_eq!(solo_padre.tokens, 1000, "il padre da solo");

        let tutti = usage_for_runs(&pool, &[padre, figlio_a, figlio_b])
            .await
            .expect("lettura");
        assert_eq!(tutti.tokens, 3000, "il lavoro delegato non e' entrato");
    }

    /// Una prenotazione APERTA non e' spesa: entra solo cio' che e' finalizzato.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn una_prenotazione_aperta_non_e_spesa(pool: PgPool) {
        let id = identita(&pool).await;
        let run = Uuid::new_v4();
        crate::reserve(&pool, id, "mistral", "m1", 5000, 500, serde_json::json!({}))
            .await
            .expect("prenotazione mai chiusa");
        assert_eq!(
            usage_for_runs(&pool, &[run]).await.expect("lettura").tokens,
            0
        );
    }

    /// La ripartizione somma al totale che le sta sopra, e nomina i modelli usati
    /// SOLO dai figli — quelli che dai metadata del messaggio non comparivano.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_ripartizione_nomina_anche_i_modelli_dei_figli(pool: PgPool) {
        let id = identita(&pool).await;
        let padre = Uuid::new_v4();
        let figlio = Uuid::new_v4();
        chiamata_su_run(&pool, id, padre, "mistral", "m1", 1800, 200).await;
        chiamata_su_run(&pool, id, figlio, "deepseek", "d1", 450, 50).await;

        let per_modello = usage_by_model_for_runs(&pool, &[padre, figlio])
            .await
            .expect("lettura");
        assert_eq!(per_modello.len(), 2);
        assert_eq!(per_modello[0].0, "mistral/m1", "ordine per token DESC");
        assert_eq!(per_modello[1].0, "deepseek/d1");
        let somma: i64 = per_modello.iter().map(|(_, c)| c.tokens).sum();
        let totale = usage_for_runs(&pool, &[padre, figlio])
            .await
            .expect("lettura")
            .tokens;
        assert_eq!(somma, totale, "la ripartizione non somma al totale");
    }

    /// Nessun run: zero senza interrogare il DB (e senza `ANY('{}')`).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn senza_run_il_consumo_e_zero(pool: PgPool) {
        assert_eq!(
            usage_for_runs(&pool, &[]).await.expect("lettura"),
            Consumption::default()
        );
        assert!(usage_by_model_for_runs(&pool, &[])
            .await
            .expect("lettura")
            .is_empty());
    }
}
