// ═══════════════════════════════════════════════════════════════════════════
// wiki/acl.rs — Permission middleware per il knowledge graph unificato.
//
// L'ACL e' applicata in UN solo punto (questo modulo) e governa sia le SELECT
// (via `scope_clause()`) sia le mutazioni (via `can_write`). Gli handler in
// `wiki/routes.rs` ricevono i `Claims` validati dal middleware `require_auth`
// (attached come `axum::Extension`), e costruiscono il `WikiAcl` chiamando
// `WikiAcl::from_claims(&state, &claims).await`.
//
// Decisione: i progetti di cui l'utente e' membro vengono risolti via JOIN
// con `project_members` (vedi crates/mcp-core/src/projects/mod.rs:776).
// L'admin globale (claims.role == "admin") salta il filtro e vede tutto.
// ═══════════════════════════════════════════════════════════════════════════

use crate::auth::Claims;
use crate::wiki::model::WikiDoc;
use crate::AppState;
use anyhow::{Context, Result};
use uuid::Uuid;

/// Snapshot dei permessi di un utente sul knowledge graph al momento della
/// richiesta. Costruito una volta per handler (vedi `from_claims`); nessuna
/// cache: il costo e' una query su `project_members`.
#[derive(Debug, Clone)]
pub struct WikiAcl {
    /// Identificativo utente, parsato dal `Claims.sub` (formato UUID).
    /// Tenuto come `String` perche' alcuni utenti storici hanno sub non-UUID;
    /// l'ACL non lo usa direttamente per filtrare (solo i project_ids contano).
    pub user_sub: String,
    pub is_admin: bool,
    pub project_ids: Vec<Uuid>,
}

impl WikiAcl {
    /// Costruisce l'ACL a partire dai `Claims` validati dal middleware.
    /// Esegue una query su `project_members` per popolare `project_ids`.
    pub async fn from_claims(state: &AppState, claims: &Claims) -> Result<Self> {
        let is_admin = claims.role == "admin";
        // `sub` puo' essere un UUID o uno user_id legacy. Se non e' UUID,
        // l'utente non avra' project_ids associati (e' lo stesso comportamento
        // dei vecchi handler `knowledge::routes::*`).
        let user_uuid = Uuid::parse_str(&claims.sub).ok();

        let project_ids: Vec<Uuid> = match user_uuid {
            Some(uid) => sqlx::query_scalar::<_, Uuid>(
                "SELECT project_id FROM project_members WHERE user_id = $1",
            )
            .bind(uid)
            .fetch_all(&state.db)
            .await
            .context("SELECT project_members per WikiAcl")?,
            None => Vec::new(),
        };

        Ok(WikiAcl {
            user_sub: claims.sub.clone(),
            is_admin,
            project_ids,
        })
    }

    /// Frammento `WHERE` parametrizzato per SELECT su `wiki_docs` (alias
    /// implicito = nome tabella). I parametri sono passati via il singolo bind
    /// `Vec<Uuid>` ritornato. Per gli admin il filtro e' `TRUE` (nessun bind).
    ///
    /// Esempio d'uso:
    /// ```ignore
    /// let (clause, projects) = acl.scope_clause(/* extra_param_idx = */ 1);
    /// let sql = format!("SELECT ... FROM wiki_docs WHERE {clause} AND ...");
    /// let q = sqlx::query_as::<_, WikiDoc>(&sql).bind(&projects);
    /// ```
    ///
    /// `extra_param_idx` indica l'indice di partenza dei placeholder $N (1-based).
    /// Restituisce `(clausola, bind_projects)`. Se admin: `("TRUE", vec![])`.
    pub fn scope_clause(&self, param_idx: usize) -> (String, Vec<Uuid>) {
        if self.is_admin {
            return ("TRUE".to_string(), Vec::new());
        }
        // Utente normale: vede meta-doc public + doc dei propri progetti.
        let clause = format!(
            "((wiki_docs.scope = 'meta' AND wiki_docs.public_read = TRUE) \
              OR (wiki_docs.scope = 'project' AND wiki_docs.project_id = ANY(${}::uuid[])))",
            param_idx
        );
        (clause, self.project_ids.clone())
    }

    /// Check write su uno specifico documento. Regole:
    /// - scope=meta -> solo admin scrive
    /// - scope=project -> solo membri del progetto (qualsiasi role) scrivono
    pub fn can_write(&self, doc: &WikiDoc) -> bool {
        if self.is_admin {
            return true;
        }
        match doc.scope.as_str() {
            "meta" => false,
            "project" => doc
                .project_id
                .map(|pid| self.project_ids.contains(&pid))
                .unwrap_or(false),
            _ => false,
        }
    }

    /// Check read su uno specifico documento (utile post-fetch quando la
    /// query non ha applicato `scope_clause`, es. lookup by primary key).
    pub fn can_read(&self, doc: &WikiDoc) -> bool {
        if self.is_admin {
            return true;
        }
        match doc.scope.as_str() {
            "meta" => doc.public_read,
            "project" => doc
                .project_id
                .map(|pid| self.project_ids.contains(&pid))
                .unwrap_or(false),
            _ => false,
        }
    }
}
