//! Semi condivisi dai test del crate.
//!
//! Esiste perche' `identita` era stata ricopiata: due moduli di test la
//! costruivano ciascuno per conto suo, e il gate di duplicazione l'ha vista
//! (regola L, che vale per i test quanto per la produzione — anzi di piu': un
//! helper divergente rende verdi due test che misurano condizioni diverse
//! credendo di misurare la stessa).

use sqlx::PgPool;
use uuid::Uuid;

use crate::Identity;

/// Un'identita' contabile REALE: le due colonne del ledger portano una FK,
/// quindi utente e progetto devono esistere davvero. Senza le righe vere il
/// seed passerebbe solo perche' il vincolo NOT NULL scatta prima della FK — un
/// verde che non dimostra nulla sullo schema (regola O).
pub(crate) async fn identita(pool: &PgPool) -> Identity {
    let team = Uuid::new_v4();
    let user = Uuid::new_v4();
    let project = Uuid::new_v4();
    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ($1, 'T', $2)")
        .bind(team)
        .bind(team.to_string())
        .execute(pool)
        .await
        .expect("insert team");
    sqlx::query("INSERT INTO users (id, email, display_name) VALUES ($1, $2, 'U')")
        .bind(user)
        .bind(format!("{user}@test.local"))
        .execute(pool)
        .await
        .expect("insert user");
    sqlx::query(
        "INSERT INTO projects (id, team_id, name, slug, owner_user_id) \
         VALUES ($1, $2, 'P', $3, $4)",
    )
    .bind(project)
    .bind(team)
    .bind(project.to_string())
    .bind(user)
    .execute(pool)
    .await
    .expect("insert project");
    Identity {
        user_id: user,
        project_id: project,
    }
}
