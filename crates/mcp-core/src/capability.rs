//! Punto unico (regola L, ADR 0024) della RISOLUZIONE a runtime dello stile di
//! `tool_choice` per una coppia `(provider, model)`.
//!
//! Lo stile (`anthropic_any` / `openai_required` / `google_function_calling_any`
//! / `openai_auto` / `none`) e' la fonte di verita' che dice all'executor se puo'
//! OBBLIGARE una tool call (`force_tool_choice = Some(true)` -> gateway emette
//! `tool_choice="required"`). Vive nel catalog: colonna `tool_choice_style` di
//! `nexus_provider_capabilities` (mig 0240), esposta dalla vista unica
//! `v_model_capabilities` (mig 0318 / ADR 0024). NESSUN nome modello hardcoded
//! (regola G): qui si mappano solo provider->stile-di-default, non modelli.
//!
//! Perche' un punto unico (regola L): prima del porting Rust il flag
//! `tool_choice_style` finiva nella `ExecutorConfig` letta dal DB; con il porting
//! `load_executor_config` ha smesso di popolarlo e il forcing e' diventato INERTE
//! per ogni provider (force_now sempre false). Centralizzando QUI la lettura, ogni
//! call site che deve sapere "questo modello supporta il force di tool_choice?"
//! delega a una sola funzione, testata una volta.
//!
//! Cache 60s via `nexus_cache::TtlCache` (punto unico cache, regola L): coerente
//! col TTL della routing matrix e degli altri letti-da-DB. Allineamento al DB
//! entro 60s, niente redeploy.

use nexus_agent_graph::decisions::tetto_output::{FattiTetto, RichiestaOutput, TettoOutput};
use nexus_cache::TtlCache;
use sqlx::PgPool;
use std::sync::OnceLock;
use std::time::Duration;

/// TTL della cache dello stile tool_choice. Allineato ai 60s di routing matrix /
/// pesi di scoring / intent classifier (nessun magic number sparso: e' lo stesso
/// orizzonte di refresh degli altri letti-da-DB di questo crate).
const TOOL_CHOICE_STYLE_TTL_SECS: u64 = 60;

/// La vista canonica (ADR 0024) gia' espone le meccaniche di chiamata derivando i
/// flag semantici dal catalog: lo `tool_choice_style` arriva da
/// `nexus_provider_capabilities` (la vista lo ripropone 1:1).
const V_MODEL_CAPABILITIES: &str = "v_model_capabilities";

static STYLE_CACHE: OnceLock<TtlCache<String, Option<String>>> = OnceLock::new();

fn style_cache() -> &'static TtlCache<String, Option<String>> {
    STYLE_CACHE.get_or_init(|| TtlCache::new(Duration::from_secs(TOOL_CHOICE_STYLE_TTL_SECS)))
}

/// Chiave di cache stabile per `(provider, model)`.
fn cache_key(provider: &str, model: &str) -> String {
    format!("{provider}\u{1f}{model}")
}

/// Stile di tool_choice di DEFAULT per-provider, risolto QUI (punto unico) quando
/// la coppia `(provider, model)` non ha una riga nella vista capability (catalog
/// non ancora sincronizzato per quel modello, o DB momentaneamente incompleto).
///
/// Mitigazione conservativa (regola G: la fonte primaria resta il DB; questo e'
/// il default-per-FAMIGLIA, non un nome modello hardcoded): i provider
/// OpenAI-compatibili (deepseek/mistral/openai e simili) usano `openai_required`,
/// anthropic usa `anthropic_any`, google usa `google_function_calling_any`. Uno
/// stile non riconosciuto -> `None`: il forcing resta OFF (nessuna regressione,
/// fail-safe identico al comportamento attuale).
pub(crate) fn default_style_for_provider(provider: &str) -> Option<&'static str> {
    match provider.trim().to_lowercase().as_str() {
        "anthropic" => Some("anthropic_any"),
        "google" | "vertex" | "vertex_ai" | "gemini" => Some("google_function_calling_any"),
        // Famiglia OpenAI-compatibile (dialetto chat/completions con
        // tool_choice="required"). deepseek e mistral parlano lo stesso dialetto.
        "openai" | "deepseek" | "mistral" | "azure_openai" | "openrouter" | "groq" | "xai" => {
            Some("openai_required")
        }
        _ => None,
    }
}

/// Legge lo `tool_choice_style` dalla vista capability per `(provider, model)`.
/// NESSUNA cache (per testabilita' isolata): il wrapper con cache e'
/// [`resolve_tool_choice_style`].
///
/// Ritorna:
/// - `Ok(Some(style))` se la riga esiste -> stile reale del catalog;
/// - `Ok(None)` se la riga NON esiste -> il chiamante applica il default
///   per-provider (vedi [`default_style_for_provider`]);
/// - `Err` se la query fallisce (DB down): il chiamante decide se ripiegare sul
///   default per-provider o lasciare il forcing OFF (qui non si maschera).
async fn fetch_tool_choice_style(
    db: &PgPool,
    provider: &str,
    model: &str,
) -> Result<Option<String>, sqlx::Error> {
    let sql = format!(
        "SELECT tool_choice_style FROM {V_MODEL_CAPABILITIES} \
          WHERE provider = $1 AND model = $2"
    );
    let style: Option<String> = sqlx::query_scalar(&sql)
        .bind(provider)
        .bind(model)
        .fetch_optional(db)
        .await?;
    Ok(style)
}

/// Risolve lo stile di tool_choice per `(provider, model)` con cache 60s.
///
/// Strategia (regola G + robustezza punto 3 del task):
///   1. lettura dal catalog (vista `v_model_capabilities`) — fonte primaria;
///   2. se la riga manca, default per-FAMIGLIA-provider (mai un nome modello);
///   3. se il DB e' irraggiungibile, default per-famiglia-provider (per non
///      rendere inerte il forcing su un blip transitorio del DB);
///   4. stile sconosciuto / provider non mappato -> `None` (forcing OFF,
///      fail-safe: nessuna regressione rispetto al comportamento attuale).
///
/// Il valore (anche `None`) e' cache-ato: un provider non-tool-capable non
/// ripaga la query a ogni iterazione del loop executor.
pub async fn resolve_tool_choice_style(db: &PgPool, provider: &str, model: &str) -> Option<String> {
    let key = cache_key(provider, model);
    if let Some(cached) = style_cache().get(&key) {
        return cached;
    }

    let resolved = match fetch_tool_choice_style(db, provider, model).await {
        Ok(Some(style)) => Some(style),
        Ok(None) => {
            // Riga assente: default per-famiglia (mig non ancora applicata per
            // questo modello, o catalog parziale). Non e' un magic fallback di
            // modello: e' il dialetto noto della famiglia provider.
            tracing::debug!(
                provider,
                model,
                "tool_choice_style assente in {V_MODEL_CAPABILITIES}: applico default per-provider"
            );
            default_style_for_provider(provider).map(str::to_string)
        }
        Err(e) => {
            // DB down: niente magic fallback di modello, ma il forcing non deve
            // diventare inerte su un blip transitorio -> default per-famiglia.
            // Non si cache-a un esito da errore: la prossima chiamata ritenta.
            tracing::warn!(
                provider,
                model,
                error = %e,
                "tool_choice_style: query capability fallita, default per-provider (non cache-ato)"
            );
            return default_style_for_provider(provider).map(str::to_string);
        }
    };

    style_cache().insert(key, resolved.clone());
    resolved
}

/// Floor/ceiling del budget di thinking bounded per i modelli a thinking OBBLIGATORIO.
/// Il floor garantisce reasoning non-degenere (>0, mai il budget 0 che gemini-3
/// rifiuta) ed e' il fail-safe se il setting manca; il ceiling evita di gonfiare
/// `maxOutputTokens` oltre il ragionevole (il gateway alza `maxOutputTokens =
/// max_tokens + budget`).
const MANDATORY_THINKING_BUDGET_FLOOR: u32 = 2048;
const MANDATORY_THINKING_BUDGET_CEIL: u32 = 24576;

/// Setting DB (regola G) del budget di thinking per i modelli 'native' (gemini-3),
/// tunabile senza redeploy (cache 60s). Mig 0581 (default 4096). Un valore piu' basso
/// = gemini-3 ragiona abbastanza da non andare vuoto ma piu' VELOCE (meno timeout).
const GEMINI_THINKING_BUDGET_KEY: &str = "orchestrator.gemini_thinking_budget";

static THINKING_DIRECTIVE_CACHE: OnceLock<TtlCache<String, Option<u32>>> = OnceLock::new();

fn thinking_directive_cache() -> &'static TtlCache<String, Option<u32>> {
    THINKING_DIRECTIVE_CACHE.get_or_init(|| TtlCache::new(Duration::from_secs(TOOL_CHOICE_STYLE_TTL_SECS)))
}

/// Legge `agentic_thinking_policy` dalla vista capability per `(provider, model)`.
/// NESSUNA cache (il wrapper con cache e' `resolve_mandatory_thinking_budget`).
async fn fetch_thinking_policy(
    db: &PgPool,
    provider: &str,
    model: &str,
) -> Result<Option<String>, sqlx::Error> {
    let sql = format!(
        "SELECT agentic_thinking_policy FROM {V_MODEL_CAPABILITIES} \
          WHERE provider = $1 AND model = $2"
    );
    sqlx::query_scalar::<_, String>(&sql)
        .bind(provider)
        .bind(model)
        .fetch_optional(db)
        .await
}

/// Parte PURA (regola L, testabile): `true` se la policy indica thinking OBBLIGATORIO
/// ('native': il modello, es. gemini-3, RIFIUTA thinkingBudget=0). Ogni altra policy
/// (o assente) -> `false` (nessun override, comportamento storico).
fn is_native_thinking(policy: Option<&str>) -> bool {
    matches!(policy, Some(p) if p.trim().eq_ignore_ascii_case("native"))
}

/// Parte PURA: il budget dal setting (stringa) clampato a `[FLOOR, CEIL]`; se assente
/// o non parsabile ricade sul FLOOR (fail-safe: budget piccolo ma >0, mai vuoto).
fn clamp_setting_thinking_budget(raw: Option<&str>) -> u32 {
    raw.and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(MANDATORY_THINKING_BUDGET_FLOOR)
        .clamp(MANDATORY_THINKING_BUDGET_FLOOR, MANDATORY_THINKING_BUDGET_CEIL)
}

/// Budget di thinking per un modello a thinking OBBLIGATORIO, con cache 60s (punto
/// unico, ADR 0024/regola L). `Some(budget)` se `agentic_thinking_policy='native'`
/// (budget dal setting DB `orchestrator.gemini_thinking_budget`, regola G, clampato),
/// `None` altrimenti. L'adapter mcp-core lo inietta in `GwThinkingConfig.mandatory`+
/// `budget_tokens` cosi' il gateway emette `Enabled(budget)` invece di
/// `DisabledForTools` (che gemini-3 rifiuta). Su DB down -> `None` (nessun override,
/// non cache-ato: la prossima chiamata ritenta; il comportamento storico non regredisce).
pub async fn resolve_mandatory_thinking_budget(
    db: &PgPool,
    provider: &str,
    model: &str,
) -> Option<u32> {
    let key = cache_key(provider, model);
    if let Some(cached) = thinking_directive_cache().get(&key) {
        return cached;
    }
    let policy = match fetch_thinking_policy(db, provider, model).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                provider,
                model,
                error = %e,
                "thinking directive: query capability fallita, nessun override (non cache-ato)"
            );
            return None;
        }
    };
    let resolved = if is_native_thinking(policy.as_deref()) {
        let raw = crate::settings::get_setting(db, GEMINI_THINKING_BUDGET_KEY)
            .await
            .ok()
            .flatten();
        Some(clamp_setting_thinking_budget(raw.as_deref()))
    } else {
        None
    };
    thinking_directive_cache().insert(key, resolved);
    resolved
}

/// I fatti E la loro provenienza: cache-are i soli fatti rimetterebbe le tre
/// risposte nello stesso silenzio a partire dal secondo turno.
static TETTO_CACHE: OnceLock<TtlCache<String, (FattiTetto, DichiarazioneTetto)>> = OnceLock::new();

fn tetto_cache() -> &'static TtlCache<String, (FattiTetto, DichiarazioneTetto)> {
    TETTO_CACHE.get_or_init(|| TtlCache::new(Duration::from_secs(TOOL_CHOICE_STYLE_TTL_SECS)))
}

/// I fatti del catalogo per la domanda del tetto di output. Nessun giudizio: il
/// criterio e' [`nexus_agent_graph::decisions::tetto_output::tetto_per`].
async fn fetch_fatti_tetto(
    db: &PgPool,
    provider: &str,
    model: &str,
) -> Result<Option<(Option<bool>, Option<i32>, Option<i32>)>, sqlx::Error> {
    let sql = format!(
        "SELECT thinking, default_max_output_tokens, max_output_tokens_hard \
           FROM {V_MODEL_CAPABILITIES} WHERE provider = $1 AND model = $2"
    );
    sqlx::query_as::<_, (Option<bool>, Option<i32>, Option<i32>)>(&sql)
        .bind(provider)
        .bind(model)
        .fetch_optional(db)
        .await
}

/// Che cosa il catalogo ha saputo dire di una coppia `(provider, model)` al
/// momento in cui si e' deciso il suo tetto di output.
///
/// E' un TIPO e non l'assenza di un valore perche' le tre risposte hanno
/// rimedi diversi e finivano tutte nello stesso silenzio: `FattiTetto::default()`
/// valeva sia «modello non dichiarato» sia «catalogo non leggibile», e da fuori
/// erano indistinguibili da «modello dichiarato che non pone limiti» (regola Q:
/// l'ignoto e' una variante, non un valore comodo).
///
/// MISURATO il 13/08/2026 sul META vivo: **37 modelli ABILITATI su 129 non
/// hanno una riga nella vista** — openrouter 17, openai 11, perplexity 3,
/// anthropic 2, google 2, groq 2 — e per tutti e 37 chi decideva il tetto
/// decideva al buio senza che nulla lo dichiarasse. La causa e' strutturale:
/// `v_model_capabilities` nasce `FROM nexus_provider_capabilities LEFT JOIN
/// ai_price_catalog`, mentre il discovery a runtime inserisce SOLO in
/// `ai_price_catalog` (`model_catalog_sync::insert_new_chat_model`) — cioe' nel
/// lato destro della join. Un modello scoperto dopo la migrazione del proprio
/// fornitore e' invisibile alla vista per costruzione.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DichiarazioneTetto {
    /// Il catalogo ha una riga per questa coppia: si decide sui fatti.
    Presente,
    /// Nessuna riga: il modello e' instradabile ma non dichiarato. Non si
    /// scioglie aspettando (nessun ciclo a runtime scrive capability), ed e'
    /// la stessa condizione che `DeclarationCoverage::richiede_intervento`
    /// riporta aggregata per fornitore.
    ModelloNonDichiarato,
    /// La query e' fallita: non e' un fatto sul modello, e' un guasto nostro.
    CatalogoNonLeggibile,
}

impl DichiarazioneTetto {
    /// `true` quando il tetto e' stato deciso senza fatti. Non e' di per se' un
    /// difetto — su un modello ignoto NON vincolare e' la scelta giusta,
    /// misurata — ma chi decide deve poterlo sapere invece di dedurlo.
    pub fn decide_al_buio(&self) -> bool {
        !matches!(self, Self::Presente)
    }
}

/// Il tetto, e su quali fatti lo si e' deciso.
#[derive(Debug, Clone, PartialEq)]
pub struct TettoRisolto {
    pub tetto: TettoOutput,
    pub dichiarazione: DichiarazioneTetto,
}

/// Il tetto di output da imporre a `(provider, model)` per ottenere `visibile`
/// token di risposta LEGGIBILE. Cache 60s.
///
/// Il chiamante dichiara solo cio' che deve vedere: il margine per il
/// ragionamento lo calcola il criterio, dai fatti del catalogo. Vedi la doc di
/// `tetto_output` per il difetto misurato che questa funzione chiude.
///
/// Su DB irraggiungibile o modello non a catalogo NON si inventa un numero: i
/// fatti restano vuoti e il criterio decide (per un modello ignoto: nessun
/// tetto, che e' il caso che non produce un vuoto fatturato). Quel «non si
/// inventa» ora e' DICHIARATO nel valore di ritorno, non solo nel commento.
pub async fn resolve_tetto_output(
    db: &PgPool,
    provider: &str,
    model: &str,
    visibile: u32,
) -> TettoRisolto {
    risolvi_richiesta(db, provider, model, &RichiestaOutput::Visibile(visibile)).await
}

/// I fatti del catalogo per questa coppia, INSIEME alla loro provenienza.
///
/// Separata da [`risolvi_richiesta`] perche' e' la meta' con l'I/O: qui si
/// legge e si classifica cio' che si e' letto, li' si decide e si mette in
/// cache. Le tre risposte nascono TUTTE qui, che e' anche l'unico modo perche'
/// nessun ramo dimentichi di dichiarare la propria.
async fn fatti_con_provenienza(
    db: &PgPool,
    provider: &str,
    model: &str,
) -> (FattiTetto, DichiarazioneTetto) {
    match fetch_fatti_tetto(db, provider, model).await {
        Ok(Some((thinking, default_out, hard))) => (
            FattiTetto {
                ragiona: thinking,
                default_output: default_out.and_then(|v| u32::try_from(v).ok()),
                massimo_fornitore: hard.and_then(|v| u32::try_from(v).ok()),
            },
            DichiarazioneTetto::Presente,
        ),
        Ok(None) => (
            FattiTetto::default(),
            DichiarazioneTetto::ModelloNonDichiarato,
        ),
        Err(e) => {
            tracing::warn!("tetto output: catalogo non leggibile per {provider}/{model}: {e}");
            (
                FattiTetto::default(),
                DichiarazioneTetto::CatalogoNonLeggibile,
            )
        }
    }
}

/// Il tetto per una richiesta qualunque (punto unico, regola L).
///
/// `TotaleDichiarato` NON interroga il catalogo: chi misura un modello non puo'
/// leggere da li' i fatti che sta derivando (regola O). E' anche l'unica strada
/// per cui un numero letterale puo' ancora raggiungere `max_tokens`, e pretende
/// un motivo scritto.
pub async fn risolvi_richiesta(
    db: &PgPool,
    provider: &str,
    model: &str,
    richiesta: &RichiestaOutput,
) -> TettoRisolto {
    if richiesta.scavalca_il_catalogo() {
        return TettoRisolto {
            tetto: richiesta.tetto(&FattiTetto::default()),
            // Il catalogo non e' stato interrogato: dire «non dichiarato»
            // accuserebbe il catalogo di un silenzio che nessuno gli ha chiesto.
            dichiarazione: DichiarazioneTetto::Presente,
        };
    }
    let key = cache_key(provider, model);
    if let Some((fatti, dichiarazione)) = tetto_cache().get(&key) {
        return TettoRisolto {
            tetto: richiesta.tetto(&fatti),
            dichiarazione,
        };
    }
    let (fatti, dichiarazione) = fatti_con_provenienza(db, provider, model).await;
    let tetto = richiesta.tetto(&fatti);
    if dichiarazione.decide_al_buio() {
        // WARN e non DEBUG: e' una decisione presa senza fatti su un modello
        // che il sistema sta instradando, e nessun ciclo la riparera' da solo.
        // La cache 60s la rende una riga per coppia per minuto, non per turno.
        tracing::warn!(
            provider,
            model,
            dichiarazione = ?dichiarazione,
            vincolato = tetto.max_tokens().is_some(),
            "tetto di output deciso senza i fatti del catalogo: il modello e' \
             abilitato ma non compare in v_model_capabilities"
        );
    }
    tetto_cache().insert(key, (fatti, dichiarazione));
    TettoRisolto {
        tetto,
        dichiarazione,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_style_anthropic() {
        assert_eq!(
            default_style_for_provider("anthropic"),
            Some("anthropic_any")
        );
        assert_eq!(
            default_style_for_provider("Anthropic"),
            Some("anthropic_any"),
            "case-insensitive"
        );
    }

    #[test]
    fn default_style_google() {
        assert_eq!(
            default_style_for_provider("google"),
            Some("google_function_calling_any")
        );
        assert_eq!(
            default_style_for_provider("vertex_ai"),
            Some("google_function_calling_any")
        );
    }

    #[test]
    fn default_style_openai_compat() {
        for p in ["openai", "deepseek", "mistral", "groq", "xai"] {
            assert_eq!(
                default_style_for_provider(p),
                Some("openai_required"),
                "{p} parla il dialetto OpenAI-compat"
            );
        }
    }

    #[test]
    fn default_style_provider_sconosciuto_none() {
        // Fail-safe: provider non mappato -> nessuno stile -> forcing OFF.
        assert_eq!(default_style_for_provider("acme_llm"), None);
        assert_eq!(default_style_for_provider(""), None);
    }

    #[test]
    fn is_native_thinking_solo_per_native() {
        assert!(is_native_thinking(Some("native")));
        assert!(is_native_thinking(Some("NATIVE")), "case-insensitive");
        assert!(is_native_thinking(Some("  native  ")), "trim");
        // Ogni altra policy / assente -> false (nessun override, storico invariato).
        assert!(!is_native_thinking(Some("disable_for_tools")));
        assert!(!is_native_thinking(Some("none")));
        assert!(!is_native_thinking(None));
    }

    #[test]
    fn clamp_setting_thinking_budget_parsa_e_clampa() {
        assert_eq!(clamp_setting_thinking_budget(Some("4096")), 4096);
        assert_eq!(clamp_setting_thinking_budget(Some("  8192  ")), 8192, "trim");
        assert_eq!(
            clamp_setting_thinking_budget(Some("100")),
            MANDATORY_THINKING_BUDGET_FLOOR,
            "sotto il floor -> floor"
        );
        assert_eq!(
            clamp_setting_thinking_budget(Some("999999")),
            MANDATORY_THINKING_BUDGET_CEIL,
            "sopra il ceil -> ceil"
        );
        // Fail-safe: assente o non parsabile -> floor (budget piccolo ma >0, mai vuoto).
        assert_eq!(
            clamp_setting_thinking_budget(Some("xyz")),
            MANDATORY_THINKING_BUDGET_FLOOR
        );
        assert_eq!(
            clamp_setting_thinking_budget(None),
            MANDATORY_THINKING_BUDGET_FLOOR
        );
    }

    #[test]
    fn cache_key_separa_provider_e_model() {
        // La chiave deve distinguere coppie diverse anche con parti che si
        // concatenerebbero ambiguamente senza separatore.
        assert_ne!(cache_key("a", "bc"), cache_key("ab", "c"));
    }

    /// Le tre risposte non sono la stessa risposta: due di esse dicono che si
    /// sta decidendo senza fatti, e i loro rimedi sono diversi (una migrazione
    /// mancante contro un DB che non risponde).
    #[test]
    fn solo_la_dichiarazione_presente_non_decide_al_buio() {
        assert!(!DichiarazioneTetto::Presente.decide_al_buio());
        assert!(DichiarazioneTetto::ModelloNonDichiarato.decide_al_buio());
        assert!(DichiarazioneTetto::CatalogoNonLeggibile.decide_al_buio());
        assert_ne!(
            DichiarazioneTetto::ModelloNonDichiarato,
            DichiarazioneTetto::CatalogoNonLeggibile,
            "collassarle e' il difetto che questo tipo chiude"
        );
    }

    /// IL CASO MISURATO IL 13/08/2026, sullo schema che le migrazioni
    /// producono: un modello ABILITATO che non ha riga di capability.
    ///
    /// E' la condizione dei 37 modelli reali (groq 2, openrouter 17, openai 11,
    /// perplexity 3, anthropic 2, google 2), e non e' un caso di laboratorio:
    /// `v_model_capabilities` nasce `FROM nexus_provider_capabilities LEFT JOIN
    /// ai_price_catalog`, mentre il discovery a runtime inserisce solo nel lato
    /// DESTRO — quindi un modello scoperto dopo la migrazione del proprio
    /// fornitore e' invisibile alla vista per costruzione.
    ///
    /// Il tetto che ne esce e' `NonVincolabile`, ed e' la scelta GIUSTA:
    /// MISURATO sull'API groq col prompt vero del supervisore, un tetto
    /// prudente su un modello ignoto produce `finish_reason=length` e un turno
    /// vuoto fatturato, mentre nessun tetto risponde `stop`. Cio' che mancava
    /// non era il vincolo: era che qualcuno sapesse di aver deciso al buio.
    ///
    /// MUTAZIONE (regola O): far ritornare `DichiarazioneTetto::Presente` anche
    /// sul ramo `Ok(None)` di `risolvi_richiesta` -> il primo assert cade, e con
    /// esso il WARN che e' l'unico segnale al momento della decisione.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn un_modello_abilitato_fuori_dalla_vista_dichiara_di_decidere_al_buio(pool: PgPool) {
        // Il trigger del gate 0629 respinge a `is_enabled=false` ogni riga senza
        // prova di probe: si abilita dandogli quella prova, come fa il catalogo
        // vero, invece di seminare lo stato finale a mano.
        sqlx::query(
            "INSERT INTO ai_price_catalog \
                (provider, model, display_name, input_cost_per_million_tokens, \
                 output_cost_per_million_tokens, currency, is_enabled, capability_source) \
             VALUES ('zeta', 'zeta-scoperto', 'zeta-scoperto', 1.0, 1.0, 'USD', true, 'auto')",
        )
        .execute(&pool)
        .await
        .expect("seed catalog");
        sqlx::query(
            "UPDATE ai_price_catalog SET is_enabled = true, last_probe_healthy_at = NOW(), \
                    auto_disabled_reason = NULL, auto_disabled_at = NULL \
              WHERE provider = 'zeta'",
        )
        .execute(&pool)
        .await
        .expect("abilita con la prova che il gate pretende");

        let risolto = resolve_tetto_output(&pool, "zeta", "zeta-scoperto", 512).await;
        assert_eq!(
            risolto.dichiarazione,
            DichiarazioneTetto::ModelloNonDichiarato,
            "il modello e' instradabile e la vista non lo conosce: va DETTO"
        );
        assert!(risolto.dichiarazione.decide_al_buio());
        assert_eq!(
            risolto.tetto.max_tokens(),
            None,
            "su fatti ignoti non si inventa un tetto: e' il tetto inventato a \
             produrre il turno vuoto fatturato"
        );

        // MUTAZIONE speculare: una coppia DICHIARATA, e per giunta dichiarata
        // come modello che ragiona. Cambia provenienza E tetto; se restasse
        // `ModelloNonDichiarato`, la lettura non starebbe guardando la vista.
        //
        // La coppia e' nuova e non la stessa di sopra perche' la cache 60s
        // terrebbe il verdetto precedente — ed e' anche il modo in cui la
        // produzione vede una riga scritta oggi.
        sqlx::query(
            "INSERT INTO ai_price_catalog \
                (provider, model, display_name, input_cost_per_million_tokens, \
                 output_cost_per_million_tokens, currency, is_enabled, \
                 capability_source, uses_thinking_mode) \
             VALUES ('zeta', 'zeta-dichiarato', 'zeta-dichiarato', 1.0, 1.0, 'USD', \
                     true, 'auto', true)",
        )
        .execute(&pool)
        .await
        .expect("seed catalog 2");
        sqlx::query(
            "INSERT INTO nexus_provider_capabilities \
                (provider, model, default_max_output_tokens, max_output_tokens_hard) \
             VALUES ('zeta', 'zeta-dichiarato', 4096, 16384)",
        )
        .execute(&pool)
        .await
        .expect("seed capability 2");

        let dichiarato = resolve_tetto_output(&pool, "zeta", "zeta-dichiarato", 512).await;
        assert_eq!(dichiarato.dichiarazione, DichiarazioneTetto::Presente);
        assert!(!dichiarato.dichiarazione.decide_al_buio());
        assert_eq!(
            dichiarato.tetto.max_tokens(),
            Some(4096),
            "un modello che RAGIONA riceve lo spazio del catalogo, non il \
             visibile: e' il margine che gli evita di finire i token pensando"
        );
    }

    /// «Pensiero IGNOTO» non e' rappresentabile, e non per la vista: e' lo
    /// SCHEMA. `ai_price_catalog.uses_thinking_mode` e' `NOT NULL DEFAULT
    /// false`, quindi un modello nasce dichiarando di non ragionare e resta
    /// cosi' finche' qualcuno non prova il contrario — e il ramo prudente del
    /// criterio (`ragiona: None` -> si tratta come se ragionasse) e'
    /// IRRAGGIUNGIBILE da questa query.
    ///
    /// La verifica va fatta sullo schema e non sul sospetto: il `COALESCE(
    /// c.uses_thinking_mode, false)` della vista (mig 0478) suggerisce che
    /// l'ignoto esista e venga appiattito li', e invece e' ridondante — a monte
    /// quel NULL non puo' nascere. Due spiegazioni diverse dello stesso
    /// comportamento, e solo una regge alla misura.
    ///
    /// E' una TRAPPOLA ARMATA, non un difetto attivo, perche' le due condizioni
    /// che la farebbero scattare non si incontrano: un modello mai classificato
    /// non ha nemmeno la riga di capability (il discovery scrive solo
    /// `ai_price_catalog`, la riga la scrivono le migrazioni), quindi cade nel
    /// ramo `ModelloNonDichiarato` del test qui sopra e NON riceve alcun tetto.
    /// Scatterebbe il giorno in cui una migrazione dichiarasse un modello senza
    /// dichiararne il pensiero: da li' in poi quel modello riceverebbe in
    /// silenzio il tetto stretto `visibile * 2` — cioe' 1024 sul supervisore,
    /// il valore delle 15 righe `degenerate_hollow`.
    ///
    /// MUTAZIONE: alzare il default della colonna a `true`, o togliere il
    /// `NOT NULL`, fa cadere questo test — che e' il punto: il giorno in cui
    /// l'ignoto diventa rappresentabile, il criterio cambia ramo e va saputo.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_pensiero_ignoto_non_e_rappresentabile_e_arriva_come_non_ragiona(pool: PgPool) {
        // PREMESSA, chiesta allo schema che le migrazioni producono: la colonna
        // non ammette NULL, quindi «non lo so» non ha dove stare.
        let nullable: String = sqlx::query_scalar(
            "SELECT is_nullable FROM information_schema.columns \
              WHERE table_name = 'ai_price_catalog' AND column_name = 'uses_thinking_mode'",
        )
        .fetch_one(&pool)
        .await
        .expect("lettura schema");
        assert_eq!(
            nullable, "NO",
            "se questa colonna diventasse nullable, l'ignoto sarebbe finalmente \
             rappresentabile e il criterio prenderebbe il ramo prudente"
        );

        sqlx::query(
            "INSERT INTO ai_price_catalog \
                (provider, model, display_name, input_cost_per_million_tokens, \
                 output_cost_per_million_tokens, currency, is_enabled, capability_source) \
             VALUES ('zeta', 'zeta-muto', 'zeta-muto', 1.0, 1.0, 'USD', true, 'auto')",
        )
        .execute(&pool)
        .await
        .expect("seed catalog");
        sqlx::query(
            "INSERT INTO nexus_provider_capabilities \
                (provider, model, default_max_output_tokens, max_output_tokens_hard) \
             VALUES ('zeta', 'zeta-muto', 4096, 16384)",
        )
        .execute(&pool)
        .await
        .expect("seed capability");

        // Nessuno ha detto nulla del pensiero, e il catalogo risponde «false».
        let grezzo: bool = sqlx::query_scalar(
            "SELECT uses_thinking_mode FROM ai_price_catalog \
              WHERE provider='zeta' AND model='zeta-muto'",
        )
        .fetch_one(&pool)
        .await
        .expect("lettura grezza");
        assert!(
            !grezzo,
            "il default della colonna afferma che il modello non ragiona"
        );

        // CONSEGUENZA: il ramo «non ragiona», cioe' il tetto stretto
        // `visibile * 2` invece dei 4096 che il catalogo dichiara.
        let risolto = resolve_tetto_output(&pool, "zeta", "zeta-muto", 512).await;
        assert_eq!(risolto.dichiarazione, DichiarazioneTetto::Presente);
        assert_eq!(
            risolto.tetto.max_tokens(),
            Some(1024),
            "un'affermazione mai verificata vale quanto una verificata, e stringe \
             il tetto senza che nulla lo dichiari"
        );
    }

    /// Chi MISURA non interroga il catalogo, e non viene percio' accusato di
    /// decidere al buio: non ha chiesto nulla a nessuno.
    ///
    /// Il pool e' volutamente una coppia che NON esiste: se `risolvi_richiesta`
    /// leggesse comunque il DB, la dichiarazione uscirebbe
    /// `ModelloNonDichiarato` e questo test cadrebbe.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_totale_dichiarato_non_interroga_il_catalogo(pool: PgPool) {
        let risolto = risolvi_richiesta(
            &pool,
            "fornitore-inesistente",
            "modello-inesistente",
            &RichiestaOutput::TotaleDichiarato {
                totale: 256,
                perche: "il probe dichiara il proprio budget",
            },
        )
        .await;
        assert_eq!(risolto.tetto.max_tokens(), Some(256));
        assert_eq!(risolto.dichiarazione, DichiarazioneTetto::Presente);
        assert!(!risolto.dichiarazione.decide_al_buio());
    }
}
