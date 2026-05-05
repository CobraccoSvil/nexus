// Re-export da @nexus/shared — la logica risiede lì per evitare dipendenze circolari
export { SecretScanner } from "@nexus/shared";
export type { ScanResult, FoundPattern, PatternType } from "@nexus/shared";
