fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Use pre-installed protoc binary
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME"))?;
    let protoc = std::path::PathBuf::from(&home).join(".local/protoc/bin/protoc.exe");
    if protoc.exists() {
        std::env::set_var("PROTOC", &protoc);
    }

    let protos = [
        "../../proto/neural_core.proto",
        "../../proto/mcp_service.proto",
        "../../proto/tool_runner.proto",
        "../../proto/agent_router.proto",
    ];
    // Rigenera i binding quando un .proto cambia: senza questo cargo usava la
    // cache e i nuovi rpc/message (es. ClassifyError) non comparivano nel
    // codice generato.
    for p in &protos {
        println!("cargo:rerun-if-changed={p}");
    }
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&protos, &["../../proto"])?;
    Ok(())
}
