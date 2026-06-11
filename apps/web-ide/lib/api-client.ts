// Barrel del client API.
//
// Questo file e' stato modularizzato per dominio in lib/api/* (audit
// best-practice dimensione file). L'API pubblica resta INVARIATA: ogni import
// esistente `from "@/lib/api-client"` (o path relativo) continua a funzionare
// perche' qui ri-esportiamo tutto.
//
// Helper condivisi (base URL, wrapper fetch, gestione errori) vivono in un
// unico punto: lib/api/_shared.ts (regola H, nessuna duplicazione).
//
// Per aggiungere nuove funzioni: crearle nel modulo di dominio appropriato
// sotto lib/api/ (o crearne uno nuovo) e aggiungere qui il relativo
// `export * from "./api/<modulo>"`.

export * from "./api/_shared";
export * from "./api/system";
export * from "./api/admin-settings";
export * from "./api/admin-sudo";
export * from "./api/prompts";
export * from "./api/chat";
export * from "./api/agent";
export * from "./api/projects";
export * from "./api/workspace";
export * from "./api/runtime";
export * from "./api/git";
export * from "./api/mcp-plugins";
export * from "./api/billing";
export * from "./api/profiles";
export * from "./api/quality";
export * from "./api/models";
export * from "./api/admin-users";
export * from "./api/project-db";
export * from "./api/knowledge";
export * from "./api/meta-docs";
