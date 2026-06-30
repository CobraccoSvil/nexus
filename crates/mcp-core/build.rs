fn main() {
    // Inietta il timestamp di build come env var accessibile con env!("BUILD_TIMESTAMP")
    // Questo valore cambia ad ogni compilazione — usato per verificare che il binario attivo
    // sia quello appena compilato (deploy check).
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string());
    println!("cargo:rustc-env=BUILD_TIMESTAMP={ts}");
    // Forza la ricompilazione se questo file cambia
    println!("cargo:rerun-if-changed=build.rs");
}
