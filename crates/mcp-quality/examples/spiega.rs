//! `cargo run -p mcp-quality --example spiega -- <file.rs>` — quali findings
//! produce lo scanner su UN file, con categoria, titolo e riga.
//!
//! Esiste perche' `xtask quality-scan` dichiara un DELTA («findings totali:
//! 7801 -> 7850») e chiede di ridurlo, ma non dice quali siano: la sua work-list
//! esporta i soli file con long-fn/complessita/security, e un peggioramento in
//! un'altra categoria resta senza nome. Senza un modo di porre la domanda, chi
//! diagnostica finisce per RICOSTRUIRE le regole a mano e misurare la propria
//! imitazione dello scanner (regola O): meglio aggiungere l'interrogazione.
//!
//! Chiama [`mcp_quality::analyze_source`], cioe' la stessa funzione che il gate
//! usa per contare — non una sua riscrittura. Non filtra i moduli `#[cfg(test)]`
//! che il gate esclude: qui la domanda e' «cosa vede lo scanner in questo file»,
//! e togliere righe le renderebbe invisibili proprio mentre le si cerca.

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("uso: cargo run -p mcp-quality --example spiega -- <file.rs>");
        std::process::exit(2);
    };
    let sorgente = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    };
    let report = mcp_quality::analyze_source(&path, &sorgente);
    for f in &report.findings {
        println!(
            "{:16} riga {:>5}  {}",
            f.category,
            f.line.map(|l| l.to_string()).unwrap_or_else(|| "-".into()),
            f.title
        );
    }
    println!("TOTALE {}", report.findings.len());
}
