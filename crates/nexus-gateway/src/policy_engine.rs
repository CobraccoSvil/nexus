//! Motore di policy di routing per tier di sensibilita'.
//!
//! Porting fedele di `packages/llm-gateway/src/router/policy-engine.ts`. Carica
//! la policy YAML (`config/policies/<profilo>.yaml`): profilo, routing
//! tier_0..tier_3 (primary/secondary/tertiary/fallback/blocked) e i flag DLP
//! (`features.allow_cloud_tier2/3`, `dlp_enabled`).
//!
//! Decisione di routing (`decide`):
//!   - se `dlp_enabled=false`, ogni richiesta e' trattata come tier 0;
//!   - gate cloud per-tier governato dal flag DLP (DB > YAML), non da `blocked`;
//!   - tenant override `block_cloud` e gate DLP escludono i provider cloud;
//!   - ritorna la lista ordinata dei provider per il tier effettivo.
//!
//! Override DB (regola G): i flag `dlp_allow_cloud_tier2/3` e `dlp_enabled`
//! arrivano dai `settings`, con priorita' sul valore YAML. La cache TTL e' il
//! punto unico `nexus_cache::TtlCache` (regola L): niente timer/contatori
//! manuali. Se il DB e' irraggiungibile gli override correnti restano validi
//! (fallback graceful sullo YAML / ultimi valori noti).

use std::sync::Arc;
use std::time::Duration;

use nexus_cache::TtlCache;
use serde::Deserialize;
use sqlx::PgPool;

use crate::types::SensitivityTier;

/// Policy di un singolo tier nel file YAML (`tier_N`).
#[derive(Debug, Clone, Default, Deserialize)]
struct TierPolicy {
    #[serde(default)]
    primary: Option<String>,
    #[serde(default)]
    secondary: Option<String>,
    #[serde(default)]
    tertiary: Option<String>,
    #[serde(default)]
    fallback: Option<String>,
    #[serde(default)]
    blocked: bool,
}

/// Blocco `routing` del file policy.
#[derive(Debug, Clone, Default, Deserialize)]
struct RoutingTable {
    #[serde(default)]
    tier_0: TierPolicy,
    #[serde(default)]
    tier_1: TierPolicy,
    #[serde(default)]
    tier_2: TierPolicy,
    #[serde(default)]
    tier_3: TierPolicy,
}

impl RoutingTable {
    /// Restituisce la policy del tier indicato (0..3). `None` per tier fuori range.
    fn for_tier(&self, tier: SensitivityTier) -> Option<&TierPolicy> {
        match tier {
            0 => Some(&self.tier_0),
            1 => Some(&self.tier_1),
            2 => Some(&self.tier_2),
            3 => Some(&self.tier_3),
            _ => None,
        }
    }
}

/// Blocco `features` del file policy (default DLP).
#[derive(Debug, Clone, Default, Deserialize)]
struct PolicyFeatures {
    #[serde(default)]
    allow_cloud_tier2: Option<bool>,
    #[serde(default)]
    allow_cloud_tier3: Option<bool>,
    #[serde(default)]
    dlp_enabled: Option<bool>,
}

/// File policy completo. Sono dichiarati solo i campi usati dal routing; gli
/// altri blocchi (providers, redaction, telemetry, ...) sono ignorati.
#[derive(Debug, Clone, Default, Deserialize)]
struct PolicyFile {
    #[serde(default)]
    profile: String,
    #[serde(default)]
    routing: RoutingTable,
    #[serde(default)]
    features: PolicyFeatures,
}

/// Esito di una decisione di routing (`RoutingDecision` del TS).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingDecision {
    /// Provider ordinati per priorita' (primary -> fallback) ammessi.
    pub providers: Vec<String>,
    /// `true` se nessun provider e' instradabile per la richiesta.
    pub blocked: bool,
    /// Motivo del blocco / dell'esito (per audit).
    pub reason: Option<String>,
    /// SEGNALE STRUTTURATO (regola M): `true` quando il blocco deriva dal gate
    /// DLP cloud per-tier (contenuto riservato -> provider cloud esclusi), NON
    /// da tier ignoto o flag tenant. I caller lo usano per rispondere col
    /// codice dedicato `POLICY_TIER_EXCLUDED` invece del generico
    /// `TIER_BLOCKED`, senza parsare la `reason` testuale.
    pub dlp_blocked: bool,
}

/// Override DLP letti dai settings DB. `None` su un campo = nessun valore in DB
/// -> si ricade sul valore YAML.
#[derive(Debug, Clone, Copy, Default)]
struct DlpOverrides {
    allow_cloud_tier2: Option<bool>,
    allow_cloud_tier3: Option<bool>,
    dlp_enabled: Option<bool>,
}

/// Errore di caricamento policy.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("caricamento policy fallito: {0}")]
    Load(String),
}

/// Provider considerati cloud (inviano dati a servizi esterni). Fonte unica per
/// il filtro tenant e per l'enforcement dei flag DLP per-tier. Non sono "nomi di
/// modello" (regola G), ma l'insieme strutturale dei provider non-locali, allineato
/// alla costante `CLOUD_PROVIDERS` del TS.
const CLOUD_PROVIDERS: [&str; 5] = ["anthropic", "openai", "mistral", "deepseek", "google"];

/// TTL della cache override DB (60s, come il TS `DB_TTL_MS`).
const DB_TTL: Duration = Duration::from_secs(60);

/// Motore di policy. La policy YAML e' immutabile dopo il caricamento; gli
/// override DB vivono in una `TtlCache` condivisa (clonabile a basso costo).
#[derive(Debug, Clone)]
pub struct PolicyEngine {
    policy: Arc<PolicyFile>,
    // Chiave unit: c'e' un solo set di override globale, ma la scadenza temporale
    // e' gestita dal punto unico TtlCache (regola L) invece che da un timestamp
    // manuale. `get(&())` ritorna gli override solo se non scaduti.
    overrides: TtlCache<(), DlpOverrides>,
}

impl PolicyEngine {
    /// Carica la policy dal file YAML indicato.
    pub fn from_yaml_file(path: &str) -> Result<Self, PolicyError> {
        let raw = std::fs::read_to_string(path).map_err(|e| PolicyError::Load(e.to_string()))?;
        Self::from_yaml_str(&raw)
    }

    /// Carica la policy da una stringa YAML (usato nei test e dal loader file).
    pub fn from_yaml_str(yaml: &str) -> Result<Self, PolicyError> {
        let policy: PolicyFile =
            serde_yaml::from_str(yaml).map_err(|e| PolicyError::Load(e.to_string()))?;
        Ok(Self {
            policy: Arc::new(policy),
            overrides: TtlCache::new(DB_TTL),
        })
    }

    /// Nome del profilo policy (`getProfile` del TS).
    pub fn profile(&self) -> &str {
        &self.policy.profile
    }

    /// Override correnti (se in cache e non scaduti), altrimenti default vuoto.
    fn current_overrides(&self) -> DlpOverrides {
        self.overrides.get(&()).unwrap_or_default()
    }

    /// Valore effettivo di un flag: priorita' al DB (settings), fallback YAML.
    fn flag(&self, key: DlpFlag) -> Option<bool> {
        let ov = self.current_overrides();
        let (from_db, from_yaml) = match key {
            DlpFlag::AllowCloudTier2 => (ov.allow_cloud_tier2, self.policy.features.allow_cloud_tier2),
            DlpFlag::AllowCloudTier3 => (ov.allow_cloud_tier3, self.policy.features.allow_cloud_tier3),
            DlpFlag::DlpEnabled => (ov.dlp_enabled, self.policy.features.dlp_enabled),
        };
        from_db.or(from_yaml)
    }

    /// Gate cloud per-tier. Tier 0/1 non hanno gate (sempre permessi -> `None`).
    fn cloud_gate_for_tier(&self, tier: SensitivityTier) -> Option<bool> {
        if tier >= 3 {
            self.flag(DlpFlag::AllowCloudTier3)
        } else if tier == 2 {
            self.flag(DlpFlag::AllowCloudTier2)
        } else {
            None
        }
    }

    /// `true` se la classificazione DLP e' disattivata (`dlp_enabled=false`).
    /// In tal caso il caller tratta ogni richiesta come tier 0.
    pub fn is_dlp_disabled(&self) -> bool {
        self.flag(DlpFlag::DlpEnabled) == Some(false)
    }

    /// `true` se i provider CLOUD sono bloccati dal gate DLP per il tier dato
    /// (flag `dlp_allow_cloud_tier2/3`, DB > YAML). Tier 0/1 non hanno gate.
    /// Punto unico (regola L) riusato dal path pin di `routes.rs`: il pin
    /// bypassa il ROUTING (`decide`) ma non questo gate di sicurezza.
    pub fn cloud_blocked_for_tier(&self, tier: SensitivityTier) -> bool {
        if self.is_dlp_disabled() {
            return false;
        }
        self.cloud_gate_for_tier(tier) == Some(false)
    }

    /// `true` se `name` e' un provider cloud (invia dati a servizi esterni).
    /// Espone la fonte unica [`CLOUD_PROVIDERS`] ai caller del gate.
    pub fn is_cloud_provider(name: &str) -> bool {
        CLOUD_PROVIDERS.contains(&name)
    }

    /// Ricarica i flag DLP dai settings DB con cache TTL 60s. Se il DB e'
    /// irraggiungibile mantiene i valori correnti (fallback graceful sullo YAML).
    /// `force=true` ignora la cache (usato all'avvio).
    ///
    /// La gestione TTL e' delegata interamente a `TtlCache`: se la entry e'
    /// ancora valida e non si forza, si esce subito senza toccare il DB.
    pub async fn refresh_db_overrides(&self, pool: &PgPool, force: bool) {
        if !force && self.overrides.get(&()).is_some() {
            return;
        }

        // settings.value e' TEXT; leggiamo le 3 chiavi DLP in un colpo.
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT key, value FROM settings \
             WHERE key IN ('dlp_allow_cloud_tier2', 'dlp_allow_cloud_tier3', 'dlp_enabled')",
        )
        .fetch_all(pool)
        .await;

        match rows {
            Ok(rows) => {
                let mut next = DlpOverrides::default();
                for (key, value) in &rows {
                    let parsed = parse_bool(value);
                    match key.as_str() {
                        "dlp_allow_cloud_tier2" => next.allow_cloud_tier2 = parsed,
                        "dlp_allow_cloud_tier3" => next.allow_cloud_tier3 = parsed,
                        "dlp_enabled" => next.dlp_enabled = parsed,
                        _ => {}
                    }
                }
                self.overrides.insert((), next);
            }
            Err(err) => {
                // DB down: non azzerare gli override gia' noti. La entry scaduta
                // resta tale -> retry al prossimo giro; il routing prosegue sui
                // valori YAML / ultimi noti.
                tracing::warn!(
                    error = %err,
                    "policy-engine: refresh override DLP fallito, mantengo i flag correnti (fallback YAML)"
                );
            }
        }
    }

    /// Decide la lista di provider per (tier, feature, tenant flags).
    /// Porting 1:1 della `decide` del TS.
    pub fn decide(
        &self,
        tier: SensitivityTier,
        _feature: &str,
        tenant_flags: &std::collections::HashMap<String, bool>,
    ) -> RoutingDecision {
        // DLP disattivato da DB: nessuna sensibilita', si instrada come tier 0.
        let effective_tier: SensitivityTier = if self.is_dlp_disabled() { 0 } else { tier };

        let Some(tier_policy) = self.policy.routing.for_tier(effective_tier) else {
            return RoutingDecision {
                providers: Vec::new(),
                blocked: true,
                reason: Some(format!("Nessuna policy per tier {effective_tier}")),
                dlp_blocked: false,
            };
        };

        // Gate cloud per-tier: il flag DLP (DB > YAML) e' la fonte di verita' del
        // blocco cloud, NON il campo `tier_N.blocked`. Se il flag e' definito vince;
        // se non definito (ne' DB ne' YAML) si ricade sul comportamento storico
        // basato su `blocked`.
        let cloud_gate = self.cloud_gate_for_tier(effective_tier);
        let block_cloud_by_dlp = cloud_gate == Some(false);

        if cloud_gate.is_none() && tier_policy.blocked {
            return RoutingDecision {
                providers: Vec::new(),
                blocked: true,
                reason: Some(format!(
                    "Tier {effective_tier} bloccato dalla policy (profilo: {})",
                    self.policy.profile
                )),
                dlp_blocked: false,
            };
        }

        // Tenant override: se il tenant blocca il cloud, esclude i provider cloud.
        let tenant_blocks_cloud = tenant_flags.get("block_cloud").copied().unwrap_or(false);

        let ordered: Vec<String> = [
            tier_policy.primary.as_deref(),
            tier_policy.secondary.as_deref(),
            tier_policy.tertiary.as_deref(),
            tier_policy.fallback.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(|s| s.to_string())
        .collect();

        let exclude_cloud = tenant_blocks_cloud || block_cloud_by_dlp;
        let filtered: Vec<String> = if exclude_cloud {
            ordered
                .into_iter()
                .filter(|p| !CLOUD_PROVIDERS.contains(&p.as_str()))
                .collect()
        } else {
            ordered
        };

        if filtered.is_empty() {
            let reason = if block_cloud_by_dlp {
                format!(
                    "Tier {effective_tier}: provider cloud bloccati dal flag DLP \
                     (dlp_allow_cloud_tier{effective_tier}=false) e nessun provider locale configurato"
                )
            } else {
                format!("Nessun provider disponibile per tier {effective_tier} con i flag tenant correnti")
            };
            return RoutingDecision {
                providers: Vec::new(),
                blocked: true,
                reason: Some(reason),
                dlp_blocked: block_cloud_by_dlp,
            };
        }

        RoutingDecision {
            providers: filtered,
            blocked: false,
            reason: None,
            dlp_blocked: false,
        }
    }

    /// Segnala (solo per audit) un'auto-elevazione di tier. Non interrompe il
    /// flusso: e' il gateway a rilevare il tier reale via classifier, quindi un
    /// `detected > claimed` non e' un errore del caller (vedi nota nel TS).
    pub fn validate_tier_claim(&self, claimed: SensitivityTier, detected: SensitivityTier) {
        if detected > claimed {
            tracing::warn!(
                claimed,
                detected,
                "policy-engine: tier auto-elevato; routing applicato sul tier effettivo (policy enforced)"
            );
        }
    }
}

/// Flag DLP gestiti dal motore.
#[derive(Debug, Clone, Copy)]
enum DlpFlag {
    AllowCloudTier2,
    AllowCloudTier3,
    DlpEnabled,
}

/// Parsing booleano tollerante (`PARSE_BOOL` del TS): "true"/"1" -> true,
/// "false"/"0" -> false, altro -> `None` (valore ignoto -> nessun override).
fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // Policy minimale che riproduce la struttura di default.yaml ma senza i
    // blocchi non usati dal routing. Autosufficiente: nessuna lettura on-disk.
    const POLICY: &str = r#"
profile: cloud
features:
  allow_cloud_tier2: true
  allow_cloud_tier3: false
  dlp_enabled: true
routing:
  tier_0:
    primary: openai
    secondary: deepseek
    tertiary: google
    fallback: mistral
  tier_1:
    primary: openai
    secondary: deepseek
  tier_2:
    primary: openai
    secondary: deepseek
  tier_3:
    primary: anthropic
    secondary: openai
    fallback: deepseek
"#;

    fn engine() -> PolicyEngine {
        PolicyEngine::from_yaml_str(POLICY).expect("policy valida")
    }

    #[test]
    fn tier_0_ritorna_chain_ordinata() {
        let e = engine();
        let d = e.decide(0, "chat", &HashMap::new());
        assert!(!d.blocked);
        assert_eq!(d.providers, vec!["openai", "deepseek", "google", "mistral"]);
    }

    #[test]
    fn tier_3_cloud_bloccato_da_flag_yaml() {
        let e = engine();
        // allow_cloud_tier3=false (YAML) -> tutti i provider del tier 3 sono cloud
        // -> nessun provider locale -> blocked.
        let d = e.decide(3, "chat", &HashMap::new());
        assert!(d.blocked);
        assert!(d.providers.is_empty());
        // Segnale strutturato: il blocco viene dal gate DLP (regola M).
        assert!(d.dlp_blocked);
        assert!(d.reason.unwrap().contains("dlp_allow_cloud_tier3=false"));
        // Gate riusato dal path pin: cloud bloccato a tier 3, libero a tier 0-2.
        assert!(e.cloud_blocked_for_tier(3));
        assert!(!e.cloud_blocked_for_tier(2));
        assert!(!e.cloud_blocked_for_tier(0));
        assert!(PolicyEngine::is_cloud_provider("deepseek"));
        assert!(!PolicyEngine::is_cloud_provider("vllm"));
    }

    #[test]
    fn tier_2_cloud_permesso_da_flag_yaml() {
        let e = engine();
        // allow_cloud_tier2=true -> i provider cloud restano.
        let d = e.decide(2, "chat", &HashMap::new());
        assert!(!d.blocked);
        assert_eq!(d.providers, vec!["openai", "deepseek"]);
    }

    #[test]
    fn tenant_block_cloud_esclude_provider_cloud() {
        let e = engine();
        let mut flags = HashMap::new();
        flags.insert("block_cloud".to_string(), true);
        // Tier 0 ha solo provider cloud nello YAML di test -> tutti esclusi -> blocked.
        let d = e.decide(0, "chat", &flags);
        assert!(d.blocked);
        assert!(d.providers.is_empty());
        // Blocco da flag TENANT, non dal gate DLP: dlp_blocked resta false.
        assert!(!d.dlp_blocked);
    }

    #[test]
    fn override_db_dlp_disabilitato_instrada_come_tier0() {
        let e = engine();
        // Simula override DB: dlp_enabled=false -> ogni tier trattato come tier 0.
        e.overrides.insert(
            (),
            DlpOverrides {
                dlp_enabled: Some(false),
                ..Default::default()
            },
        );
        assert!(e.is_dlp_disabled());
        // Richiesta tier 3, ma instradata come tier 0 (chain completa, non bloccata).
        let d = e.decide(3, "chat", &HashMap::new());
        assert!(!d.blocked);
        assert_eq!(d.providers, vec!["openai", "deepseek", "google", "mistral"]);
    }

    #[test]
    fn override_db_vince_su_yaml() {
        let e = engine();
        // YAML: allow_cloud_tier3=false. Override DB: true -> cloud tier 3 permesso.
        e.overrides.insert(
            (),
            DlpOverrides {
                allow_cloud_tier3: Some(true),
                ..Default::default()
            },
        );
        let d = e.decide(3, "chat", &HashMap::new());
        assert!(!d.blocked);
        assert_eq!(d.providers, vec!["anthropic", "openai", "deepseek"]);
    }

    #[test]
    fn parse_bool_tollerante() {
        assert_eq!(parse_bool("true"), Some(true));
        assert_eq!(parse_bool(" 1 "), Some(true));
        assert_eq!(parse_bool("FALSE"), Some(false));
        assert_eq!(parse_bool("0"), Some(false));
        assert_eq!(parse_bool("boh"), None);
    }
}
