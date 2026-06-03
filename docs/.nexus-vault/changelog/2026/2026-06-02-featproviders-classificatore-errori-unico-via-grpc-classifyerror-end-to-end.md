---
id: 4b21fae3-4cd8-44bb-9262-eb0b86262b36
kind: changelog
title: "feat(providers): classificatore errori UNICO via gRPC ClassifyError (end-to-end)"
slug: featproviders-classificatore-errori-unico-via-grpc-classifyerror-end-to-end
tags:
  - changelog
source_commit: 754e72f2a07880c67e7b9b35d169843f2cda0d67
source_files:
  - brain/grpc_server/generated/agent_router_pb2.py
  - brain/grpc_server/generated/agent_router_pb2_grpc.py
  - brain/grpc_server/generated/neural_core_pb2.py
  - brain/grpc_server/generated/neural_core_pb2_grpc.py
  - brain/grpc_server/neural_service.py
  - crates/mcp-core/src/model_health_probe.rs
  - crates/mcp-core/src/orchestrator.rs
  - crates/mcp-core/src/provider_health_probe.rs
  - crates/mcp-proto/build.rs
  - proto/neural_core.proto
auto_generated: true
created_at: 2026-06-02T17:52:04Z
updated_at: 2026-06-02T17:52:04Z
nexus_meta_version: 1
---

# feat(providers): classificatore errori UNICO via gRPC ClassifyError (end-to-end)

**Commit**: `754e72f2a07880c67e7b9b35d169843f2cda0d67` (2026-06-02 17:52 UTC)

**Significance**: 0.60

## File toccati

- `brain/grpc_server/generated/agent_router_pb2.py`
- `brain/grpc_server/generated/agent_router_pb2_grpc.py`
- `brain/grpc_server/generated/neural_core_pb2.py`
- `brain/grpc_server/generated/neural_core_pb2_grpc.py`
- `brain/grpc_server/neural_service.py`
- `crates/mcp-core/src/model_health_probe.rs`
- `crates/mcp-core/src/orchestrator.rs`
- `crates/mcp-core/src/provider_health_probe.rs`
- `crates/mcp-proto/build.rs`
- `proto/neural_core.proto`

## Cosa cambia

feat(providers): classificatore errori UNICO via gRPC ClassifyError (end-to-end)

## Riferimenti

- Vedi diff git: `git show 754e72f2a07880c67e7b9b35d169843f2cda0d67`

## Documenti correlati

- [[crates-rust]]
- [[brain-python]]
- [[rest-endpoints]]
- [[multi-provider-routing]]
- [[routing-matrix]]
