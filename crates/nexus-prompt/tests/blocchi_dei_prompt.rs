//! Il presidio della migrazione 0744: i blocchi di un prompt non spariscono in
//! silenzio.
//!
//! ## Perche' questi test vivono qui e non accanto a un modulo
//!
//! Il criterio non ha un proprietario in Rust, ed e' deliberato: la domanda
//! «quali blocchi dichiara questo contenuto?» ha UNA risposta,
//! `nexus_prompt_blocchi` (mig 0744), e vive nel DB perche' e' li' che si
//! interpone fra chi scrive e la tabella. Riscriverla in Rust sarebbe una
//! seconda idea di «blocco» (regola L). Questi test la INTERROGANO, non la
//! imitano.
//!
//! Il crate ospite e' quello che COMPONE i prompt e che gia' porta il migratore
//! META fra le proprie dev-dependencies: ogni test gira su un DB effimero
//! ricostruito dal set REALE di `db/migrations` (regola O), quindi il trigger
//! che si esercita e' esattamente quello che la produzione ricevera'.
//!
//! ## Il difetto che chiudono
//!
//! Le migrazioni 0437 e 0438 hanno eseguito `SET content = $$LINGUA: ...$$` su
//! tre prompt, cancellando i 23 blocchi che 23 migrazioni precedenti vi avevano
//! appeso. Non e' fallito niente — un blocco che non arriva al modello non fa
//! fallire nulla — e la perdita e' rimasta invisibile 48 giorni.

use sqlx::{PgPool, Row};

/// La statement REALE della 0437, parola per parola: e' l'unica forma di cui si
/// sappia con certezza che ha prodotto il danno.
const RISCRITTURA_0437: &str = "LINGUA: Rispondi SEMPRE e COMPLETAMENTE in italiano, senza eccezioni.";

/// Un prompt di prova con due blocchi e della prosa attorno.
const CHIAVE_PROVA: &str = "_test_blocchi";
const CONTENUTO_PROVA: &str =
    "testa\n<primo>corpo primo</primo>\nmezzo\n<secondo>corpo secondo</secondo>\ncoda";

async fn semina(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO nexus_prompt_templates (key, category, title, content, is_active) \
         VALUES ($1, 'system', 'prova', $2, TRUE)",
    )
    .bind(CHIAVE_PROVA)
    .bind(CONTENUTO_PROVA)
    .execute(pool)
    .await
    .expect("semina del template di prova");
}

/// I blocchi che il DB dichiara per una chiave: la domanda si pone al PUNTO
/// UNICO (`nexus_prompt_blocchi`), mai a una regexp scritta qui — una seconda
/// implementazione resterebbe verde mentre il trigger cambia idea.
async fn blocchi(pool: &PgPool, key: &str) -> Vec<String> {
    sqlx::query("SELECT nexus_prompt_blocchi(content) AS b FROM nexus_prompt_templates WHERE key = $1")
        .bind(key)
        .fetch_one(pool)
        .await
        .expect("lettura dei blocchi")
        .get::<Vec<String>, _>("b")
}

/// Il messaggio dell'errore, insieme al suo SQLSTATE.
fn guasto(e: sqlx::Error) -> (String, String) {
    let db = e.as_database_error().expect("errore del database, non di trasporto");
    (
        db.code().map(|c| c.to_string()).unwrap_or_default(),
        db.message().to_string(),
    )
}

/// IL CONTRATTO IN UNA RIGA, in forma controfattuale: se la 0744 fosse
/// esistita il 02/07/2026, la 0437 non avrebbe potuto essere applicata.
///
/// L'elenco atteso NON e' ricopiato: si chiede al DB quali blocchi
/// `agent.coder.base` porta DAVVERO dopo la 0743, e si pretende che l'errore li
/// nomini tutti. Un elenco letterale qui invecchierebbe alla prima migrazione
/// che ne aggiunge uno, e invecchiando smetterebbe di misurare.
///
/// MUTAZIONE: vedi [`senza_il_trigger_la_stessa_riscrittura_passa`], che la
/// esegue invece di descriverla.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn la_riscrittura_integrale_della_0437_e_rifiutata(pool: PgPool) {
    let prima = blocchi(&pool, "agent.coder.base").await;
    assert!(
        prima.len() >= 10,
        "premessa del test: dopo la 0743 agent.coder.base porta i blocchi ripristinati, trovati {prima:?}"
    );

    let esito = sqlx::query("UPDATE nexus_prompt_templates SET content = $1 WHERE key = 'agent.coder.base'")
        .bind(RISCRITTURA_0437)
        .execute(&pool)
        .await;

    let (codice, messaggio) = guasto(esito.expect_err("la riscrittura integrale doveva essere rifiutata"));
    assert_eq!(codice, "23000", "{messaggio}");
    assert!(messaggio.contains("agent.coder.base"), "{messaggio}");
    for tag in &prima {
        assert!(
            messaggio.contains(&format!("<{tag}>")),
            "l'errore non nomina <{tag}>, che questa scrittura avrebbe distrutto: {messaggio}"
        );
    }
    // E il prompt e' intatto: il rifiuto e' un rifiuto, non un avviso.
    assert_eq!(blocchi(&pool, "agent.coder.base").await, prima);
}

/// LA MUTAZIONE, permanente invece che raccontata: tolto il trigger, la STESSA
/// scrittura passa e i blocchi spariscono davvero.
///
/// Senza questo test, `la_riscrittura_integrale_della_0437_e_rifiutata`
/// potrebbe restare verde per una ragione qualunque — un vincolo diverso, un
/// errore di sintassi — e nessuno saprebbe che cosa sta misurando. Qui si vede
/// il difetto reale accadere, col suo valore: 10 blocchi prima, zero dopo.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn senza_il_trigger_la_stessa_riscrittura_passa(pool: PgPool) {
    let prima = blocchi(&pool, "agent.coder.base").await;
    assert!(!prima.is_empty(), "premessa: i blocchi ci sono");

    sqlx::query("DROP TRIGGER trg_prompt_blocchi_update ON nexus_prompt_templates")
        .execute(&pool)
        .await
        .expect("il trigger esiste e si chiama cosi'");

    sqlx::query("UPDATE nexus_prompt_templates SET content = $1 WHERE key = 'agent.coder.base'")
        .bind(RISCRITTURA_0437)
        .execute(&pool)
        .await
        .expect("senza presidio la riscrittura passa: e' il difetto del 02/07/2026");

    assert!(
        blocchi(&pool, "agent.coder.base").await.is_empty(),
        "e i {} blocchi sono spariti senza che niente fallisse", prima.len()
    );
}

/// La via d'uscita esiste e funziona: una rimozione VOLUTA si dichiara per nome
/// e passa. Non e' un dettaglio di comodita' — la 0137 e la 0674 hanno revocato
/// blocchi di proposito, e un presidio che lo vietasse verrebbe disattivato.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn una_rimozione_dichiarata_passa(pool: PgPool) {
    semina(&pool).await;

    let mut tx = pool.begin().await.expect("transazione");
    sqlx::query("SELECT set_config('nexus.blocchi_rimossi', $1, true)")
        .bind("primo,secondo")
        .execute(&mut *tx)
        .await
        .expect("dichiarazione");
    sqlx::query("UPDATE nexus_prompt_templates SET content = 'solo prosa' WHERE key = $1")
        .bind(CHIAVE_PROVA)
        .execute(&mut *tx)
        .await
        .expect("la rimozione dichiarata passa");
    tx.commit().await.expect("commit");

    assert!(blocchi(&pool, CHIAVE_PROVA).await.is_empty());
}

/// Dichiararne UNO su due non basta, e l'errore nomina il solo blocco non
/// dichiarato.
///
/// E' la ragione per cui la dichiarazione non ammette un jolly: un elenco che
/// assolve per intero sarebbe un interruttore, e si scriverebbe per abitudine.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn dichiararne_uno_su_due_non_basta(pool: PgPool) {
    semina(&pool).await;

    let mut tx = pool.begin().await.expect("transazione");
    sqlx::query("SELECT set_config('nexus.blocchi_rimossi', $1, true)")
        .bind("primo")
        .execute(&mut *tx)
        .await
        .expect("dichiarazione parziale");
    let esito = sqlx::query("UPDATE nexus_prompt_templates SET content = 'solo prosa' WHERE key = $1")
        .bind(CHIAVE_PROVA)
        .execute(&mut *tx)
        .await;

    let (codice, messaggio) = guasto(esito.expect_err("il secondo blocco non era dichiarato"));
    assert_eq!(codice, "23000", "{messaggio}");
    assert!(messaggio.contains("<secondo>"), "{messaggio}");
    assert!(!messaggio.contains("<primo>"), "il blocco dichiarato non e' un'accusa: {messaggio}");
}

/// APPENDERE non e' perdere: e' la forma con cui 20 migrazioni hanno costruito
/// questi prompt, e resta libera. Il criterio e' direzionale.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn appendere_non_e_perdere(pool: PgPool) {
    semina(&pool).await;

    sqlx::query("UPDATE nexus_prompt_templates SET content = content || $1 WHERE key = $2")
        .bind("\n<terzo>corpo terzo</terzo>")
        .bind(CHIAVE_PROVA)
        .execute(&pool)
        .await
        .expect("un append non perde niente");

    assert_eq!(blocchi(&pool, CHIAVE_PROVA).await, vec!["primo", "secondo", "terzo"]);
}

/// Le scritture che non toccano il contenuto passano senza nemmeno pagare il
/// criterio (clausola `WHEN` sul trigger di UPDATE). Sono la maggioranza:
/// `is_active`, `mcp_tools_json`, la promozione di una variante d'esperimento.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn le_scritture_che_non_toccano_il_contenuto_passano(pool: PgPool) {
    semina(&pool).await;

    for statement in [
        "UPDATE nexus_prompt_templates SET is_active = FALSE WHERE key = $1",
        "UPDATE nexus_prompt_templates SET is_active = TRUE, experimental = TRUE WHERE key = $1",
        "UPDATE nexus_prompt_templates SET mcp_tools_json = '[]'::jsonb WHERE key = $1",
        "UPDATE nexus_prompt_templates SET title = 'altro titolo' WHERE key = $1",
    ] {
        sqlx::query(statement)
            .bind(CHIAVE_PROVA)
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("'{statement}' non tocca il contenuto e deve passare: {e}"));
    }

    assert_eq!(blocchi(&pool, CHIAVE_PROVA).await, vec!["primo", "secondo"]);
}

/// Cancellare la riga porta via tutti i blocchi: e' una perdita come le altre.
///
/// Chiude la via del `DELETE` + `INSERT`, che riscriverebbe un template
/// scavalcando l'UPDATE. Costo su cio' che si fa gia': nessuno — in 743
/// migrazioni non c'e' un solo `DELETE FROM nexus_prompt_templates`.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn cancellare_un_template_e_perdere_i_suoi_blocchi(pool: PgPool) {
    semina(&pool).await;

    let esito = sqlx::query("DELETE FROM nexus_prompt_templates WHERE key = $1")
        .bind(CHIAVE_PROVA)
        .execute(&pool)
        .await;
    let (codice, messaggio) = guasto(esito.expect_err("un DELETE non dichiarato porta via i blocchi"));
    assert_eq!(codice, "23000", "{messaggio}");
    assert!(messaggio.contains("DELETE"), "l'errore dichiara quale scrittura: {messaggio}");
    assert!(messaggio.contains("<primo>") && messaggio.contains("<secondo>"), "{messaggio}");

    // Dichiarata, passa.
    let mut tx = pool.begin().await.expect("transazione");
    sqlx::query("SELECT set_config('nexus.blocchi_rimossi', $1, true)")
        .bind("primo,secondo")
        .execute(&mut *tx)
        .await
        .expect("dichiarazione");
    sqlx::query("DELETE FROM nexus_prompt_templates WHERE key = $1")
        .bind(CHIAVE_PROVA)
        .execute(&mut *tx)
        .await
        .expect("il DELETE dichiarato passa");
    tx.commit().await.expect("commit");
}

/// Il criterio e' il tag di CHIUSURA: sostituire un blocco con una MENZIONE
/// della sua apertura resta una perdita.
///
/// MUTAZIONE: far decidere il criterio sull'apertura e questo test passa a
/// verde per la ragione sbagliata — la menzione conterebbe come blocco, cioe'
/// esattamente il modo in cui un prompt puo' perdere una regola conservandone
/// il nome. E' la stessa trappola che la 0674 documenta e su cui la 0743 ha
/// dovuto correggere l'estrazione dal donatore.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn una_menzione_non_e_un_blocco(pool: PgPool) {
    semina(&pool).await;

    let esito = sqlx::query("UPDATE nexus_prompt_templates SET content = $1 WHERE key = $2")
        .bind("vedi il blocco <primo> piu' avanti\n<secondo>corpo secondo</secondo>")
        .bind(CHIAVE_PROVA)
        .execute(&pool)
        .await;

    let (_, messaggio) = guasto(esito.expect_err("la menzione non conserva il blocco"));
    assert!(messaggio.contains("<primo>"), "{messaggio}");
    assert!(!messaggio.contains("<secondo>"), "il secondo c'e' ancora davvero: {messaggio}");
}

/// Il criterio si interroga, non si ricopia: il punto unico risponde anche
/// fuori dal trigger, ed e' cio' che rende inutile una seconda regexp.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn il_criterio_e_interrogabile_in_un_posto_solo(pool: PgPool) {
    let casi: Vec<(&str, Vec<&str>)> = vec![
        ("<a>x</a>", vec!["a"]),
        // Menzione dell'apertura: non e' un blocco.
        ("cita <a> e basta", vec![]),
        // Ordinato e deduplicato: due occorrenze, un blocco.
        ("<b>1</b> <a>2</a> <b>3</b>", vec!["a", "b"]),
        // Il trattino e' ammesso: il riconoscimento e' permissivo di proposito,
        // perche' un tag NON sorvegliato e' una perdita silenziosa.
        ("<un-blocco>x</un-blocco>", vec!["un-blocco"]),
        ("", vec![]),
    ];
    for (contenuto, atteso) in casi {
        let letti: Vec<String> = sqlx::query_scalar("SELECT nexus_prompt_blocchi($1)")
            .bind(contenuto)
            .fetch_one(&pool)
            .await
            .expect("il punto unico risponde");
        assert_eq!(letti, atteso, "contenuto: {contenuto:?}");
    }

    // E la domanda derivata — che cosa si perde — e' direzionale.
    let persi: Vec<String> = sqlx::query_scalar("SELECT nexus_prompt_blocchi_persi($1, $2)")
        .bind("<a>1</a><b>2</b>")
        .bind("<b>2</b><c>3</c>")
        .fetch_one(&pool)
        .await
        .expect("il punto unico risponde");
    assert_eq!(persi, vec!["a"], "aggiungere <c> non e' perdere");
}

// ── mig 0745: la copertura di un blocco sul perimetro servibile ─────────────

/// Il PERIMETRO ha DUE implementazioni e la loro relazione va MISURATA.
///
/// `nexus_types::chiavi_servibili` (Rust, ESATTO) e `prompt_chiavi_servibili`
/// (SQL, per PREFISSO, quindi SOVRAINSIEME) esistono entrambe perche' il SQL
/// non puo' leggere il Rust: un guard scritto in una migrazione si appoggia al
/// secondo, i test al primo. Se il sovrainsieme smettesse di contenere l'esatto,
/// un guard SQL potrebbe passare su una riga che il runtime serve davvero — che
/// e' il difetto della 0739, dove i mandati italiani erano aggiornati e le
/// varianti `.en` no.
///
/// MUTAZIONE: togliere il ramo `key LIKE p_chiave || '.%'` da
/// `prompt_chiavi_servibili` fa cadere questa asserzione nominando le chiavi
/// `.en` che il runtime serve e il perimetro SQL non vedrebbe piu'.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn il_perimetro_sql_contiene_quello_rust(pool: PgPool) {
    let basi: Vec<String> = sqlx::query_scalar(
        "SELECT key FROM nexus_prompt_templates WHERE is_active AND key NOT LIKE '%.en' ORDER BY key",
    )
    .fetch_all(&pool)
    .await
    .expect("elenco delle chiavi base");
    assert!(basi.len() > 100, "corpus troppo piccolo: {} chiavi", basi.len());

    let mut mancanti = Vec::new();
    for base in &basi {
        let dal_sql: Vec<String> = sqlx::query_scalar("SELECT prompt_key FROM prompt_chiavi_servibili($1)")
            .bind(base)
            .fetch_all(&pool)
            .await
            .expect("perimetro SQL");
        for attesa in nexus_types::chiavi_servibili(base) {
            // Solo le righe che ESISTONO e sono attive: una variante mai
            // scritta non e' nel perimetro di nessuno dei due.
            let esiste: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM nexus_prompt_templates WHERE key = $1 AND is_active)",
            )
            .bind(&attesa)
            .fetch_one(&pool)
            .await
            .expect("esistenza");
            if esiste && !dal_sql.contains(&attesa) {
                mancanti.push(attesa);
            }
        }
    }
    assert!(
        mancanti.is_empty(),
        "il perimetro SQL non contiene righe che il runtime puo' servire: {mancanti:?}"
    );
}

/// La copertura risponde alla domanda del 18/08 sul corpus vero, e la risposta
/// e' quella che il guard LESSICALE della 0742 dava per un'altra strada.
///
/// Il valore non e' il numero: e' che il perimetro non si scrive a mano. Il
/// `8 su 8` di ieri e' falso alla prima figura aggiunta, e una figura nuova
/// entra qui da sola.
///
/// MUTAZIONE: togliere il blocco a una riga servibile nella 0742 fa cadere
/// questa asserzione nominando la chiave scoperta.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn la_copertura_risponde_dove_l_ilike_rispondeva_zero(pool: PgPool) {
    // Il perimetro della 0742: le figure che emettono `advisory_verdict`.
    let figure: Vec<String> = sqlx::query_scalar(
        "SELECT key FROM nexus_prompt_templates \
         WHERE is_active AND key LIKE 'subagent.%' AND content LIKE '%advisory_verdict%' \
         ORDER BY key",
    )
    .fetch_all(&pool)
    .await
    .expect("figure advisory");
    assert!(!figure.is_empty(), "perimetro vuoto: la verifica sarebbe vacua");

    let mut fuori = Vec::new();
    let mut righe = 0usize;
    for figura in &figure {
        let esiti: Vec<(String, String)> =
            sqlx::query_as("SELECT prompt_key, esito FROM prompt_copertura_blocco($1, $2)")
                .bind("prove_eseguibili")
                .bind(figura)
                .fetch_all(&pool)
                .await
                .expect("copertura");
        for (chiave, esito) in esiti {
            righe += 1;
            if esito != "presente" {
                fuori.push(format!("{chiave}: {esito}"));
            }
        }
    }
    assert!(righe >= figure.len(), "meno righe servibili delle figure");
    assert!(fuori.is_empty(), "righe servibili senza il piano di verifica: {fuori:?}");

    // E la forma esatta del falso negativo del 18/08: con lo SPAZIO al posto
    // dell'underscore, il LESSICALE risponde zero sulle stesse righe.
    let con_spazio: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM nexus_prompt_templates WHERE is_active AND content ILIKE '%prove eseguibili%'",
    )
    .fetch_one(&pool)
    .await
    .expect("conteggio lessicale");
    assert_eq!(con_spazio, 0, "la ricerca con lo spazio deve rispondere zero");
}
