// Punto unico (regola L) per la classificazione "file binario non testuale".
//
// Un documento generato da nexus_doc_generate viene salvato come .docx (ZIP
// Office Open XML): non e' decodificabile come testo UTF-8. Aprirlo nell'editor
// di codice (che legge il contenuto come stringa via /api/projects/:id/files)
// produce l'errore 400 "Impossibile leggere il file come testo UTF-8".
//
// Tutti i call site che decidono "questo file va aperto nell'editor o gestito
// come binario" devono delegare a queste funzioni invece di duplicare la lista
// di estensioni: openFileInGroup (ide-shell), DocumentsSidebar, eventuali
// future viste file.

// Estensioni di documenti/binari che NON vanno aperti come testo nell'editor.
const BINARY_DOC_EXTENSIONS = new Set([
  "docx",
  "doc",
  "xlsx",
  "xls",
  "pptx",
  "ppt",
  "pdf",
  "odt",
  "ods",
  "odp",
]);

/** Estensione (lowercase, senza punto) dell'ultimo segmento del path, o "". */
export function fileExtension(path: string): string {
  const name = path.split("/").pop() ?? path;
  const dot = name.lastIndexOf(".");
  if (dot <= 0) return "";
  return name.slice(dot + 1).toLowerCase();
}

/**
 * True se il path punta a un documento binario (Office/PDF) che non puo' essere
 * letto come testo UTF-8 e quindi non va aperto nell'editor di codice.
 */
export function isBinaryDocPath(path: string): boolean {
  return BINARY_DOC_EXTENSIONS.has(fileExtension(path));
}
