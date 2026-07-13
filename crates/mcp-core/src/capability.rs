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
/// rifiuta); il ceiling evita di gonfiare `maxOutputTokens` oltre il ragionevole
/// (il gateway alza `maxOutputTokens = max_tokens + budget`).
const MANDATORY_THINKING_BUDGET_FLOOR: u32 = 2048;
const MANDATORY_THINKING_BUDGET_CEIL: u32 = 24576;

static THINKING_DIRECTIVE_CACHE: OnceLock<TtlCache<String, Option<u32>>> = OnceLock::new();

fn thinking_directive_cache() -> &'static TtlCache<String, Option<u32>> {
    THINKING_DIRECTIVE_CACHE.get_or_init(|| TtlCache::new(Duration::from_secs(TOOL_CHOICE_STYLE_TTL_SECS)))
}

/// Legge `(agentic_thinking_policy, default_max_output_tokens)` dalla vista capability
/// e ritorna `Some(budget)` SOLO se la policy e' `'native'` (thinking OBBLIGATORIO: il
/// modello, es. gemini-3, RIFIUTA `thinkingBudget=0` e senza config spende un thinking
/// DEFAULT illimitato). Il budget deriva da `default_max_output_tokens` del catalog
/// (regola G), clampato a `[FLOOR, CEIL]`. NESSUNA cache (testabilita' isolata).
async fn fetch_mandatory_thinking_budget(
    db: &PgPool,
    provider: &str,
    model: &str,
) -> Result<Option<u32>, sqlx::Error> {
    let sql = format!(
        "SELECT agentic_thinking_policy, default_max_output_tokens FROM {V_MODEL_CAPABILITIES} \
          WHERE provider = $1 AND model = $2"
    );
    let row: Option<(String, i32)> = sqlx::query_as(&sql)
        .bind(provider)
        .bind(model)
        .fetch_optional(db)
        .await?;
    Ok(mandatory_budget_from_row(row))
}

/// Parte PURA (regola L, testabile senza DB): mappa la riga capability nel budget di
/// thinking obbligatorio. `'native'` -> `Some(default_max clampato)`; ogni altra policy
/// (o riga assente) -> `None` (nessun override, comportamento storico).
fn mandatory_budget_from_row(row: Option<(String, i32)>) -> Option<u32> {
    match row {
        Some((policy, default_max)) if policy.trim().eq_ignore_ascii_case("native") => {
            let base = u32::try_from(default_max).unwrap_or(MANDATORY_THINKING_BUDGET_FLOOR);
            Some(base.clamp(MANDATORY_THINKING_BUDGET_FLOOR, MANDATORY_THINKING_BUDGET_CEIL))
        }
        _ => None,
    }
}

/// Budget di thinking per un modello a thinking OBBLIGATORIO, con cache 60s (punto
/// unico, ADR 0024/regola L). `Some(budget)` se `agentic_thinking_policy='native'`,
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
    let resolved = match fetch_mandatory_thinking_budget(db, provider, model).await {
        Ok(v) => v,
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
    thinking_directive_cache().insert(key, resolved);
    resolved
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
    fn mandatory_budget_solo_per_native_e_clampato() {
        // policy 'native' (thinking obbligatorio) -> Some(budget) clampato.
        assert_eq!(
            mandatory_budget_from_row(Some(("native".into(), 8192))),
            Some(8192)
        );
        assert_eq!(
            mandatory_budget_from_row(Some(("NATIVE".into(), 100))),
            Some(MANDATORY_THINKING_BUDGET_FLOOR),
            "case-insensitive; sotto il floor -> floor"
        );
        assert_eq!(
            mandatory_budget_from_row(Some(("native".into(), 999_999))),
            Some(MANDATORY_THINKING_BUDGET_CEIL),
            "sopra il ceil -> ceil"
        );
        // Ogni altra policy / riga assente -> None (nessun override, storico invariato).
        assert_eq!(
            mandatory_budget_from_row(Some(("disable_for_tools".into(), 8192))),
            None
        );
        assert_eq!(mandatory_budget_from_row(Some(("none".into(), 8192))), None);
        assert_eq!(mandatory_budget_from_row(None), None);
    }

    #[test]
    fn cache_key_separa_provider_e_model() {
        // La chiave deve distinguere coppie diverse anche con parti che si
        // concatenerebbero ambiguamente senza separatore.
        assert_ne!(cache_key("a", "bc"), cache_key("ab", "c"));
    }
}
