import { en } from "./dictionaries/en";

export type Locale = "en" | "it" | "es";

export type TranslationKey = keyof typeof en;

export type Dictionary = Record<TranslationKey, string>;

// `it` ed `es` possono omettere chiavi presenti in `en` (fallback runtime su `en`).
// Partial vieta i typo (chiavi sconosciute) ma tollera le omissioni preesistenti.
export type PartialDictionary = Partial<Dictionary>;
