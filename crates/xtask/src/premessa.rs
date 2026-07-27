//! Da dove sto guardando: la premessa che accompagna ogni numero.
//!
//! Punto unico (regola L) della redazione dell'URL del database. Esisteva in due
//! copie — `battery_explain::dichiara_db` e `service_manifests::url_redatto` —
//! con semantiche diverse, e **entrambe lasciavano passare intatto**
//! `postgres://u:p@host/db?password=segreto`: nessuna delle due toccava la
//! query string.
//!
//! Un numero senza la sua premessa e' un'opinione; ma la premessa non deve
//! essere una credenziale.

/// L'URL come si dichiara a chi legge: utente conservato, password rimossa
/// ovunque compaia.
///
/// L'utente resta perche' fa parte della diagnosi (un errore di permessi si
/// capisce solo sapendo con quale ruolo ci si e' connessi). I parametri di query
/// NON si troncano: `?sslmode=disable` distingue due connessioni e va mostrato.
/// Si redige per CHIAVE, non tagliando tutto dopo il `?`.
pub fn db_dichiarato(url: &str) -> String {
    let Some((schema, resto)) = url.split_once("://") else {
        return "(DATABASE_URL non interpretabile)".into();
    };
    // La parte host puo' contenere '@' nella password: si taglia sull'ULTIMO.
    let (credenziali, host_e_resto) = match resto.rsplit_once('@') {
        Some((c, h)) => (Some(c), h),
        None => (None, resto),
    };
    let utente = credenziali.map(|c| c.split_once(':').map_or(c, |(u, _)| u));

    let (percorso, query) = match host_e_resto.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (host_e_resto, None),
    };

    let mut out = String::with_capacity(url.len());
    out.push_str(schema);
    out.push_str("://");
    if let Some(u) = utente {
        out.push_str(u);
        out.push('@');
    }
    out.push_str(percorso);
    if let Some(q) = query {
        out.push('?');
        out.push_str(&redigi_query(q));
    }
    out
}

/// Redige i soli parametri che portano un segreto, lasciando gli altri leggibili.
fn redigi_query(query: &str) -> String {
    query
        .split('&')
        .map(|coppia| {
            let (chiave, _) = coppia.split_once('=').unwrap_or((coppia, ""));
            if e_segreto(chiave) {
                format!("{chiave}=***")
            } else {
                coppia.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// Chiavi di query che portano una credenziale. Confronto case-insensitive:
/// libpq accetta `password` e `PGPASSWORD` indifferentemente.
fn e_segreto(chiave: &str) -> bool {
    let k = chiave.trim().to_ascii_lowercase();
    matches!(k.as_str(), "password" | "pgpassword" | "sslpassword")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_password_sparisce_e_l_utente_resta() {
        assert_eq!(
            db_dichiarato("postgres://nexus:segretissimo@localhost:5433/nexus"),
            "postgres://nexus@localhost:5433/nexus"
        );
    }

    /// IL DIFETTO CHE ENTRAMBE LE COPIE AVEVANO: la password nella query string
    /// usciva intatta. MUTAZIONE: togliere `redigi_query` e questo rosseggia.
    #[test]
    fn la_password_nella_query_string_non_passa() {
        let r = db_dichiarato("postgres://u:p@h:5433/db?password=segreto&sslmode=disable");
        assert!(!r.contains("segreto"), "password esposta nella query: {r}");
        assert!(
            r.contains("sslmode=disable"),
            "sslmode e' premessa utile e va mostrato: {r}"
        );
    }

    /// Troncare alla `?` sarebbe perdita di premessa: sslmode distingue due
    /// connessioni verso lo stesso host.
    #[test]
    fn i_parametri_non_sensibili_restano_leggibili() {
        let r = db_dichiarato("postgres://u@h/db?sslmode=require&application_name=xtask");
        assert!(r.contains("sslmode=require"));
        assert!(r.contains("application_name=xtask"));
    }

    #[test]
    fn un_url_senza_credenziali_resta_intero() {
        assert_eq!(
            db_dichiarato("postgres://localhost:5433/nexus"),
            "postgres://localhost:5433/nexus"
        );
    }

    /// Mai la stringa grezza in caso di dubbio: `url_redatto` la restituiva
    /// tale e quale quando non trovava `://`, cioe' proprio quando non aveva
    /// capito cosa stava guardando.
    #[test]
    fn un_url_incomprensibile_non_viene_stampato_com_e() {
        let r = db_dichiarato("questo-non-e-un-url-ma-contiene:segreto");
        assert!(!r.contains("segreto"), "stringa grezza esposta: {r}");
        assert_eq!(r, "(DATABASE_URL non interpretabile)");
    }

    #[test]
    fn la_password_con_chiocciola_non_confonde_il_taglio() {
        // Una password con '@' e' legittima: il taglio va fatto sull'ultimo.
        let r = db_dichiarato("postgres://utente:pa@ss@host:5433/db");
        assert_eq!(r, "postgres://utente@host:5433/db");
    }
}
