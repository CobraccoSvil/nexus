pub mod neural {
    tonic::include_proto!("ai_orchestrator.neural");
}

pub mod mcp {
    tonic::include_proto!("ai_orchestrator.mcp");
}

pub mod tool_runner {
    tonic::include_proto!("ai_orchestrator.tool_runner");
}

pub mod agent_router {
    tonic::include_proto!("ai_orchestrator.agent_router");
}
