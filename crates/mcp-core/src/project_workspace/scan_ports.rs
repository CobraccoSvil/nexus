//! Fix M1: parser auto-popola `nexus_port_allocations` dai metadata del progetto.
//!
//! Scansiona:
//! - package.json scripts.dev / scripts.start per `--port N` pattern
//! - vite.config.ts per `server.port = N`
//! - next.config.js per `PORT` env
//! - Procfile per `web: ... -p N`
//! - docker-compose.yml per `ports: - "N:M"`
//!
//! Per ogni porta rilevata fa UPSERT in nexus_port_allocations con label inferita.

// safety: tutte le `Regex::new("...").unwrap()` in questo modulo sono
// pattern literal hardcoded ammessi da CLAUDE.md §F. Refactor opportuno
// (LazyLock<Regex>) ma non e' una violazione.

use super::*;
use regex::Regex;

/// Regex `server.port = N` dei vite.config.{ts,js,mjs} (compilata una volta sola,
/// fuori dal loop sulle estensioni).
// safety: pattern literal valido
static VITE_PORT_RE: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"port\s*[:=]\s*(\d+)").unwrap());

/// Regex `ports: - "N:M"` dei docker-compose (compilata una volta sola,
/// fuori dal loop sui nomi file compose).
// safety: pattern literal valido
static COMPOSE_PORT_RE: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r#"-\s+"?(\d{4,5}):\d{2,5}"?"#).unwrap());

/// I 3 path package.json scansionati (root + frontend/ + backend/) con la
/// label canonica associata. Punto unico (regola L, S71) per il pattern
/// duplicato fra le versioni sync (`compute_detected_ports`) e async
/// (`auto_populate_port_allocations`).
fn package_json_path_labels(root: &std::path::Path) -> [(std::path::PathBuf, &'static str); 3] {
    [
        (root.join("package.json"), "app"),
        (root.join("frontend").join("package.json"), "frontend"),
        (root.join("backend").join("package.json"), "backend"),
    ]
}

/// Le 3 regex (--port=, "PORT": "...", PORT=...) per estrarre porte da
/// `package.json` con la label base passata.
fn package_json_regex_patterns(label_base: &'static str) -> Vec<(Regex, &'static str)> {
    vec![
        (Regex::new(r"--port[= ](\d+)").unwrap(), label_base),
        (Regex::new(r#""PORT"\s*:\s*"?(\d+)"?"#).unwrap(), label_base),
        (Regex::new(r"PORT=(\d+)").unwrap(), label_base),
    ]
}

/// Estrae le porte valide (1024..65535) da un blob di testo applicando una lista
/// di regex etichettate. Punto unico (regola L / ADR 0026, step S17) per la
/// logica di scansione condivisa fra le versioni sync e async di `scan_file`.
fn scan_content(content: &str, patterns: &[(Regex, &str)]) -> Vec<(i32, String)> {
    let mut found = Vec::new();
    for (re, label) in patterns {
        for cap in re.captures_iter(content) {
            if let Some(m) = cap.get(1) {
                if let Ok(p) = m.as_str().parse::<i32>() {
                    if (1024..65535).contains(&p) {
                        found.push((p, label.to_string()));
                    }
                }
            }
        }
    }
    found
}

/// Fix M31: scansiona il filesystem del progetto e ritorna le porte rilevate.
/// Helper sync senza dipendenze HTTP, usato da auto_populate_port_allocations.
/// Ritorna: Vec<(port, label, source)>
pub fn compute_detected_ports(root: &std::path::Path) -> Vec<(i32, String, String)> {
    let mut detected: Vec<(i32, String, String)> = Vec::new();

    fn scan_file(path: &std::path::Path, patterns: &[(Regex, &str)]) -> Vec<(i32, String)> {
        match std::fs::read_to_string(path) {
            Ok(s) => scan_content(&s, patterns),
            Err(_) => Vec::new(),
        }
    }

    // 1) package.json (root + frontend/ + backend/): punto unico S71.
    for (pkg, label_base) in package_json_path_labels(root) {
        if !pkg.is_file() {
            continue;
        }
        let patterns = package_json_regex_patterns(label_base);
        for (p, lbl) in scan_file(&pkg, &patterns) {
            detected.push((p, lbl, format!("package.json:{}", label_base)));
        }
    }

    // 2) vite.config.ts/js/mjs (frontend)
    for ext in &["ts", "js", "mjs"] {
        let p = root.join("frontend").join(format!("vite.config.{}", ext));
        if !p.is_file() {
            continue;
        }
        let patterns: Vec<(Regex, &str)> = vec![(VITE_PORT_RE.clone(), "frontend")];
        for (port, lbl) in scan_file(&p, &patterns) {
            detected.push((port, lbl, "vite.config".to_string()));
        }
    }

    // 3) Procfile
    let procfile = root.join("Procfile");
    if procfile.is_file() {
        let patterns: Vec<(Regex, &str)> = vec![
            (Regex::new(r"-p\s+(\d+)").unwrap(), "app"),
            (Regex::new(r"--port[= ](\d+)").unwrap(), "app"),
        ];
        for (port, lbl) in scan_file(&procfile, &patterns) {
            detected.push((port, lbl, "Procfile".to_string()));
        }
    }

    // 4) docker-compose.yml
    for name in &[
        "docker-compose.yml",
        "docker-compose.yaml",
        "compose.yml",
        "compose.yaml",
    ] {
        let p = root.join(name);
        if !p.is_file() {
            continue;
        }
        let patterns: Vec<(Regex, &str)> = vec![(COMPOSE_PORT_RE.clone(), "compose")];
        for (port, lbl) in scan_file(&p, &patterns) {
            detected.push((port, lbl, format!("compose:{}", name)));
        }
        break;
    }

    detected
}

/// Divide le porte rilevate nei sorgenti fra quelle registrabili come
/// allocazione di QUESTO progetto e quelle da scartare, deduplicando per
/// (porta, label). Funzione pura: la decisione si puo' guardare senza un DB.
///
/// Le porte qui vengono LETTE DAI SORGENTI, dove sono qualunque numero l'autore
/// del progetto abbia scritto. Registrarle senza chiedersi se appartengono al
/// bucket di questo progetto e' come dare per allocato cio' che si e' soltanto
/// trovato scritto: la riga in `nexus_port_allocations` vale poi come prova di
/// legittimita' davanti al linter e al port_enforcer, e chiude da sola la
/// violazione che l'ha prodotta. Criterio dal punto unico (regola L).
fn partiziona_porte_rilevate(
    project_id: &Uuid,
    detected: &[(i32, String, String)],
) -> (Vec<(i32, String)>, Vec<(i32, String)>) {
    let mut seen = std::collections::HashSet::new();
    let mut da_registrare = Vec::new();
    let mut scartate = Vec::new();
    for (port, label, _source) in detected {
        if !seen.insert((*port, label.clone())) {
            continue;
        }
        let registrabile = u16::try_from(*port).is_ok_and(|p| {
            crate::project_workspace::services::port_in_project_bucket(project_id, p)
        });
        if registrabile {
            da_registrare.push((*port, label.clone()));
        } else {
            scartate.push((*port, label.clone()));
        }
    }
    (da_registrare, scartate)
}

/// Scrive le allocazioni e ritorna quante righe NUOVE sono nate.
///
/// `allocation_mode` ha un vocabolario chiuso da un CHECK (mig 0114, esteso da
/// 0146 e 0434): auto | manual | dynamic | existing | adopted. Qui stava scritto
/// 'auto-detected', che quel CHECK non ammette: OGNI insert falliva, e l'errore
/// veniva inghiottito da un `unwrap_or(false)` che lo faceva sembrare "nessuna
/// riga nuova". Sul DB di sviluppo, infatti, di righe 'auto-detected' non ce
/// n'e' mai stata una. 'auto' e' il termine canonico per "rilevata
/// automaticamente" ed e' gia' quello che usa il rilevamento porta-da-output
/// (regola N: un solo identificatore per concetto).
async fn registra_porte(
    db: &sqlx::PgPool,
    project_id: Uuid,
    porte: &[(i32, String)],
) -> usize {
    let mut inserted = 0_usize;
    for (port, label) in porte {
        let res = sqlx::query(
            r#"
            INSERT INTO nexus_port_allocations (project_id, port, label, allocation_mode)
            VALUES ($1, $2, $3, 'auto')
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(project_id)
        .bind(port)
        .bind(label)
        .execute(db)
        .await;
        match res {
            Ok(r) if r.rows_affected() > 0 => inserted += 1,
            // ON CONFLICT DO NOTHING: la porta e' gia' registrata, non e' un errore.
            Ok(_) => {}
            // Un fallimento va DETTO. Inghiottirlo e' cio' che ha tenuto nascosto
            // per intero il malfunzionamento di questa funzione (regola H).
            Err(e) => tracing::warn!(
                project_id = %project_id,
                port,
                label = %label,
                error = %e,
                "insert dell'allocazione porta fallito"
            ),
        }
    }
    inserted
}

/// Fix M31: auto-popola la tabella `nexus_port_allocations` con le porte
/// rilevate scansionando il filesystem. Idempotente via ON CONFLICT DO NOTHING.
/// Chiamata da `register_project` come spawn-and-forget post-insert.
pub async fn auto_populate_port_allocations(
    db: &sqlx::PgPool,
    project_id: Uuid,
    project_root: &std::path::Path,
) {
    let detected = compute_detected_ports(project_root);
    if detected.is_empty() {
        tracing::debug!(
            "auto_populate_port_allocations: nessuna porta rilevata per {}",
            project_id
        );
        return;
    }
    let (da_registrare, scartate) = partiziona_porte_rilevate(&project_id, &detected);
    let inserted = registra_porte(db, project_id, &da_registrare).await;
    // Cio' che si scarta si dichiara: un conteggio che tace su meta' del lavoro
    // si legge come "tutto registrato" (regola O). Le porte scartate restano
    // visibili dove devono, cioe' come violazione sul sorgente che le contiene.
    let (bucket_start, bucket_end) =
        crate::project_workspace::services::project_bucket_range(&project_id);
    tracing::info!(
        project_id = %project_id,
        inserted,
        scartate = scartate.len(),
        bucket_start,
        bucket_end,
        "porte registrate; le scartate stanno fuori dal bucket e vanno migrate via request_port"
    );
    if !scartate.is_empty() {
        tracing::warn!(
            project_id = %project_id,
            porte = ?scartate,
            bucket_start,
            bucket_end,
            "porte trovate nei sorgenti ma fuori dal bucket del progetto: NON registrate"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// L'altra via per cui una porta altrui finiva nel registro: l'import di un
    /// progetto esistente. Le porte qui vengono LETTE DAI SORGENTI, dove sono
    /// qualunque numero l'autore abbia scritto, e venivano registrate tutte -
    /// anche una 3000 o una porta del bucket di un altro progetto. Da quel
    /// momento la riga faceva da prova di legittimita' e il linter taceva sul
    /// file che l'aveva prodotta.
    ///
    /// Il test attraversa il produttore vero (`compute_detected_ports` sul
    /// filesystem, poi l'INSERT reale sullo schema META) e guarda la
    /// conseguenza: quali righe esistono in `nexus_port_allocations`.
    ///
    /// Mutazione che rende rossa: togliere il filtro `port_in_project_bucket` da
    /// `auto_populate_port_allocations` -> la porta fuori bucket ricompare in
    /// tabella e la seconda asserzione cade.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn import_progetto_non_registra_le_porte_di_altri_bucket(pool: sqlx::PgPool) {
        let (_user, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let (bucket_start, bucket_end) =
            crate::project_workspace::services::project_bucket_range(&project_id);

        // Un progetto importato come se ne trovano: il frontend con una porta
        // scelta a mano (3000, per giunta quella della UI di Nexus), il backend
        // con una porta del bucket assegnato.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            "{\"scripts\":{\"dev\":\"vite --port 3000\"}}\n",
        )
        .expect("package.json radice");
        std::fs::create_dir_all(dir.path().join("backend")).expect("dir backend");
        std::fs::write(
            dir.path().join("backend").join("package.json"),
            format!("{{\"scripts\":{{\"dev\":\"node server.js --port {bucket_start}\"}}}}\n"),
        )
        .expect("package.json backend");

        // Premessa esplicita: il rilevamento le vede entrambe. Il filtro decide
        // che farne, e senza questa riga il test potrebbe passare per il motivo
        // sbagliato (nessuna porta trovata affatto).
        let rilevate = compute_detected_ports(dir.path());
        assert!(
            rilevate.iter().any(|(p, _, _)| *p == 3000)
                && rilevate.iter().any(|(p, _, _)| *p == bucket_start as i32),
            "il rilevamento deve vedere entrambe le porte: {rilevate:?}"
        );

        auto_populate_port_allocations(&pool, project_id, dir.path()).await;

        let righe: Vec<(i32, String)> = sqlx::query_as(
            "SELECT port, allocation_mode FROM nexus_port_allocations \
             WHERE project_id = $1 ORDER BY port",
        )
        .bind(project_id)
        .fetch_all(&pool)
        .await
        .expect("lettura allocazioni");
        let registrate: Vec<i32> = righe.iter().map(|(p, _)| *p).collect();

        assert!(
            registrate.contains(&(bucket_start as i32)),
            "la porta del bucket del progetto va registrata: {righe:?}"
        );
        // Il valore scritto deve stare nel vocabolario che il CHECK della tabella
        // ammette: qui c'era 'auto-detected', che il CHECK rifiuta, e ogni insert
        // falliva in silenzio. Girando sullo schema META reale (non su un CREATE
        // TABLE ricopiato) questo test lega il termine al vincolo.
        assert!(
            righe.iter().all(|(_, modo)| modo == "auto"),
            "allocation_mode fuori dal vocabolario della tabella: {righe:?}"
        );
        assert!(
            !registrate.contains(&3000),
            "3000 non appartiene al bucket {bucket_start}-{bucket_end}: registrarla la \
             farebbe passare per allocazione legittima e chiuderebbe da sola la \
             violazione sul package.json che la contiene. Righe: {registrate:?}"
        );
    }
}
