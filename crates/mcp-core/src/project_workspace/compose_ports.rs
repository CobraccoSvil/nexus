//! Mapping porte docker-compose coerente (PUNTO UNICO, regola L).
//!
//! Problema: il `docker-compose.yml` di un progetto mappa tipicamente
//! `${PORT_FRONTEND:-20001}:${PORT_FRONTEND:-20001}` (host = container con la
//! stessa variabile). Ma il processo DENTRO il container puo' ascoltare su una
//! porta FISSA diversa (es. `vite --port 20001` hardcoded), e la porta HOST deve
//! essere quella GESTITA del bucket di progetto (39550-39599), non un default
//! fuori bucket. Risultato senza intervento: mapping `39566:39566` mentre vite e'
//! su 20001 -> il servizio non risponde; oppure default `20001` fuori bucket.
//!
//! Soluzione deterministica (niente agente, niente modifiche ai file del
//! progetto): Nexus genera un OVERRIDE compose (`docker-compose.nexus.yml`) che
//! per ogni servizio applicativo mappa `host_bucket:porta_interna` e fissa la
//! variabile di porta al valore interno (cosi' anche i processi che leggono
//! `${PORT_X}` ascoltano sulla porta interna coerente col mapping). Si lancia con
//! `docker compose -f <compose> -f docker-compose.nexus.yml up`.
//!
//! Questo modulo contiene la logica PURA (parsing + render); l'allocazione delle
//! porte host dal bucket (async, registro) resta nel chiamante (wizard.rs).

/// Nome del file override generato da Nexus.
pub(super) const OVERRIDE_FILE: &str = "docker-compose.nexus.yml";

/// Un servizio del compose con i suoi mapping `ports:` grezzi (host_expr, container_expr).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ServicePorts {
    pub name: String,
    pub mappings: Vec<(String, String)>,
}

/// Entry dell'override per un servizio: i mapping riscritti e le variabili di
/// porta da fissare nell'environment.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct OverrideEntry {
    pub service: String,
    pub ports: Vec<String>,         // "host_bucket:container"
    pub env: Vec<(String, String)>, // (VAR, "container")
}

/// Estrae il numero di porta "default" da una espressione del compose:
/// - `"20001"` -> 20001
/// - `"${PORT_FRONTEND:-20001}"` -> 20001 (default dopo `:-`)
/// - `"${PORT_FRONTEND}"` -> None (nessun default)
pub(super) fn port_default(expr: &str) -> Option<u16> {
    let e = expr.trim().trim_matches(['"', '\'']).trim();
    if let Some(rest) = e.strip_prefix("${") {
        if let Some(inner) = rest.strip_suffix('}') {
            if let Some(idx) = inner.find(":-") {
                return inner[idx + 2..].trim().parse::<u16>().ok();
            }
            return None;
        }
    }
    e.parse::<u16>().ok()
}

/// Estrae il nome variabile da `"${VAR...}"` -> `Some("VAR")`, altrimenti `None`.
pub(super) fn port_var(expr: &str) -> Option<String> {
    let e = expr.trim().trim_matches(['"', '\'']).trim();
    let inner = e.strip_prefix("${")?.strip_suffix('}')?;
    let name = inner.split(":-").next()?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// True se una porta `n` e' nel range gestito dei progetti (bucket globale).
fn in_project_range(n: u16) -> bool {
    (20000..=39999).contains(&n)
}

/// Parser minimale (line-based, come `parse_compose_services`) dei mapping
/// `ports:` per servizio. Gestisce le forme comuni:
/// ```yaml
/// services:
///   frontend:
///     ports:
///       - "${PORT_FRONTEND:-20001}:${PORT_FRONTEND:-20001}"
///       - '5432:5432'
/// ```
pub(super) fn parse_service_ports(compose: &str) -> Vec<ServicePorts> {
    let mut out: Vec<ServicePorts> = Vec::new();
    let mut in_services = false;
    let mut services_indent = 0usize;
    let mut service_indent: Option<usize> = None;
    let mut cur: Option<ServicePorts> = None;
    let mut in_ports = false;
    let mut ports_indent = 0usize;

    let flush = |cur: &mut Option<ServicePorts>, out: &mut Vec<ServicePorts>| {
        if let Some(s) = cur.take() {
            if !s.mappings.is_empty() {
                out.push(s);
            }
        }
    };

    for raw in compose.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();

        if !in_services {
            if trimmed == "services:" {
                in_services = true;
                services_indent = indent;
            }
            continue;
        }

        // Nuova chiave top-level allo stesso livello di `services:` -> fine sezione.
        if indent <= services_indent {
            flush(&mut cur, &mut out);
            in_services = false;
            in_ports = false;
            service_indent = None;
            continue;
        }

        // Il primo livello di indentazione sotto `services:` definisce i servizi.
        let svc_lvl = *service_indent.get_or_insert(indent);

        if indent == svc_lvl && trimmed.ends_with(':') {
            // Inizio di un nuovo servizio.
            flush(&mut cur, &mut out);
            in_ports = false;
            let name = trimmed.trim_end_matches(':').trim().to_string();
            cur = Some(ServicePorts {
                name,
                mappings: Vec::new(),
            });
            continue;
        }

        if cur.is_none() {
            continue;
        }

        if indent > svc_lvl && trimmed == "ports:" {
            in_ports = true;
            ports_indent = indent;
            continue;
        }

        // Uscita dalla sezione ports (chiave allo stesso livello o inferiore).
        if in_ports && indent <= ports_indent && !trimmed.starts_with('-') {
            in_ports = false;
        }

        if in_ports && trimmed.starts_with('-') {
            let item = trimmed
                .trim_start_matches('-')
                .trim()
                .trim_matches(['"', '\'']);
            // Mapping host:container, incluse le forme `ip:host:container`.
            if let Some((host, container)) = split_mapping(item) {
                if let Some(s) = cur.as_mut() {
                    s.mappings.push((host.to_string(), container.to_string()));
                }
            }
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// Divide un mapping di porta nelle due componenti che ci interessano, host e
/// container, rispettando le `${...}`. Riconosce le due forme di Docker Compose:
///   - `host:container`         (2 campi)
///   - `ip:host:container`      (3 campi: l'IP di bind si scarta)
/// Ritorna None per qualunque altro numero di campi o se host/container sono
/// vuoti.
fn split_mapping(item: &str) -> Option<(&str, &str)> {
    // Posizioni dei ':' che NON sono dentro `${...}`.
    let bytes = item.as_bytes();
    let mut depth = 0i32;
    let mut seps: Vec<usize> = Vec::new();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            b':' if depth == 0 => seps.push(i),
            _ => {}
        }
    }
    // host:container -> un separatore; ip:host:container -> il primo delimita
    // l'IP, che scartiamo, e usiamo gli ultimi due campi.
    let (host_start, sep) = match seps.as_slice() {
        [only] => (0, *only),
        [_ip_sep, host_sep] => {
            let ip_sep = seps[0];
            (ip_sep + 1, *host_sep)
        }
        _ => return None,
    };
    let host = item[host_start..sep].trim();
    let container = item[sep + 1..].trim();
    if host.is_empty() || container.is_empty() {
        return None;
    }
    Some((host, container))
}

/// True se il mapping va riscritto da Nexus: la porta HOST e' una variabile
/// `${PORT*}` oppure un numero nel range progetti (applicativo). Le porte
/// standard/infra (es. `5432:5432` del db) NON vengono toccate.
fn is_managed_host(host_expr: &str) -> bool {
    if let Some(v) = port_var(host_expr) {
        if v.starts_with("PORT") {
            return true;
        }
    }
    port_default(host_expr)
        .map(in_project_range)
        .unwrap_or(false)
}

/// Un mapping applicativo da riscrivere: servizio, porta interna reale
/// (container) e variabili di porta da fissare. PURO: l'allocazione della porta
/// host (async, dal registro) e' fatta dal chiamante (wizard.rs), che poi
/// costruisce le `OverrideEntry`.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PlannedMapping {
    pub service: String,
    pub container: u16,
    pub vars: Vec<String>,
}

/// Seleziona i mapping APPLICATIVI da riscrivere (host gestito: `${PORT*}` o
/// numero nel range progetti) e ne determina la porta interna reale. Le porte
/// standard/infra (es. `5432:5432` del db) sono escluse e restano al compose base.
pub(super) fn planned_mappings(services: &[ServicePorts]) -> Vec<PlannedMapping> {
    let mut out: Vec<PlannedMapping> = Vec::new();
    for svc in services {
        for (host_expr, container_expr) in &svc.mappings {
            if !is_managed_host(host_expr) {
                continue;
            }
            // Porta interna reale: default del lato container, fallback al lato host.
            let container = match port_default(container_expr).or_else(|| port_default(host_expr)) {
                Some(c) => c,
                None => continue,
            };
            let mut vars: Vec<String> = Vec::new();
            for v in [port_var(host_expr), port_var(container_expr)]
                .into_iter()
                .flatten()
            {
                if !vars.contains(&v) {
                    vars.push(v);
                }
            }
            out.push(PlannedMapping {
                service: svc.name.clone(),
                container,
                vars,
            });
        }
    }
    out
}

/// Render YAML dell'override docker-compose.
pub(super) fn render_override_yaml(entries: &[OverrideEntry]) -> String {
    let mut s = String::from(
        "# Generato automaticamente da Nexus — NON modificare a mano.\n\
         # Mapping porte coerente: host = porta gestita del bucket di progetto,\n\
         # container = porta interna reale del servizio. Override applicato con\n\
         # `docker compose -f docker-compose.yml -f docker-compose.nexus.yml up`.\n\
         services:\n",
    );
    for e in entries {
        s.push_str(&format!("  {}:\n", e.service));
        s.push_str("    ports:\n");
        for p in &e.ports {
            s.push_str(&format!("      - \"{p}\"\n"));
        }
        if !e.env.is_empty() {
            s.push_str("    environment:\n");
            for (k, v) in &e.env {
                s.push_str(&format!("      {k}: \"{v}\"\n"));
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPOSE: &str = r#"
services:
  db:
    image: postgres:15-alpine
    ports:
      - '5432:5432'
  backend:
    build:
      context: .
    ports:
      - "${PORT_BACKEND:-20002}:${PORT_BACKEND:-20002}"
    environment:
      - PORT=${PORT_BACKEND:-20002}
  frontend:
    ports:
      - "${PORT_FRONTEND:-20001}:${PORT_FRONTEND:-20001}"
volumes:
  db_data:
"#;

    #[test]
    fn port_default_estrae_il_default() {
        assert_eq!(port_default("20001"), Some(20001));
        assert_eq!(port_default("${PORT_FRONTEND:-20001}"), Some(20001));
        assert_eq!(port_default("\"${PORT_BACKEND:-20002}\""), Some(20002));
        assert_eq!(port_default("${PORT_FRONTEND}"), None);
    }

    #[test]
    fn port_var_estrae_il_nome() {
        assert_eq!(
            port_var("${PORT_FRONTEND:-20001}").as_deref(),
            Some("PORT_FRONTEND")
        );
        assert_eq!(port_var("${PORT_BACKEND}").as_deref(), Some("PORT_BACKEND"));
        assert_eq!(port_var("5432"), None);
    }

    #[test]
    fn parse_servizi_e_mapping() {
        let svcs = parse_service_ports(COMPOSE);
        let names: Vec<&str> = svcs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["db", "backend", "frontend"]);
        let fe = svcs.iter().find(|s| s.name == "frontend").unwrap();
        assert_eq!(fe.mappings.len(), 1);
        assert_eq!(fe.mappings[0].1, "${PORT_FRONTEND:-20001}");
    }

    #[test]
    fn planned_rimappa_solo_applicativi_non_il_db() {
        let svcs = parse_service_ports(COMPOSE);
        let plans = planned_mappings(&svcs);
        // db (5432:5432, porta standard) NON deve essere riscritto.
        assert!(!plans.iter().any(|p| p.service == "db"));
        // backend e frontend: porta interna = default del compose (20002/20001).
        let be = plans.iter().find(|p| p.service == "backend").unwrap();
        assert_eq!(be.container, 20002);
        assert!(be.vars.contains(&"PORT_BACKEND".to_string()));
        let fe = plans.iter().find(|p| p.service == "frontend").unwrap();
        assert_eq!(fe.container, 20001);
        assert!(fe.vars.contains(&"PORT_FRONTEND".to_string()));
    }

    #[test]
    fn render_yaml_valido() {
        let entries = vec![OverrideEntry {
            service: "frontend".to_string(),
            ports: vec!["39566:20001".to_string()],
            env: vec![("PORT_FRONTEND".to_string(), "20001".to_string())],
        }];
        let y = render_override_yaml(&entries);
        assert!(y.contains("services:"));
        assert!(y.contains("  frontend:"));
        assert!(y.contains("- \"39566:20001\""));
        assert!(y.contains("PORT_FRONTEND: \"20001\""));
    }

    #[test]
    fn split_mapping_rispetta_le_variabili() {
        assert_eq!(split_mapping("39566:20001"), Some(("39566", "20001")));
        assert_eq!(
            split_mapping("${PORT_FRONTEND:-20001}:${PORT_FRONTEND:-20001}"),
            Some(("${PORT_FRONTEND:-20001}", "${PORT_FRONTEND:-20001}"))
        );
    }

    #[test]
    fn split_mapping_gestisce_ip_host_container() {
        // Forma a 3 campi: l'IP di bind si scarta, restano host e container.
        assert_eq!(
            split_mapping("127.0.0.1:8080:80"),
            Some(("8080", "80")),
            "ip:host:container -> (host, container)"
        );
        // Con variabile nella porta host, sempre a 3 campi.
        assert_eq!(
            split_mapping("127.0.0.1:${PORT_API:-20005}:3000"),
            Some(("${PORT_API:-20005}", "3000"))
        );
        // Un solo campo o quattro campi non sono un mapping valido.
        assert_eq!(split_mapping("8080"), None);
        assert_eq!(split_mapping("a:b:c:d"), None);
    }

    #[test]
    fn parse_vede_il_servizio_pubblicato_su_ip_esplicito() {
        // Il caso del finding: un servizio che pubblica su 127.0.0.1 non deve
        // sparire dal pannello Porte / enforcement.
        let compose = "\
services:
  api:
    ports:
      - \"127.0.0.1:8080:80\"
";
        let out = parse_service_ports(compose);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "api");
        assert_eq!(out[0].mappings, vec![("8080".to_string(), "80".to_string())]);
    }
}
