//! Diagnostica temporanea: stampa i findings che lo scanner produce per un file.
//!
//! Il gate `xtask quality-scan` dice QUANTI findings, mai QUALI. Questo esempio
//! pone la stessa domanda al PUNTO UNICO che il gate interroga
//! (`mcp_quality::analyze_source`) invece di reimplementarne il criterio
//! (regola O).
//!
//! NOTA sullo scoping: il gate esclude i moduli `#[cfg(test)]` con
//! `scan::scoped_source`, che e' privato. Qui il file arriva GIA' tagliato dal
//! chiamante (`awk` fino al primo `#[cfg(test)]`) e il taglio e' dichiarato,
//! non nascosto: per un file con un solo modulo test in coda i due scoping
//! coincidono, e su un file diverso il conteggio va confrontato con il gate
//! prima di trarne conclusioni.
//!
//! Uso: cargo run -q -p mcp-quality --example findings_di_un_file -- <path.rs>

fn main() {
    let path = std::env::args().nth(1).expect("uso: <path.rs>");
    let src = std::fs::read_to_string(&path).expect("lettura file");
    let report = mcp_quality::analyze_source(&path, &src);
    println!("{}: {} findings", path, report.findings.len());
    for f in &report.findings {
        println!(
            "  [{}/{}] riga {:?} — {}",
            f.category, f.severity, f.line, f.title
        );
    }
}
