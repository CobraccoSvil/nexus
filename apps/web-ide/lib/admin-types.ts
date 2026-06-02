// admin-types.ts — Tipi condivisi per le pagine admin (Fase G del piano).
// Centralizza le forme dati ricorrenti nelle pagine di amministrazione cosi'
// che i componenti condivisi (AdminPageHeader, ListEditorLayout) e gli hook
// (useAdminList) parlino lo stesso linguaggio.

export interface AdminListState<T> {
  items: T[];
  loading: boolean;
  error: string;
}

export interface AdminMutationState {
  saving: boolean;
  error: string;
}
