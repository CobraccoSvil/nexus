//! Prompt template: cache TTL + loader DB (punto unico, regola L / ADR 0026).
//!
//! Prima questa logica era duplicata IDENTICA in `mcp-core` e `admin-service`,
//! con due cache TTL non coordinate sulla stessa tabella `nexus_prompt_templates`
//! (rischio di prompt incoerenti tra chat e admin). Ora vive qui una volta sola:
//! la logica di scadenza e' in `nexus_cache::TtlCache`, questo modulo aggiunge la
//! specializzazione (TTL 60s, chiave->contenuto) e il caricamento dal DB.

use std::time::Duration;

use nexus_cache::TtlCache;
use sqlx::PgPool;

/// Cache dei prompt template (chiave -> contenuto) con TTL di 60 secondi.
///
/// Incapsula `TtlCache` esponendo l'API attesa dai call site esistenti
/// (`new`/`get`/`set`/`invalidate`).
#[derive(Clone, Debug)]
pub struct TemplateCache(TtlCache<String, String>);

impl TemplateCache {
    /// Crea una nuova cache con TTL di 60 secondi.
    ///
    /// # Esempi
    ///
    /// ```
    /// use nexus_types::TemplateCache;
    ///
    /// let cache = TemplateCache::new();
    /// // Chiave assente restituisce None
    /// assert!(cache.get("missing").is_none());
    /// ```
    pub fn new() -> Self {
        Self(TtlCache::new(Duration::from_secs(60)))
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.0.get(key)
    }

    pub fn set(&self, key: String, value: String) {
        self.0.insert(key, value);
    }

    pub fn invalidate(&self, key: &str) {
        self.0.invalidate(key);
    }
}

impl Default for TemplateCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Chiave del setting che seleziona la variante INGLESE dei template
/// (A/B lingua, fase 5b, mig 0726): CSV di chiavi di `nexus_prompt_templates`
/// da servire nella variante `<chiave>.en`. Vuoto = tutti i template in
/// italiano. Regola G: il flip e' un UPDATE del setting, il rollback e'
/// svuotare il CSV, niente redeploy.
pub const ENGLISH_VARIANTS_SETTING_KEY: &str = "prompt.english_variants";

/// Suffisso delle righe di variante inglese in `nexus_prompt_templates`.
pub const ENGLISH_VARIANT_SUFFIX: &str = ".en";

/// La SELECT unica sulla tabella dei template: la variante EN e quella IT
/// escono dalla stessa query, o le due strade divergerebbero al primo filtro
/// aggiunto a una sola delle due (regola L).
async fn fetch_active_content(db: &PgPool, key: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT content FROM nexus_prompt_templates WHERE key = $1 AND is_active = TRUE",
    )
    .bind(key)
    .fetch_optional(db)
    .await
}

/// La chiave e' fra quelle flippate alla variante inglese?
///
/// Legge il CSV dal punto unico dei settings (`nexus_auth::get_csv_setting`,
/// cache 60s per pool): a regime la domanda non costa un round-trip in piu'.
async fn variante_inglese_selezionata(db: &PgPool, key: &str) -> bool {
    nexus_auth::get_csv_setting(db, ENGLISH_VARIANTS_SETTING_KEY)
        .await
        .iter()
        .any(|voce| voce == key)
}

/// Carica un prompt template dal DB (singola fonte di verita').
///
/// Priorita':
/// 1. Cache in-memory (TTL 60s)
/// 2. Variante INGLESE `<chiave>.en`, SOLO se la chiave e' elencata nel CSV
///    del setting `prompt.english_variants` E la riga `.en` e' attiva
///    (A/B lingua fase 5b, mig 0726). Riga `.en` assente o illeggibile =
///    degrado DICHIARATO con WARN e si serve la riga italiana: il flip di un
///    template non migrato non puo' produrre un prompt vuoto.
/// 3. DB PostgreSQL (`nexus_prompt_templates` WHERE is_active=TRUE)
/// 4. Stringa vuota con log errore critico
///
/// CACHE: il contenuto risolto (IT o EN) e' memorizzato sotto la chiave
/// RICHIESTA, con lo stesso TTL 60s di ogni template: un flip del setting si
/// propaga entro il TTL, la stessa disciplina di ogni modifica a caldo.
///
/// Tutti i template di sistema devono essere presenti nel DB via migration.
/// Se manca un template, il log errore indica esattamente quale chiave aggiungere.
pub async fn get_template_or_default(db: &PgPool, cache: &TemplateCache, key: &str) -> String {
    if let Some(cached) = cache.get(key) {
        return cached;
    }
    if variante_inglese_selezionata(db, key).await {
        let chiave_en = format!("{key}{ENGLISH_VARIANT_SUFFIX}");
        match fetch_active_content(db, &chiave_en).await {
            Ok(Some(content)) => {
                cache.set(key.to_string(), content.clone());
                return content;
            }
            Ok(None) => tracing::warn!(
                "Variante EN selezionata per '{}' ma riga '{}' assente o disabilitata: \
                 servo la riga italiana.",
                key,
                chiave_en
            ),
            Err(e) => tracing::warn!(
                "Errore lettura variante EN '{}': {} — servo la riga italiana.",
                chiave_en,
                e
            ),
        }
    }
    match fetch_active_content(db, key).await {
        Ok(Some(content)) => {
            cache.set(key.to_string(), content.clone());
            content
        }
        Ok(None) => {
            tracing::error!(
                "PROMPT TEMPLATE MANCANTE: key='{}' non trovata in nexus_prompt_templates \
                 o disabilitata. Aggiungila tramite /admin/prompts o migration.",
                key
            );
            String::new()
        }
        Err(e) => {
            tracing::error!("Errore lettura prompt template '{}': {}", key, e);
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Le 4 chiavi dell'A/B lingua (fase 5b, mig 0726).
    const CHIAVI_AB: [&str; 4] = [
        "automation.supervisor_monitoring",
        "subagent.step_gatekeeper.base",
        "subagent.step_challenger.base",
        "system.choices_extractor",
    ];

    /// L'istruzione che le sole varianti EN del gate duale portano in coda
    /// (vincolo 6 del report: reason/evidence affiorano nei pannelli UI).
    const ISTRUZIONE_CAMPI_ITALIANI: &str =
        "Write the human-readable reason and evidence fields in Italian.";

    /// STESSA regex per entrambe le varianti (regola O): un confronto fatto
    /// con due estrattori diversi confronterebbe due idee di placeholder.
    fn placeholder_di(testo: &str) -> BTreeSet<String> {
        let re = regex::Regex::new(r"\{\{[a-zA-Z_]+\}\}").expect("regex placeholder");
        re.find_iter(testo).map(|m| m.as_str().to_string()).collect()
    }

    async fn contenuto_attivo(db: &PgPool, key: &str) -> Option<String> {
        fetch_active_content(db, key).await.expect("lettura template")
    }

    /// MUTAZIONE: togliere un placeholder (o una riga .en) dalla mig 0726 fa
    /// rosseggiare questo test — un placeholder perso e' un template che smette
    /// di riempirsi in silenzio.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn le_varianti_en_esistono_e_conservano_i_placeholder_delle_it(db: PgPool) {
        for chiave in CHIAVI_AB {
            let it = contenuto_attivo(&db, chiave)
                .await
                .unwrap_or_else(|| panic!("riga IT '{chiave}' assente"));
            let en = contenuto_attivo(&db, &format!("{chiave}{ENGLISH_VARIANT_SUFFIX}"))
                .await
                .unwrap_or_else(|| panic!("riga EN '{chiave}.en' assente o disattiva"));
            assert_eq!(
                placeholder_di(&it),
                placeholder_di(&en),
                "placeholder divergenti fra IT ed EN su '{chiave}'"
            );
        }
        // Il setting selettore nasce vuoto: default = tutto italiano.
        let valore: Option<String> =
            sqlx::query_scalar("SELECT value FROM settings WHERE key = $1")
                .bind(ENGLISH_VARIANTS_SETTING_KEY)
                .fetch_optional(&db)
                .await
                .expect("lettura setting");
        assert_eq!(valore.as_deref(), Some(""), "il selettore deve nascere vuoto");
    }

    /// MUTAZIONE: spostare l'istruzione sui campi italiani su un template non
    /// del gate (o toglierla da un giudice) fa rosseggiare questo test.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn solo_i_giudici_del_gate_ordinano_i_campi_liberi_in_italiano(db: PgPool) {
        for chiave in CHIAVI_AB {
            let en = contenuto_attivo(&db, &format!("{chiave}{ENGLISH_VARIANT_SUFFIX}"))
                .await
                .unwrap_or_else(|| panic!("riga EN '{chiave}.en' assente"));
            let e_giudice = chiave.starts_with("subagent.step_");
            assert_eq!(
                en.contains(ISTRUZIONE_CAMPI_ITALIANI),
                e_giudice,
                "istruzione campi-in-italiano fuori posto su '{chiave}.en'"
            );
        }
    }

    /// MUTAZIONE (eseguita durante lo sviluppo): spegnere la consultazione del
    /// CSV in `get_template_or_default` (ramo `variante_inglese_selezionata`)
    /// fa rosseggiare il caso "chiave nel CSV -> esce la EN".
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_selezione_segue_il_csv_e_degrada_dichiarando(db: PgPool) {
        const CHIAVE_GATE: &str = "subagent.step_gatekeeper.base";

        // 1) CSV vuoto (default della migrazione) -> esce la riga italiana.
        let testo = get_template_or_default(&db, &TemplateCache::new(), CHIAVE_GATE).await;
        assert!(
            testo.contains("Sei il GATEKEEPER"),
            "col CSV vuoto deve uscire la riga italiana"
        );

        // 2) Chiave nel CSV -> esce la variante inglese.
        sqlx::query("UPDATE settings SET value = $1 WHERE key = $2")
            .bind(CHIAVE_GATE)
            .bind(ENGLISH_VARIANTS_SETTING_KEY)
            .execute(&db)
            .await
            .expect("flip del setting");
        // La scrittura avviene con una query propria: la cache dei settings va
        // invalidata come da contratto di `invalidate_setting_cache`.
        nexus_auth::invalidate_setting_cache(&db, ENGLISH_VARIANTS_SETTING_KEY);
        let testo = get_template_or_default(&db, &TemplateCache::new(), CHIAVE_GATE).await;
        assert!(
            testo.contains("You are the GATEKEEPER"),
            "con la chiave nel CSV deve uscire la variante EN"
        );

        // 3) Chiave nel CSV ma riga .en ASSENTE -> degrado dichiarato: esce la
        //    riga italiana, nessun errore, mai un prompt vuoto.
        sqlx::query(
            "INSERT INTO nexus_prompt_templates (key, category, title, content) \
             VALUES ('test.variante_assente', 'system', 'Solo IT', 'CONTENUTO ITALIANO')",
        )
        .execute(&db)
        .await
        .expect("seed riga solo-IT");
        sqlx::query("UPDATE settings SET value = $1 WHERE key = $2")
            .bind("test.variante_assente")
            .bind(ENGLISH_VARIANTS_SETTING_KEY)
            .execute(&db)
            .await
            .expect("flip del setting");
        nexus_auth::invalidate_setting_cache(&db, ENGLISH_VARIANTS_SETTING_KEY);
        let testo =
            get_template_or_default(&db, &TemplateCache::new(), "test.variante_assente").await;
        assert_eq!(
            testo, "CONTENUTO ITALIANO",
            "riga .en assente: si degrada alla riga italiana"
        );
    }
}
