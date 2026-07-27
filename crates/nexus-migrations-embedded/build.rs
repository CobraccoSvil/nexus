fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // `sqlx::migrate!` (src/lib.rs) incorpora i file del set con `include_str!`:
    // MODIFICARE una migrazione esistente invalida la macro da sola, AGGIUNGERE
    // un file no — la macro non osserva la directory. Senza questa riga un nuovo
    // 00NN_*.sql non ricompila questo crate, i test girano sullo schema
    // precedente e restano verdi mentre la produzione usa gia' l'altro.
    //
    // La riga va QUI, nel crate che espande la macro: metterla nei crate
    // CONSUMATORI non serve a nulla (verificato rompendo apposta: con il
    // rerun-if-changed su mcp-core e nexus-agent-graph, l'aggiunta di una
    // migrazione lasciava la colonna invisibile ai test).
    println!("cargo:rerun-if-changed=../../db/migrations/project");
    println!("cargo:rerun-if-changed=../../db/migrations");
}
