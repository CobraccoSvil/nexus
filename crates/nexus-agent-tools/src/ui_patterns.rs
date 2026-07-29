//! Catalogo dei PATTERN DI LAYOUT: PUNTO UNICO (regola L) della lettura dei
//! riferimenti di interfaccia che il consiglio cita e l'esecuzione applica.
//!
//! Una figura che dice "servirebbe un layout migliore" non produce una UI
//! migliore: produce una frase. Il catalogo esiste perche' il parere possa
//! citare una struttura CONCRETA — quali zone, in che gerarchia, con quali
//! stati obbligatori — e l'implementatore abbia qualcosa da seguire invece di
//! un aggettivo.
//!
//! I pattern sono un DATO nel DB (regola G): `nexus_ui_layout_patterns`, DB
//! META, seedata dalla migrazione. Aggiungere un pattern e' una riga, non un
//! deploy; niente catalogo hardcoded in questo file.
//!
//! Perche' una tabella e non la Knowledge Base: `knowledge_search` cerca in
//! `wiki_docs` con `scope = 'project' AND project_id = <progetto corrente>`
//! (vedi `knowledge.rs`). Un catalogo trasversale messo li' andrebbe seedato in
//! OGNI progetto — N copie dello stesso testo, che divergono alla prima
//! correzione: esattamente la duplicazione che la regola L vieta.

use serde_json::{json, Value};
use sqlx::{PgPool, Row};

/// Un pattern del catalogo. I campi sono il contratto verso l'agente: chi
/// aggiunge un pattern compila queste sezioni, non un testo libero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiLayoutPattern {
    pub key: String,
    /// Tipo di schermata ricorrente: `crud`, `dashboard`, `wizard`,
    /// `master_detail`, `settings`. E' un DATO, non un enum Rust: un tipo nuovo
    /// entra con una riga in tabella.
    pub app_type: String,
    pub title: String,
    /// Quando questo pattern e' quello giusto (e quando non lo e').
    pub when_to_use: String,
    /// Struttura e gerarchia visiva: quali zone, in che ordine, cosa domina.
    pub structure: String,
    /// Come si PRESENTA il pattern: densita', gerarchia del testo, comportamento
    /// a larghezza ridotta. Gemella di `structure` — quella dice dove stanno le
    /// zone, questa che aspetto hanno — e sta nella stessa scheda perche' chi
    /// chiede un pattern non deve ricevere meta' risposta. Criteri accertabili
    /// (misure, conteggi), mai aggettivi: il catalogo non e' un giudice di gusto.
    pub presentation: String,
    /// Stati OBBLIGATORI da rendere: vuoto, caricamento, errore, e gli altri
    /// che il pattern richiede. E' la parte verificabile del catalogo.
    pub required_states: String,
    /// Cosa NON fare: gli errori ricorrenti di questo pattern.
    pub anti_patterns: String,
}

impl UiLayoutPattern {
    fn from_row(r: &sqlx::postgres::PgRow) -> Self {
        Self {
            key: r.try_get("key").unwrap_or_default(),
            app_type: r.try_get("app_type").unwrap_or_default(),
            title: r.try_get("title").unwrap_or_default(),
            when_to_use: r.try_get("when_to_use").unwrap_or_default(),
            structure: r.try_get("structure").unwrap_or_default(),
            presentation: r.try_get("presentation").unwrap_or_default(),
            required_states: r.try_get("required_states").unwrap_or_default(),
            anti_patterns: r.try_get("anti_patterns").unwrap_or_default(),
        }
    }

    /// Riga di INDICE: quanto basta per scegliere, senza pagare il dettaglio di
    /// tutti i pattern in una volta.
    fn to_index_value(&self) -> Value {
        json!({
            "key": self.key,
            "app_type": self.app_type,
            "title": self.title,
            "when_to_use": self.when_to_use,
        })
    }

    /// Scheda COMPLETA del pattern.
    pub fn to_value(&self) -> Value {
        json!({
            "key": self.key,
            "app_type": self.app_type,
            "title": self.title,
            "when_to_use": self.when_to_use,
            "structure": self.structure,
            "presentation": self.presentation,
            "required_states": self.required_states,
            "anti_patterns": self.anti_patterns,
        })
    }
}

/// Errore di lettura del catalogo. Tipizzato (regola M): il chiamante distingue
/// "il DB non risponde" da "il catalogo e' vuoto" senza leggere un messaggio.
#[derive(Debug)]
pub enum UiPatternError {
    /// Query fallita: DB irraggiungibile o tabella assente (migrazione non
    /// applicata).
    Db(String),
}

impl std::fmt::Display for UiPatternError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(e) => write!(f, "catalogo pattern di layout non leggibile: {e}"),
        }
    }
}

/// Colonne lette, in un posto solo: l'ordine e i nomi valgono per entrambe le
/// query, cosi' non possono divergere.
const PATTERN_COLUMNS: &str =
    "key, app_type, title, when_to_use, structure, presentation, required_states, anti_patterns";

/// Carica i pattern attivi, opzionalmente filtrati per `app_type`.
/// PUNTO UNICO della lettura del catalogo: il tool, e chiunque dopo di lui,
/// passano di qui.
///
/// Ordine deterministico (`app_type, key`): due chiamate identiche danno la
/// stessa risposta, e il parere di una figura e' riproducibile.
pub async fn load_patterns(
    db: &PgPool,
    app_type: Option<&str>,
) -> Result<Vec<UiLayoutPattern>, UiPatternError> {
    let sql = format!(
        "SELECT {PATTERN_COLUMNS} FROM nexus_ui_layout_patterns \
         WHERE is_active = TRUE AND ($1::text IS NULL OR app_type = $1) \
         ORDER BY app_type, key"
    );
    let rows = sqlx::query(&sql)
        .bind(app_type.map(str::trim).filter(|s| !s.is_empty()))
        .fetch_all(db)
        .await
        .map_err(|e| UiPatternError::Db(e.to_string()))?;
    Ok(rows.iter().map(UiLayoutPattern::from_row).collect())
}

/// `ui_layout_patterns` — catalogo dei riferimenti di layout.
///
/// Input: `{ app_type?: "crud"|"dashboard"|... }`.
/// Senza `app_type` ritorna l'INDICE (chiave, tipo, titolo, quando usarlo): si
/// sceglie prima, si legge il dettaglio dopo. Con `app_type` ritorna le schede
/// complete di quel tipo.
///
/// Un catalogo vuoto non e' un errore da inghiottire: lo dice, cosi' chi legge
/// sa che la risposta e' "non c'e' nulla", non "non ho cercato" (regola M).
pub async fn tool_ui_layout_patterns(db: &PgPool, input: &Value) -> String {
    let app_type = input
        .get("app_type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let patterns = match load_patterns(db, app_type).await {
        Ok(p) => p,
        Err(e) => return json!({ "error": e.to_string() }).to_string(),
    };

    if patterns.is_empty() {
        return json!({
            "patterns": [],
            "count": 0,
            "message": match app_type {
                Some(t) => format!(
                    "nessun pattern per app_type='{t}'; richiama senza app_type per vedere \
                     i tipi disponibili"
                ),
                None => "catalogo dei pattern di layout vuoto".to_string(),
            },
        })
        .to_string();
    }

    match app_type {
        // Dettaglio: si e' gia' scelto il tipo.
        Some(t) => json!({
            "app_type": t,
            "patterns": patterns.iter().map(UiLayoutPattern::to_value).collect::<Vec<_>>(),
            "count": patterns.len(),
        })
        .to_string(),
        // Indice: elenco leggero + i tipi disponibili, per la seconda chiamata.
        None => {
            let mut app_types: Vec<&str> = patterns.iter().map(|p| p.app_type.as_str()).collect();
            app_types.dedup();
            json!({
                "patterns": patterns.iter().map(UiLayoutPattern::to_index_value).collect::<Vec<_>>(),
                "count": patterns.len(),
                "app_types": app_types,
                "hint": "richiama con app_type=<tipo> per la scheda completa \
                         (struttura, stati obbligatori, anti-pattern)",
            })
            .to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(key: &str, app_type: &str) -> UiLayoutPattern {
        UiLayoutPattern {
            key: key.to_string(),
            app_type: app_type.to_string(),
            title: format!("titolo {key}"),
            when_to_use: "quando serve".to_string(),
            structure: "zona A sopra, zona B sotto".to_string(),
            presentation: "righe di altezza unica, numeri a destra".to_string(),
            required_states: "vuoto, caricamento, errore".to_string(),
            anti_patterns: "niente tabella senza stato vuoto".to_string(),
        }
    }

    /// L'indice porta quanto basta a scegliere e NON il dettaglio: e' la
    /// ragione per cui esistono due forme invece di una.
    #[test]
    fn indice_non_include_il_dettaglio() {
        let v = pattern("lista_crud", "crud").to_index_value();
        assert_eq!(v["key"], json!("lista_crud"));
        assert_eq!(v["when_to_use"], json!("quando serve"));
        assert!(v.get("structure").is_none(), "l'indice resta leggero: {v}");
        assert!(v.get("presentation").is_none());
        assert!(v.get("required_states").is_none());
    }

    /// La scheda completa porta le quattro sezioni che rendono il pattern
    /// applicabile: senza queste il catalogo tornerebbe a essere un aggettivo.
    /// `presentation` e' la piu' recente (mig 0655) e la piu' facile da perdere
    /// per strada: descrive la RESA, che prima non era detta da nessuno — ne'
    /// dal catalogo, che parlava solo di struttura, ne' dalla lente della figura.
    #[test]
    fn scheda_completa_porta_struttura_resa_stati_e_antipattern() {
        let v = pattern("dashboard_kpi", "dashboard").to_value();
        assert_eq!(v["structure"], json!("zona A sopra, zona B sotto"));
        assert_eq!(
            v["presentation"],
            json!("righe di altezza unica, numeri a destra")
        );
        assert_eq!(v["required_states"], json!("vuoto, caricamento, errore"));
        assert_eq!(
            v["anti_patterns"],
            json!("niente tabella senza stato vuoto")
        );
    }
}
