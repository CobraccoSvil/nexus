//! Helper condivisi fra i due lati che gestiscono i plugin: `mcp-core` e
//! `plugin-service`.
//!
//! Stesso motivo per cui esiste [`crate::server_storage`], e stesso crate: la
//! Wave C1 lo aveva creato per le ~510 righe di `mcp_client.rs` duplicate fra
//! questi due, e queste erano rimaste indietro.
//!
//! Trovate dal censimento delle firme (`xtask signature-census`) il 2026-08-05:
//! `value_to_string_map` e `normalize_scope` uscivano come gruppi cross-crate ad
//! alto punteggio. Guardandole, la duplicazione era piu' larga di quanto la
//! firma dicesse — anche `get_json_object` e `can_manage_instance` erano
//! gemelle, e `normalize_scope` esisteva in **tre** copie.
//!
//! Nessuna di loro era visibile a `jscpd`: sono blocchi da 6-10 righe, sotto la
//! sua soglia di 15.

use std::collections::HashMap;

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// Gli scope che un plugin puo' avere. Vocabolario chiuso, in un posto solo.
pub const SCOPE_VALIDI: [&str; 3] = ["global", "project", "user"];

/// Normalizza e valida lo scope di un plugin: `Ok` col valore canonico
/// (minuscolo, senza spazi), `Err` con il valore rifiutato.
///
/// **Pura, e senza tipi HTTP**: questo crate non dipende da axum e non deve. Il
/// messaggio d'errore lo compone il chiamante, che sa quale CAMPO stava
/// validando — le tre copie differivano esattamente in quello (`"Scope non
/// valido"` contro `"defaultScope non valido"`), e portarlo qui avrebbe
/// richiesto un parametro `nome_campo` che esiste solo per fabbricare una
/// stringa.
///
/// L'assenza vale `global`: e' il default storico dei tre chiamanti, non una
/// scelta nuova.
pub fn normalizza_scope(raw: Option<&str>) -> Result<String, String> {
    let scope = raw.unwrap_or("global").trim().to_lowercase();
    if SCOPE_VALIDI.contains(&scope.as_str()) {
        Ok(scope)
    } else {
        Err(scope)
    }
}

/// L'oggetto JSON sotto `field`, se c'e' ed e' un oggetto.
pub fn get_json_object<'a>(raw: &'a Value, field: &str) -> Option<&'a serde_json::Map<String, Value>> {
    raw.get(field).and_then(Value::as_object)
}

/// Le sole coppie chiave/valore STRINGA di un oggetto JSON.
///
/// I valori non-stringa vengono scartati invece di far fallire l'intera mappa:
/// e' la stessa tolleranza che [`crate::server_storage::build_config`] applica
/// ad args/env/headers, e sopravvive a una row malformata.
pub fn value_to_string_map(raw: Option<&serde_json::Map<String, Value>>) -> HashMap<String, String> {
    raw.map(|obj| {
        obj.iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect::<HashMap<_, _>>()
    })
    .unwrap_or_default()
}

/// Puo' questo utente gestire la riga? Proprietario, oppure admin su una riga
/// `global`.
///
/// La colonna del proprietario e' un PARAMETRO perche' e' l'unica cosa che
/// cambia fra i due domini: `installed_by_user_id` per le istanze di plugin,
/// `user_id` per i server MCP (vedi
/// [`crate::server_storage::can_manage_server`], che resta separata perche' ha
/// gia' i suoi chiamanti e il suo nome nel dominio dei server).
pub fn puo_gestire(
    row: &sqlx::postgres::PgRow,
    user_id: Uuid,
    role: &str,
    colonna_proprietario: &str,
) -> bool {
    use sqlx::Row;
    let scope: String = row.try_get("scope").unwrap_or_else(|_| "user".to_string());
    let owner: Option<Uuid> = row.try_get(colonna_proprietario).unwrap_or(None);
    owner == Some(user_id) || (scope == "global" && role == "admin")
}



// ---------------------------------------------------------------------------
// Sette funzioni che vivevano IDENTICHE in mcp-core/src/plugins/mod.rs e
// plugin-service/src/plugins.rs.
//
// Il censimento delle firme aveva segnalato quei due file in cima ai gruppi
// cross-crate; guardandoli, ventuno funzioni portavano lo STESSO NOME. Di
// quelle, dodici avevano corpo identico a meno della formattazione e nove erano
// DIVERGUTE - due copie della stessa funzione che si sono allontanate senza che
// nessuno lo sapesse (`get_catalog_by_install_request` misurava 3006 caratteri
// da un lato e 1056 dall'altro).
//
// Qui scendono le identiche che non chiedono ne' axum ne' nexus-auth. Le altre
// restano dove sono, e per una ragione: `redirect_with_status` produce una
// risposta HTTP, e questo crate non dipende da axum - farlo dipendere per una
// funzione significherebbe trascinare un framework web dentro una libreria di
// accesso ai dati.
// ---------------------------------------------------------------------------

pub const FIGMA_DEFAULT_RETURN_TO: &str = "/admin/settings/connectors";


/// Il token e' un Personal Access Token di Figma (prefisso `figd_`)?
///
/// Distinguerlo da un token OAuth conta perche' i due si usano in modo diverso:
/// il PAT va nell'header proprietario, l'OAuth in `Authorization: Bearer`.
pub fn is_figma_pat(token: &str) -> bool {
    token.trim().to_lowercase().starts_with("figd_")
}

/// Le sole stringhe di un array JSON, scartando gli altri tipi.
///
/// Tollerante come [`value_to_string_map`]: un elemento non-stringa non fa
/// fallire l'intero campo, cosi' una riga malformata non porta giu' la lettura.
pub fn parse_string_array(raw: &Value) -> Vec<String> {
    raw.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Il `return_to` di un redirect OAuth, ridotto a un percorso INTERNO.
///
/// Accetta solo path che iniziano con `/` e non con `//`: un `//host` sarebbe
/// un URL protocol-relative, cioe' un redirect verso un altro sito con l'aria
/// di essere relativo. Tutto il resto ricade sul default.
pub fn sanitize_return_to(value: Option<&str>) -> String {
    let raw = value.unwrap_or(FIGMA_DEFAULT_RETURN_TO).trim();
    if raw.starts_with('/') && !raw.starts_with("//") {
        raw.to_string()
    } else {
        FIGMA_DEFAULT_RETURN_TO.to_string()
    }
}

/// Il valore di un setting segreto, se presente e non vuoto.
///
/// `None` copre insieme "la chiave non c'e'" e "c'e' ma e' vuota": per il
/// chiamante sono lo stesso caso — non ha una credenziale da usare.
pub async fn resolve_secret_value(db: &PgPool, setting_key: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
        .bind(setting_key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Scrive un setting, creandolo se manca.
///
/// A differenza di `nexus_auth::update_setting_value` (che RIFIUTA una chiave
/// sconosciuta, per non far nascere configurazione per errore) qui l'upsert e'
/// voluto: le chiavi dei plugin nascono quando il plugin viene installato, e
/// non esistono prima.
pub async fn upsert_setting_value(
    db: &PgPool,
    key: &str,
    value: &str,
    category: &str,
    description: &str,
    is_secret: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO settings (key, value, category, description, is_secret, updated_at)
        VALUES ($1, $2, $3, $4, $5, NOW())
        ON CONFLICT (key) DO UPDATE
        SET value = EXCLUDED.value,
            category = EXCLUDED.category,
            description = EXCLUDED.description,
            is_secret = EXCLUDED.is_secret,
            updated_at = NOW()
        "#,
    )
    .bind(key)
    .bind(value)
    .bind(category)
    .bind(description)
    .bind(is_secret)
    .execute(db)
    .await
    .map(|_| ())
}

/// Registra un evento di audit di un plugin.
///
/// Non propaga l'errore: un audit che non riesce a scriversi non deve far
/// fallire l'operazione che stava tracciando — sarebbe la coda che muove il
/// cane. L'esito finisce nei log.
pub async fn write_plugin_audit(
    db: &PgPool,
    plugin_instance_id: Option<Uuid>,
    user_id: Option<Uuid>,
    project_id: Option<Uuid>,
    action: &str,
    status: &str,
    message: Option<String>,
    payload: Value,
) {
    let _ = sqlx::query(
        r#"
        INSERT INTO plugin_audit_events
            (plugin_instance_id, user_id, project_id, action, status, message, payload)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(plugin_instance_id)
    .bind(user_id)
    .bind(project_id)
    .bind(action)
    .bind(status)
    .bind(message)
    .bind(payload)
    .execute(db)
    .await;
}

// `find_duplicate_instance_anywhere` NON e' scesa qui, pur essendo identica nei
// due file: ritorna `(StatusCode, Json<Value>)`, cioe' un errore axum. Vale la
// stessa ragione di `redirect_with_status` — questo crate non dipende da un
// framework web, e non deve iniziare a farlo per il tipo di ritorno di una
// query. Se un giorno serve davvero condivisa, la forma e' quella usata per
// `crea_directory`: la query qui, la traduzione HTTP nel chiamante.

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lo_scope_assente_vale_global() {
        assert_eq!(normalizza_scope(None).unwrap(), "global");
    }

    #[test]
    fn lo_scope_si_normalizza_prima_di_essere_giudicato() {
        assert_eq!(normalizza_scope(Some("  PROJECT ")).unwrap(), "project");
    }

    #[test]
    fn uno_scope_fuori_vocabolario_torna_indietro_per_il_messaggio() {
        // Il chiamante riceve il valore RIFIUTATO, cosi' puo' nominarlo
        // nell'errore senza doverlo ri-normalizzare.
        assert_eq!(normalizza_scope(Some("Team")).unwrap_err(), "team");
    }

    #[test]
    fn i_valori_non_stringa_si_scartano_senza_far_cadere_la_mappa() {
        let raw = json!({"a": "uno", "b": 2, "c": "tre"});
        let m = value_to_string_map(raw.as_object());
        assert_eq!(m.len(), 2);
        assert_eq!(m.get("a").map(String::as_str), Some("uno"));
        assert!(!m.contains_key("b"));
    }

    #[test]
    fn una_mappa_assente_non_e_un_errore() {
        assert!(value_to_string_map(None).is_empty());
    }

    #[test]
    fn get_json_object_pretende_un_oggetto() {
        let raw = json!({"obj": {"k": "v"}, "arr": [1, 2], "num": 3});
        assert!(get_json_object(&raw, "obj").is_some());
        assert!(get_json_object(&raw, "arr").is_none());
        assert!(get_json_object(&raw, "num").is_none());
        assert!(get_json_object(&raw, "assente").is_none());
    }
}
