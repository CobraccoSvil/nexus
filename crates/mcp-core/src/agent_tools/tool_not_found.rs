//! Punto unico (regola L) per la risoluzione di un tool NON trovato.
//!
//! Sostituisce il dizionario alias statico che viveva nel ramo `other =>` di
//! `dispatch::execute_agent_tool` e il messaggio secco del fallback
//! `nexus_builtin::execute`. Entrambi i call site delegano a
//! `resolve_tool_not_found`: l'unica logica che decide "cosa rispondere quando
//! un tool non esiste" vive qui, una sola volta.
//!
//! Concern coperti (vedi design GAP1/2/3/5):
//! - GAP1: l'output INIZIA SEMPRE con il marker `\u{274C}` cosi'
//!   `tool_runner_server` deriva `is_error = true` (niente piu' finto successo
//!   per i `nexus_*` inesistenti).
//! - GAP2: LOOKUP REALE (`search_builtin_tools` + ranking similarita' sul nome)
//!   al posto degli ~7 alias hardcoded: "forse intendevi: X, Y".
//! - GAP5: ponte verso i connettori installati (`mcp_server_tools`) e verso il
//!   catalog plugin non installato (`plugin_catalog_items`).
//! - GAP3: il nudge verso `nexus_mcp_tool_search` e' SEMPRE presente,
//!   indipendente dal flag `agent.tools.discovery_first_enabled` (M16 nel
//!   brain).
//!
//! Robustezza (regola H, sezione rischi del design): il resolver gira SOLO nel
//! path d'errore (tool inesistente), mai nell'hot path. Ogni query DB e' best-
//! effort: su fallimento degrada al messaggio base, mai panic, mantenendo
//! `is_error` coerente.

use std::time::Duration;

use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::nexus_builtin::mcp_runtime::{
    lookup_installed_tool_by_name, search_builtin_tools, semantic_search,
};
use crate::orchestrator::NeuralCoreClient;

/// Marker di errore richiesto dal contratto (`tool_runner_server.rs`: is_error
/// e' derivato da `result.trim_start().starts_with('\u{274C}')`). Eccezione
/// esplicita al divieto emoji nei sorgenti.
const ERR_MARK: char = '\u{274C}';

/// Numero massimo di suggerimenti builtin "forse intendevi" mostrati.
const MAX_BUILTIN_SUGGESTIONS: usize = 3;

/// Timeout duro per ogni lookup DB/semantico del resolver. Il resolver gira nel
/// path d'errore: se il DB e' lento/down NON deve bloccare la risposta del tool.
/// Su timeout si degrada (regola H: il path d'errore resta reattivo, mai appeso).
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(2);

/// Punto unico: risolve un nome di tool inesistente in un messaggio diagnostico
/// utile all'agente. SEMPRE prefissato con `\u{274C}` (gap1).
///
/// - `db`: pool Postgres (sempre disponibile; da `ctx.db` nel dispatch o dal
///   parametro `db` del fallback nexus_builtin).
/// - `neural`: `Some` dal dispatch (abilita il match semantico Qdrant best-
///   effort), `None` dal fallback nexus_builtin (solo builtin + DB ILIKE).
/// - `missing`: il nome del tool che non esiste.
pub(crate) async fn resolve_tool_not_found(
    db: &PgPool,
    neural: Option<&NeuralCoreClient>,
    user_id: Uuid,
    project_id: Uuid,
    user_role: &str,
    missing: &str,
) -> String {
    let _ = user_role; // riservato a futuri filtri per ruolo
    let missing = missing.trim();

    let mut out = format!("{ERR_MARK} Tool '{missing}' non esiste.");

    // ── (1) Connettore INSTALLATO/abilitato che fornisce il nome cercato ──────
    // Azionabile subito: l'agente puo' invocarlo via nexus_mcp_tool_call.
    let installed = tokio::time::timeout(
        LOOKUP_TIMEOUT,
        lookup_installed_tool_by_name(db, user_id, project_id, missing),
    )
    .await
    .unwrap_or_default();
    if let Some(top) = installed.first() {
        out.push_str(&format!(
            "\nIl tool '{}' e' fornito dal connettore '{}' (gia' installato). \
             Invocalo con nexus_mcp_tool_call({{\"server_id\": \"{}\", \"tool_name\": \"{}\", \"arguments\": {{...}}}}).",
            top.tool_name,
            top.server_name,
            top.server_id,
            top.tool_name,
        ));
        if let Some(desc) = &top.description {
            if !desc.trim().is_empty() {
                out.push_str(&format!(" ({})", first_line(desc)));
            }
        }
    }

    // ── (2) BUILTIN: "forse intendevi" (lookup reale, no alias hardcoded) ─────
    let builtin_suggestions = suggest_builtin(missing);
    if !builtin_suggestions.is_empty() {
        out.push_str("\nForse intendevi uno di questi tool builtin:");
        for (name, desc) in &builtin_suggestions {
            if desc.trim().is_empty() {
                out.push_str(&format!("\n  - {name}"));
            } else {
                out.push_str(&format!("\n  - {name}: {}", first_line(desc)));
            }
        }
    }

    // ── (2b) Match semantico best-effort (solo se neural disponibile) ─────────
    // Arricchisce i suggerimenti con i tool dei connettori semanticamente vicini
    // al nome cercato. Best-effort: se Qdrant e' down/errore -> ignorato.
    if neural.is_some() {
        let semantic = tokio::time::timeout(
            LOOKUP_TIMEOUT,
            semantic_search(db, missing, user_id, project_id, 3),
        )
        .await
        .unwrap_or_else(|_| Ok(Vec::new()));
        if let Ok(results) = semantic {
            let names: Vec<String> = results
                .iter()
                .filter_map(|r| {
                    let tn = r.get("tool_name").and_then(Value::as_str)?;
                    let sn = r.get("server_name").and_then(Value::as_str).unwrap_or("");
                    if tn.is_empty() {
                        None
                    } else if sn.is_empty() {
                        Some(tn.to_string())
                    } else {
                        Some(format!("{tn} (connettore {sn})"))
                    }
                })
                .take(3)
                .collect();
            if !names.is_empty() {
                out.push_str("\nTool affini disponibili nei connettori: ");
                out.push_str(&names.join(", "));
                out.push('.');
            }
        }
    }

    // ── (3) CATALOG plugin NON installato: informativo (install e' admin-only) ─
    let uninstalled = tokio::time::timeout(LOOKUP_TIMEOUT, lookup_uninstalled_catalog(db, missing))
        .await
        .unwrap_or(None);
    if let Some((slug, name)) = uninstalled {
        out.push_str(&format!(
            "\nIl tool '{missing}' sarebbe fornito dal connettore '{name}' (slug '{slug}'), \
             attualmente NON installato. Chiedi a un amministratore di installarlo dal pannello \
             Connettori; non e' invocabile finche' non e' installato."
        ));
    }

    // ── (4) Nudge SEMPRE verso tool_search (gap3, indipendente da M16) ────────
    out.push_str(&format!(
        "\nSe non sei certo del nome esatto, usa nexus_mcp_tool_search({{\"query\": \"{}\"}}) \
         per scoprire i tool disponibili. Per eseguire comandi shell usa run_command.",
        sanitize_query(missing)
    ));

    out
}

/// Costruisce i suggerimenti builtin "forse intendevi" combinando:
/// - il LOOKUP REALE su `search_builtin_tools` (registro AGENT_TOOLS_JSON,
///   tokenizzazione + ranking, gia' esistente: regola L);
/// - un secondo passaggio per i nomi storpiati che non producono token utili
///   (es. "redfile" -> "read_file"), ordinando i `name` del registro per
///   distanza editoriale (Levenshtein) e prendendo i piu' vicini.
///
/// Ritorna `(tool_name, description)` deduplicati, max `MAX_BUILTIN_SUGGESTIONS`.
fn suggest_builtin(missing: &str) -> Vec<(String, String)> {
    let mut ordered: Vec<(String, String)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // (a) Ricerca testuale tokenizzata sul registro builtin (riuso punto unico).
    for v in search_builtin_tools(missing, MAX_BUILTIN_SUGGESTIONS) {
        let name = v
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if name.is_empty() || !seen.insert(name.clone()) {
            continue;
        }
        let desc = v
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        ordered.push((name, desc));
    }

    // (b) Reranking per similarita' del NOME (cattura le storpiature che la
    //     tokenizzazione non raggiunge, es. "read_fil"/"redfile" -> read_file).
    if ordered.len() < MAX_BUILTIN_SUGGESTIONS {
        for (name, desc, dist) in builtin_names_by_similarity(missing) {
            if ordered.len() >= MAX_BUILTIN_SUGGESTIONS {
                break;
            }
            // Soglia: scarta candidati troppo distanti (rumore). Una distanza <=
            // meta' della lunghezza del nome mancante e' un buon compromesso.
            let max_dist = (missing.chars().count() / 2).max(2);
            if dist > max_dist {
                continue;
            }
            if seen.insert(name.clone()) {
                ordered.push((name, desc));
            }
        }
    }

    ordered
}

/// Scorre i `name` del registro AGENT_TOOLS_JSON e ritorna i candidati ordinati
/// per distanza di Levenshtein crescente rispetto a `missing`. Usato come
/// secondo passaggio del fuzzy quando la tokenizzazione non basta.
///
/// Ritorna `(name, description, distanza)`.
fn builtin_names_by_similarity(missing: &str) -> Vec<(String, String, usize)> {
    let tools_json: Value = match serde_json::from_str(crate::agent_tools::AGENT_TOOLS_JSON) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(arr) = tools_json.as_array() else {
        return Vec::new();
    };
    let needle = missing.to_ascii_lowercase();

    let mut scored: Vec<(String, String, usize)> = arr
        .iter()
        .filter_map(|t| {
            let name = t.get("name").and_then(Value::as_str)?;
            if name.is_empty() {
                return None;
            }
            let desc = t
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let dist = levenshtein(&needle, &name.to_ascii_lowercase());
            Some((name.to_string(), desc.to_string(), dist))
        })
        .collect();
    scored.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
    scored
}

/// Distanza di Levenshtein (edit distance) tra due stringhe, su `char`.
/// Implementazione iterativa a due righe (O(n*m) tempo, O(min) spazio). Niente
/// dipendenza esterna: il concern "distanza editoriale" non esiste altrove nel
/// crate e vive solo qui (regola L).
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr: Vec<usize> = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1) // cancellazione
                .min(curr[j] + 1) // inserimento
                .min(prev[j] + cost); // sostituzione
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Cerca un connettore del catalog plugin che fornirebbe `missing` ma che NON e'
/// installato. Il nome del tool, per i connettori `mode:'allowlist'`, vive in
/// `default_tool_policy->'tools'` (JSONB array); per i `mode:'all'` (allowlist
/// vuota) si ripiega sul match name/description del connettore. Best-effort: su
/// errore DB ritorna `None`.
///
/// Ritorna `(slug, name)` del connettore non installato.
async fn lookup_uninstalled_catalog(db: &PgPool, missing: &str) -> Option<(String, String)> {
    let missing = missing.trim();
    if missing.is_empty() {
        return None;
    }
    let like = format!("%{}%", missing.replace(['%', '_'], ""));
    let row = sqlx::query(
        r#"
        SELECT slug, name
        FROM plugin_catalog_items
        WHERE enabled = true
          AND id NOT IN (SELECT catalog_item_id FROM plugin_instances)
          AND (
            default_tool_policy->'tools' ? $1     -- nome esatto nell'allowlist
            OR name ILIKE $2                       -- fallback connettori mode='all'
            OR description ILIKE $2
          )
        ORDER BY
          (CASE WHEN default_tool_policy->'tools' ? $1 THEN 0 ELSE 1 END),
          name
        LIMIT 1
        "#,
    )
    .bind(missing)
    .bind(like)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()?;

    let slug: String = row.try_get("slug").ok()?;
    let name: String = row.try_get("name").ok()?;
    Some((slug, name))
}

/// Prima riga non vuota di una descrizione, troncata per non gonfiare l'output.
fn first_line(s: &str) -> String {
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() > 120 {
        let truncated: String = line.chars().take(117).collect();
        format!("{truncated}...")
    } else {
        line.to_string()
    }
}

/// Sanitizza il nome per inserirlo nella query JSON suggerita (evita di rompere
/// il JSON con virgolette/backslash).
fn sanitize_query(s: &str) -> String {
    s.replace(['\\', '"'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La distanza di Levenshtein riconosce le storpiature comuni.
    #[test]
    fn levenshtein_di_base() {
        assert_eq!(levenshtein("read_file", "read_file"), 0);
        assert_eq!(levenshtein("read_fil", "read_file"), 1);
        assert_eq!(levenshtein("redfile", "read_file"), 2);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
    }

    /// Il fuzzy sul nome storpiato propone il tool builtin corretto.
    #[test]
    fn fuzzy_suggerisce_read_file() {
        let s = suggest_builtin("read_fil");
        assert!(
            s.iter().any(|(n, _)| n == "read_file"),
            "read_fil dovrebbe suggerire read_file, ottenuti: {:?}",
            s.iter().map(|(n, _)| n).collect::<Vec<_>>()
        );
    }

    /// I 7 alias storici (coperti prima da tabella hardcoded) devono restare
    /// coperti dal fuzzy reale. Test parametrico di non-regressione (regola L).
    #[test]
    fn alias_storici_ancora_coperti() {
        let casi = [
            ("read", "read_file"),
            ("grep", "search_in_files"),
            ("write", "write_file"),
            ("git", "git_status"),
        ];
        for (storpiato, atteso) in casi {
            let s = suggest_builtin(storpiato);
            assert!(
                s.iter().any(|(n, _)| n == atteso),
                "alias '{storpiato}' dovrebbe suggerire '{atteso}', ottenuti: {:?}",
                s.iter().map(|(n, _)| n).collect::<Vec<_>>()
            );
        }
    }

    /// first_line tronca e prende la prima riga utile.
    #[test]
    fn first_line_tronca() {
        assert_eq!(first_line("\n  prima riga  \nseconda"), "prima riga");
        let lungo = "x".repeat(200);
        let r = first_line(&lungo);
        assert!(r.ends_with("..."));
        assert!(r.chars().count() <= 120);
    }

    /// sanitize_query rimuove i caratteri che romperebbero il JSON suggerito.
    #[test]
    fn sanitize_query_pulisce() {
        assert_eq!(sanitize_query("foo\"bar\\baz"), "foo bar baz");
    }

    /// Test di INTEGRAZIONE (richiede DATABASE_URL) che stampa il messaggio REALE
    /// del resolver per i casi tipici, contro il DB vero (connettori/catalog
    /// inclusi). `#[ignore]` perche' tocca il DB; eseguire con:
    ///   cargo test --bin mcp-core resolver_casi_reali -- --ignored --nocapture
    /// Verifica strutturale: ogni output inizia col marker U+274C (gap1).
    #[tokio::test]
    #[ignore]
    async fn resolver_casi_reali() {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://nexus:nexus@localhost:5433/nexus".to_string());
        let pool = PgPool::connect(&url).await.expect("connessione DB");
        // project_id Beauty-Book (scope per il lookup connettori/catalog).
        let project_id = Uuid::parse_str("73fa9139-50a4-4b17-9a3d-52fad931252d").unwrap();
        let user_id = Uuid::nil();
        let casi = [
            ("INESISTENTE (nome inventato)", "zxcvbnm_fake_tool"),
            ("NOME SBAGLIATO (typo di read_file)", "read_fil"),
            ("NOME SIMILE (vicino a list_files)", "list_file"),
            ("NOME SIMILE (vicino a delete_file)", "delete"),
        ];
        for (etichetta, missing) in casi {
            let out = resolve_tool_not_found(&pool, None, user_id, project_id, "user", missing).await;
            eprintln!("\n========== {etichetta}: input='{missing}' ==========\n{out}\n");
            assert!(out.starts_with(ERR_MARK), "manca il marker is_error per '{missing}'");
        }
    }
}
