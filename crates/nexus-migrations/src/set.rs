//! Quali set esistono, e dove stanno.
//!
//! Le due stringhe `db/migrations` e `db/migrations/project` sono scritte QUI e
//! in nessun altro posto: erano sparse in nove punti, e ogni copia poteva
//! restare indietro da sola.

/// Un set di migrazioni.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Set {
    /// Schema META: settings, catalogo, routing, telemetria, code di servizio.
    Meta,
    /// Schema di un DB-progetto.
    Project,
}

impl Set {
    /// Percorso del set, relativo alla radice del repository.
    pub const fn sottopercorso(self) -> &'static str {
        match self {
            Self::Meta => "db/migrations",
            Self::Project => "db/migrations/project",
        }
    }

    /// Nome per messaggi e premesse.
    pub const fn nome(self) -> &'static str {
        match self {
            Self::Meta => "meta",
            Self::Project => "project",
        }
    }

    /// Parse dell'identificatore canonico (regola N: un solo nome per set).
    pub fn try_parse(s: &str) -> Option<Self> {
        match s {
            "meta" => Some(Self::Meta),
            "project" => Some(Self::Project),
            _ => None,
        }
    }
}

impl std::fmt::Display for Set {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.nome())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn il_sottopercorso_del_progetto_sta_dentro_quello_meta() {
        // Non e' un dettaglio: il set project e' una SOTTODIRECTORY di quello
        // meta, quindi un migrator del set meta che non escludesse la
        // sottodirectory tenterebbe di applicare anche le migrazioni di
        // progetto. sqlx non discende nelle sottodirectory, ma il vincolo va
        // dichiarato perche' regge la scelta dei due percorsi.
        assert!(Set::Project.sottopercorso().starts_with(Set::Meta.sottopercorso()));
    }

    #[test]
    fn i_nomi_canonici_si_parsano_e_nient_altro() {
        assert_eq!(Set::try_parse("meta"), Some(Set::Meta));
        assert_eq!(Set::try_parse("project"), Some(Set::Project));
        assert_eq!(Set::try_parse("Meta"), None, "niente parser leniente (regola N)");
        assert_eq!(Set::try_parse("progetto"), None);
        assert_eq!(Set::try_parse(""), None);
    }
}
