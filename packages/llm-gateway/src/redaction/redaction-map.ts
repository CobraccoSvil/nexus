export interface RedactionEntry {
  placeholder: string;
  original: string;
  type: string;
  created_at: number;
}

// Mappa in-memoria per singola request: placeholder → valore originale.
// TTL breve (default 5 min) — la reidratazione avviene nella stessa request lifecycle.
export class RedactionMap {
  private entries = new Map<string, RedactionEntry>();
  private counter = 0;

  constructor(
    private readonly requestId: string,
    private readonly ttlMs: number = 300_000
  ) {}

  store(original: string, type: string): string {
    // Cerca se questo valore esiste già (dedup)
    for (const [placeholder, entry] of this.entries) {
      if (entry.original === original && entry.type === type) {
        return placeholder;
      }
    }

    const placeholder = `__NEXUS_${type.toUpperCase()}_${++this.counter}__`;
    this.entries.set(placeholder, {
      placeholder,
      original,
      type,
      created_at: Date.now(),
    });
    return placeholder;
  }

  rehydrate(text: string): string {
    const now = Date.now();
    let result = text;

    for (const [placeholder, entry] of this.entries) {
      if (now - entry.created_at > this.ttlMs) {
        this.entries.delete(placeholder);
        continue;
      }
      result = result.replaceAll(placeholder, entry.original);
    }

    return result;
  }

  size(): number {
    return this.entries.size;
  }

  getTypes(): string[] {
    return [...new Set([...this.entries.values()].map((e) => e.type))];
  }

  // Ritorna snapshot non modificabile per audit (senza valori originali)
  auditSnapshot(): { placeholder: string; type: string }[] {
    return [...this.entries.values()].map(({ placeholder, type }) => ({
      placeholder,
      type,
    }));
  }
}
