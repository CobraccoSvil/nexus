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
