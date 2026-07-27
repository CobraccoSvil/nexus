//! Catalogo dei servizi infrastruttura Nexus: PUNTO UNICO (regola L) della
//! forma delle voci e della loro lettura dal DB.
//!
//! La fonte di verita' resta il setting `system.services_catalog` (migrazione
//! 0541, esteso dalla 0601 e dalla 0642): questo crate non ne tiene una copia,
//! ne definisce solo la forma e il modo di leggerlo.
//!
//! Perche' un crate a se' e non un modulo di mcp-core: la struttura delle voci
//! era `pub(crate)` dentro mcp-core, quindi qualunque strumento che volesse
//! misurare il catalogo doveva ricopiarsela. Ricopiare la forma di un dato per
//! poterlo misurare e' il modo in cui uno strumento smette di misurare il
//! sistema e comincia a misurare una propria imitazione (regola O).
//!
//! Consumatori: `mcp-core` (endpoint `/api/system/services`, `services_watchdog`)
//! e `xtask service-manifests` (generazione dei manifest di servizio).

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

/// Chiave settings del catalogo unico (migrazione 0541).
pub const CATALOG_SETTING_KEY: &str = "system.services_catalog";

/// Voce del catalogo dei microservizi infrastruttura. Deserializzata da
/// `system.services_catalog`. Vedi migrazione 0541 per la semantica dei campi.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CatalogEntry {
    /// Nome canonico (= `--service` di deploy-local.sh), usato come id nell'URL.
    pub name: String,
    #[serde(default)]
    pub label: String,
    /// Chiave settings da cui risolvere la porta (regola G).
    #[serde(default)]
    pub port_setting_key: Option<String>,
    /// Porta letterale (solo infra dati: postgres/redis).
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub led: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Mostrato ma non controllabile (nessun pulsante start/stop/restart).
    #[serde(default)]
    pub readonly: bool,
    /// start/stop/restart ammessi (allowlist di controllo).
    #[serde(default)]
    pub controllable: bool,
    /// Mostrato nel pannello "Servizi Nexus".
    #[serde(default)]
    pub panel_shown: bool,
    /// Auto-restart dal services_watchdog (mcp-core escluso: ospita il watchdog).
    #[serde(default)]
    pub watchdog_managed: bool,
    /// Target di controllo su Unix.
    #[serde(default)]
    pub systemd_unit: Option<String>,
    /// Target di controllo su Windows.
    #[serde(default)]
    pub winsw_id: Option<String>,
    /// Hint di provenienza (non usato per lo stato: lo stato e' un TCP probe).
    #[serde(default)]
    pub docker_container: Option<String>,
}

/// Perche' il catalogo non e' disponibile. I tre casi sono distinti perche'
/// richiedono azioni diverse: il DB irraggiungibile e' un problema di ambiente,
/// la chiave assente e' una migrazione non applicata, il JSON illeggibile e' un
/// dato corrotto.
///
/// Prima erano tutti e tre un `Vec::new()` con un warn: il pannello diceva
/// "zero servizi" quando il fatto vero era "non ho potuto leggere il catalogo".
/// Un'assenza e un fallimento non sono lo stesso stato (regola M).
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("lettura di {key} dal DB fallita: {source}")]
    Lettura {
        key: &'static str,
        #[source]
        source: anyhow::Error,
    },
    #[error(
        "setting {key} assente: il catalogo servizi non e' stato popolato \
         (applicare le migrazioni del DB meta, vedi db/migrations/0541)"
    )]
    ChiaveAssente { key: &'static str },
    #[error("catalogo {key} non parsabile come lista di voci: {dettaglio}")]
    NonParsabile {
        key: &'static str,
        dettaglio: String,
    },
}

/// Carica il catalogo dal DB.
///
/// La lettura passa da `nexus_auth::get_setting_checked`, che propaga l'errore,
/// e non dalla variante che lo ingoia: qui la differenza fra "assente" e "non
/// ho potuto leggere" e' esattamente cio' che il chiamante deve sapere.
pub async fn load_catalog(db: &PgPool) -> Result<Vec<CatalogEntry>, CatalogError> {
    let raw = nexus_auth::get_setting_checked(db, CATALOG_SETTING_KEY)
        .await
        .map_err(|source| CatalogError::Lettura {
            key: CATALOG_SETTING_KEY,
            source,
        })?;
    let raw = raw.ok_or(CatalogError::ChiaveAssente {
        key: CATALOG_SETTING_KEY,
    })?;
    serde_json::from_str::<Vec<CatalogEntry>>(&raw).map_err(|e| CatalogError::NonParsabile {
        key: CATALOG_SETTING_KEY,
        dettaglio: e.to_string(),
    })
}

/// Risolve la porta di una voce: prima `port_setting_key` dal DB (regola G),
/// poi la porta letterale (infra dati). `None` se nessuna delle due e'
/// disponibile: il chiamante decide se e' un difetto o un caso legittimo.
pub async fn resolve_port(db: &PgPool, entry: &CatalogEntry) -> Option<u16> {
    if let Some(key) = entry.port_setting_key.as_deref() {
        if let Ok(Some(v)) = nexus_auth::get_setting_checked(db, key).await {
            if let Ok(p) = v.trim().parse::<u16>() {
                return Some(p);
            }
        }
    }
    entry.port
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Una voce del catalogo ha il solo `name` obbligatorio: le migrazioni
    /// omettono i campi non pertinenti (es. `led` solo su alcuni servizi) e una
    /// deserializzazione stretta le rifiuterebbe tutte.
    #[test]
    fn una_voce_minima_si_deserializza() {
        let e: CatalogEntry = serde_json::from_str(r#"{"name":"mcp-core"}"#).expect("voce minima");
        assert_eq!(e.name, "mcp-core");
        assert!(e.winsw_id.is_none());
        assert!(!e.watchdog_managed);
    }

    /// I campi che il generatore di manifest usa devono sopravvivere al
    /// round-trip: se un rename li staccasse dal JSON delle migrazioni, il
    /// generatore vedrebbe `None` e produrrebbe un piano monco in silenzio.
    #[test]
    fn i_campi_usati_dal_generatore_sopravvivono_al_round_trip() {
        let json = r#"{"name":"browser-bridge-mcp","label":"Browser Bridge",
            "port_setting_key":"browser_bridge_port","watchdog_managed":true,
            "winsw_id":"nexus-browser-bridge","description":"MCP browser bridge"}"#;
        let e: CatalogEntry = serde_json::from_str(json).expect("voce completa");
        assert_eq!(e.winsw_id.as_deref(), Some("nexus-browser-bridge"));
        assert_eq!(e.port_setting_key.as_deref(), Some("browser_bridge_port"));
        assert!(e.watchdog_managed);
        let ri: CatalogEntry =
            serde_json::from_str(&serde_json::to_string(&e).expect("serialize")).expect("re-parse");
        assert_eq!(ri.winsw_id, e.winsw_id);
        assert_eq!(ri.port_setting_key, e.port_setting_key);
    }
}
