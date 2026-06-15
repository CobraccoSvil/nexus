use crate::AppState;

// Tipi DTO e logica handler: punto unico in nexus_types::long_running_dto
// (regola L, S21 + cluster E6). I 4 wrapper axum (list/create/update/delete_pattern)
// sono generati dalla macro del punto unico per non duplicare il boilerplate fra
// admin-service e mcp-core.
nexus_types::long_running_axum_handlers!(AppState);
