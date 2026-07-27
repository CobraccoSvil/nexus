//! Risoluzione alias logico -> nome modello reale per (provider, tier).
//!
//! Porting fedele di `packages/llm-gateway/src/router/model-alias-resolver.ts`.
//! Carica la tabella alias da `config/model-aliases.yaml` (campo `aliases`) in
//! `ModelAliasEntry` (vedi `types.rs`, riusato come da regola L: punto unico del
//! tipo). Risolve un modello logico in nome reale, gestendo:
//!   - alias logici con voci `cloud_primary/secondary` prefissate `provider/`;
//!   - modelli diretti `provider/modello` (passthrough se stesso provider, oppure
//!     fallback cross-provider via alias `<provider>-flash-fallback` /
//!     `<provider>-fallback`);
//!   - modelli diretti senza prefisso (usati as-is);
//!   - voci on-premise (`onprem`) per il provider `vllm`.
//!
//! Regola G: nessun nome modello/provider e' hardcoded; tutti provengono dallo
//! YAML. L'unica costante e' la convenzione delle CHIAVI di fallback (suffissi
//! `-flash-fallback` / `-fallback`), che e' struttura, non un nome di modello.

use std::collections::HashMap;

use serde::Deserialize;

use crate::types::{ModelAliasEntry, SensitivityTier};

/// Documento YAML radice: `{ aliases: { <nome>: ModelAliasEntry, ... } }`.
#[derive(Debug, Clone, Deserialize)]
struct ModelAliasesFile {
    #[serde(default)]
    aliases: HashMap<String, ModelAliasEntry>,
}

/// Errore di risoluzione alias. Corrisponde al `ConfigError` lanciato dal TS:
/// un modello non risolvibile per il (provider, tier) richiesto deve escludere
/// il provider dalla chain, non produrre un fallback silenzioso (regola G/H).
#[derive(Debug, thiserror::Error)]
pub enum AliasError {
    /// Il file YAML non e' leggibile o non e' parsabile.
    #[error("caricamento alias fallito: {0}")]
    Load(String),
    /// Il modello non e' compatibile col tier richiesto (fuori da min/max).
    #[error("modello \"{model}\" non compatibile con tier {tier} (ammessi {min}-{max})")]
    TierMismatch {
        model: String,
        tier: SensitivityTier,
        min: SensitivityTier,
        max: SensitivityTier,
    },
    /// Il modello non e' disponibile per il provider richiesto.
    #[error("modello \"{model}\" non disponibile su provider \"{provider}\"")]
    Unavailable { model: String, provider: String },
}

/// Risolutore di alias modello. Immutabile dopo la costruzione (la tabella
/// alias e' di sola lettura), quindi condivisibile tra task dietro `Arc`.
#[derive(Debug, Clone)]
pub struct ModelAliasResolver {
    aliases: HashMap<String, ModelAliasEntry>,
}

impl ModelAliasResolver {
    /// Carica gli alias dal file YAML indicato.
    pub fn from_yaml_file(path: &str) -> Result<Self, AliasError> {
        let raw = std::fs::read_to_string(path).map_err(|e| AliasError::Load(e.to_string()))?;
        Self::from_yaml_str(&raw)
    }

    /// Carica gli alias da una stringa YAML (usato nei test e dal loader file).
    pub fn from_yaml_str(yaml: &str) -> Result<Self, AliasError> {
        let parsed: ModelAliasesFile =
            serde_yaml::from_str(yaml).map_err(|e| AliasError::Load(e.to_string()))?;
        Ok(Self {
            aliases: parsed.aliases,
        })
    }

    /// Risolve `logical_model` per `provider` ed `effective_tier`, ritornando il
    /// nome modello reale da inviare al provider.
    ///
    /// Riproduce passo-passo la `resolve` del TS, inclusi i rami di fallback.
    pub fn resolve(
        &self,
        logical_model: &str,
        provider: &str,
        tier: SensitivityTier,
    ) -> Result<String, AliasError> {
        let Some(entry) = self.aliases.get(logical_model) else {
            // Non e' un alias logico noto.
            if let Some((model_provider, _)) = logical_model.split_once('/') {
                if model_provider == provider {
                    // Stesso provider: rimuovi il prefisso, ritorna il resto.
                    // Il confronto lo rifa' la funzione (e' li' che vive la
                    // regola): qui resta per scegliere il RAMO, non per decidere
                    // lo strip.
                    return Ok(strip_provider_prefix(logical_model, provider));
                }
                // Provider diverso: cerca un alias di fallback cross-provider.
                let fallback_key = format!("{model_provider}-flash-fallback");
                let fallback_entry = self
                    .aliases
                    .get(&fallback_key)
                    .or_else(|| self.aliases.get(&format!("{model_provider}-fallback")));
                if let Some(fb) = fallback_entry {
                    if let Some(real) = pick_cloud_for_provider(fb, provider) {
                        return Ok(real);
                    }
                    if let Some(primary) = fb.cloud_primary.as_deref() {
                        if !primary.contains('/') {
                            return Ok(primary.to_string());
                        }
                    }
                }
                // Nessun fallback disponibile: provider escluso dalla chain.
                return Err(AliasError::Unavailable {
                    model: logical_model.to_string(),
                    provider: provider.to_string(),
                });
            }
            // Nome modello diretto senza prefisso: usalo as-is su qualsiasi provider.
            return Ok(logical_model.to_string());
        };

        // Alias logico noto: check tier prima di tutto.
        if tier < entry.min_tier || tier > entry.max_tier {
            return Err(AliasError::TierMismatch {
                model: logical_model.to_string(),
                tier,
                min: entry.min_tier,
                max: entry.max_tier,
            });
        }

        match provider {
            // Provider cloud: cerca tra cloud_primary/secondary la voce prefissata
            // `provider/`; in mancanza, accetta un cloud_primary senza prefisso
            // (provider-agnostico).
            "anthropic" | "openai" | "mistral" | "deepseek" | "google" => {
                if let Some(real) = pick_cloud_for_provider(entry, provider) {
                    return Ok(real);
                }
                if let Some(primary) = entry.cloud_primary.as_deref() {
                    if !primary.contains('/') {
                        return Ok(primary.to_string());
                    }
                }
                Err(AliasError::Unavailable {
                    model: logical_model.to_string(),
                    provider: provider.to_string(),
                })
            }
            // Provider on-premise: usa la voce `onprem`.
            "vllm" => entry
                .onprem
                .clone()
                .ok_or_else(|| AliasError::Unavailable {
                    model: logical_model.to_string(),
                    provider: provider.to_string(),
                }),
            // Provider sconosciuto: comportamento storico del TS (default branch),
            // ritorna il logico as-is.
            _ => Ok(logical_model.to_string()),
        }
    }

    /// Restituisce la voce alias grezza, se presente (`getEntry` del TS).
    pub fn get_entry(&self, logical_model: &str) -> Option<&ModelAliasEntry> {
        self.aliases.get(logical_model)
    }

    /// Elenca tutte le chiavi alias note (`listAliases` del TS).
    pub fn list_aliases(&self) -> Vec<String> {
        self.aliases.keys().cloned().collect()
    }
}

/// Rimuove il prefisso `provider/` SOLO se quel prefisso e' davvero il provider
/// di destinazione. PUNTO UNICO (regola L): e' l'unica funzione che tocca la
/// slash nel nome di un modello.
///
/// # Il nome di un modello e' OPACO
///
/// Una slash nel nome NON significa `provider/modello`: e' quella la nostra
/// convenzione interna, non una regola del mondo. Per groq e openrouter la
/// slash e' parte del NOME che il provider pubblica — `openai/gpt-oss-120b` e'
/// un modello di groq, `z-ai/glm-5.2` e' un modello di openrouter — e `openai`
/// o `z-ai` li' dentro non sono provider: sono marketing.
///
/// Misurato il 2026-07-16 contro l'API reale di groq:
///   `openai/gpt-oss-120b` -> HTTP 200 | `gpt-oss-120b` -> HTTP 404
/// Il 404 nei log del gateway non veniva da un modello inesistente nel catalog
/// (groq espone tutti e 4 i nostri): veniva da noi, che gli passavamo un nome
/// mutilato. Il chiamante di questa funzione faceva gia' il confronto giusto
/// (`if model_provider == provider`), mentre la sua copia in `routes.rs`
/// strippava alla cieca: due formulazioni della stessa regola, una sbagliata.
///
/// Il confronto e' case-insensitive perche' i nomi provider viaggiano in forme
/// diverse fra registry, catalog e richiesta.
pub(crate) fn strip_provider_prefix(model: &str, provider: &str) -> String {
    match model.split_once('/') {
        Some((prefisso, resto)) if prefisso.eq_ignore_ascii_case(provider.trim()) => {
            resto.to_string()
        }
        // Il prefisso NON e' il provider: fa parte del nome. Si passa intero.
        _ => model.to_string(),
    }
}

/// Cerca, tra `cloud_primary`/`cloud_secondary` della entry, il primo modello
/// prefissato `provider/` e ne ritorna la parte dopo il primo `/`. `None` se
/// nessuna delle due voci appartiene a `provider`.
fn pick_cloud_for_provider(entry: &ModelAliasEntry, provider: &str) -> Option<String> {
    let prefix = format!("{provider}/");
    [entry.cloud_primary.as_deref(), entry.cloud_secondary.as_deref()]
        .into_iter()
        .flatten()
        .find(|m| m.starts_with(&prefix))
        // .split("/")[1] del TS: prima componente dopo il prefisso provider.
        .and_then(|m| m.split('/').nth(1).map(|s| s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // YAML minimale autosufficiente: non dipende dal file reale on-disk, cosi'
    // i test restano idempotenti e indipendenti dall'ambiente.
    const YAML: &str = r#"
aliases:
  coder-small:
    cloud_primary: openai/gpt-4o-mini
    cloud_secondary: deepseek/deepseek-chat
    onprem: Qwen/Qwen2.5-Coder-7B-Instruct
    min_tier: 0
    max_tier: 1
  sensitive-only:
    cloud_primary: null
    cloud_secondary: null
    onprem: Qwen/Qwen2.5-72B-Instruct
    min_tier: 3
    max_tier: 3
  google-flash-fallback:
    cloud_primary: deepseek/deepseek-chat
    cloud_secondary: mistral/mistral-small-latest
    onprem: null
    min_tier: 0
    max_tier: 2
"#;

    fn resolver() -> ModelAliasResolver {
        ModelAliasResolver::from_yaml_str(YAML).expect("yaml valido")
    }

    #[test]
    fn alias_logico_risolve_per_provider_prefissato() {
        let r = resolver();
        assert_eq!(r.resolve("coder-small", "openai", 0).unwrap(), "gpt-4o-mini");
        assert_eq!(
            r.resolve("coder-small", "deepseek", 1).unwrap(),
            "deepseek-chat"
        );
    }

    #[test]
    fn alias_onprem_per_vllm() {
        let r = resolver();
        assert_eq!(
            r.resolve("sensitive-only", "vllm", 3).unwrap(),
            "Qwen/Qwen2.5-72B-Instruct"
        );
    }

    #[test]
    fn tier_fuori_range_e_errore() {
        let r = resolver();
        // coder-small ammette tier 0..1; tier 2 deve fallire.
        let err = r.resolve("coder-small", "openai", 2).unwrap_err();
        assert!(matches!(err, AliasError::TierMismatch { .. }));
    }

    #[test]
    fn modello_diretto_stesso_provider_rimuove_prefisso() {
        let r = resolver();
        assert_eq!(
            r.resolve("google/gemini-2.5-flash", "google", 0).unwrap(),
            "gemini-2.5-flash"
        );
    }

    #[test]
    fn modello_diretto_cross_provider_usa_fallback() {
        let r = resolver();
        // "google/..." richiesto su deepseek -> alias google-flash-fallback,
        // voce deepseek/deepseek-chat -> "deepseek-chat".
        assert_eq!(
            r.resolve("google/gemini-2.5-flash", "deepseek", 0).unwrap(),
            "deepseek-chat"
        );
    }

    #[test]
    fn modello_diretto_senza_prefisso_passthrough() {
        let r = resolver();
        assert_eq!(
            r.resolve("un-modello-qualsiasi", "openai", 0).unwrap(),
            "un-modello-qualsiasi"
        );
    }

    #[test]
    fn cross_provider_senza_fallback_e_errore() {
        let r = resolver();
        // "anthropic/..." su openai: non c'e' anthropic-flash-fallback nello YAML
        // di test -> errore Unavailable (provider escluso dalla chain).
        let err = r
            .resolve("anthropic/claude-x", "openai", 0)
            .unwrap_err();
        assert!(matches!(err, AliasError::Unavailable { .. }));
    }

    // ── Il nome del modello e' OPACO ────────────────────────────────────────

    /// IL CASO REALE (2026-07-16). I log del gateway mostravano 404 ripetuti su
    /// groq e la diagnosi corrente era "modello inesistente nel catalog".
    /// Verificato contro l'API vera di groq: espone 17 modelli e li contiene
    /// TUTTI e 4 i nostri. Il 404 lo producevamo noi:
    ///   `openai/gpt-oss-120b` -> HTTP 200 | `gpt-oss-120b` -> HTTP 404
    /// Su groq `openai/` non e' un provider: e' parte del nome.
    #[test]
    fn il_prefisso_che_non_e_il_provider_resta_nel_nome() {
        // groq: modelli con la slash nel nome pubblicato.
        assert_eq!(
            strip_provider_prefix("openai/gpt-oss-120b", "groq"),
            "openai/gpt-oss-120b",
            "REGRESSIONE: e' il 404 dei log. Su groq il nome va passato INTERO"
        );
        assert_eq!(
            strip_provider_prefix("openai/gpt-oss-20b", "groq"),
            "openai/gpt-oss-20b"
        );
        assert_eq!(
            strip_provider_prefix("meta-llama/llama-4-scout-17b-16e-instruct", "groq"),
            "meta-llama/llama-4-scout-17b-16e-instruct"
        );
        // openrouter: idem, ed e' il modello che il consiglio usa per le figure
        // heavy dopo la mig 0608 (agentic_index 43.1).
        assert_eq!(
            strip_provider_prefix("z-ai/glm-5.2", "openrouter"),
            "z-ai/glm-5.2"
        );
        assert_eq!(
            strip_provider_prefix("x-ai/grok-4.5", "openrouter"),
            "x-ai/grok-4.5"
        );
    }

    /// Quando il prefisso E' il provider, si toglie: e' la convenzione interna
    /// `provider/modello` con cui la routing matrix e i pin viaggiano.
    #[test]
    fn il_prefisso_che_e_il_provider_si_toglie() {
        assert_eq!(strip_provider_prefix("openai/gpt-4o", "openai"), "gpt-4o");
        assert_eq!(
            strip_provider_prefix("anthropic/claude-opus-4-7", "anthropic"),
            "claude-opus-4-7"
        );
        // Case-insensitive: i nomi provider viaggiano in forme diverse fra
        // registry, catalog e richiesta.
        assert_eq!(strip_provider_prefix("OpenAI/gpt-4o", "openai"), "gpt-4o");
        assert_eq!(strip_provider_prefix("openai/gpt-4o", " openai "), "gpt-4o");
        // Solo il PRIMO segmento: il resto del nome non si tocca.
        assert_eq!(
            strip_provider_prefix("openai/openai/strano", "openai"),
            "openai/strano"
        );
    }

    /// Senza slash non c'e' niente da togliere.
    #[test]
    fn un_nome_senza_slash_resta_se_stesso() {
        assert_eq!(strip_provider_prefix("gpt-4o", "openai"), "gpt-4o");
        assert_eq!(strip_provider_prefix("deepseek-v4-pro", "deepseek"), "deepseek-v4-pro");
        assert_eq!(strip_provider_prefix("", "openai"), "");
    }
}
