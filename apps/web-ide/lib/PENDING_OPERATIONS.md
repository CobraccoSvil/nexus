# Sistema di Operazioni Pendenti

Questo sistema previene la perdita di dati e incoerenze quando gli utenti navigano via mentre operazioni API sono in corso.

## Funzionalità

✅ **Alert automatico**: Se navighi mentre un'operazione è in corso, viene mostrato un alert di conferma
✅ **Abort automatico**: Se confermato, tutte le richieste in sospeso vengono cancellate
✅ **Protezione su refresh**: Se l'utente prova a chiudere la pagina, riceve un avvertimento
✅ **Tracciamento globale**: Tutte le operazioni pendenti sono visibili nei log

## Come Usare

### Opzione 1: useTrackedApi Hook (CONSIGLIATO)



### Opzione 2: usePendingOperations Hook (Manuale)



## Comportamento

### Quando l'utente naviga via durante un'operazione:

1. **Click su link/router.push()**: Mostra alert "Sono in corso X operazione(i). Vuoi annullarle e continuare?"
2. **Utente sceglie SÌ**: Le operazioni vengono cancellate con AbortController
3. **Utente sceglie NO**: Rimane sulla pagina

### Quando l'utente prova a chiudere la pagina:

- Se ci sono operazioni pendenti, browser mostra: "Sono in corso operazioni. Se abbandoni la pagina verranno annullate."

## Integrazione con api-client.ts

Se vuoi integrare il sistema globalmente in api-client.ts:



Ma attualmente è meglio usare il hook client-side come mostrato sopra.

## Log e Debug

Nel browser console vedrai log come:



## TypeScript

Tutti gli hook sono fully typed:


