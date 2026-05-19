/**
 * Compatibility shim: i dati sono migrati in `lib/model-catalog.ts`.
 * Mantieni gli export per non rompere i callsite esistenti. Nuovi import
 * devono andare direttamente al modulo catalog.
 */

export { fallbackContextWindow, MODEL_CONTEXT_WINDOW } from "./model-catalog";
