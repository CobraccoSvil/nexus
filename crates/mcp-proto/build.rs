fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Use pre-installed protoc binary
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME"))?;
    let protoc = std::path::PathBuf::from(&home).join(".local/protoc/bin/protoc.exe");
    if protoc.exists() {
        std::env::set_var("PROTOC", &protoc);
    }

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(
            &[
                "../../proto/neural_core.proto",
                "../../proto/mcp_service.proto",
                "../../proto/tool_runner.proto",
                "../../proto/agent_router.proto",
            ],
            &["../../proto"],
        )?;
    Ok(())
}
