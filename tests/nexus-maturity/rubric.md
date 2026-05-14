# Rubrica maturita Nexus — sessione TS=

Per ogni dimensione: punteggio 0/1/2/3, evidenza, note.

0 = assente / immaturo
1 = parziale, con problemi
2 = buono, con minor issue
3 = eccellente, production-ready

| Dim | Cosa misura | Punteggio | Evidenza | Note |
|-----|-------------|-----------|----------|------|
| D1  | N iterazioni totali (1=ideale, 5+=immaturo) |  |  |  |
| D2  | Categorie fix necessari (solo A/B=medio, D/E/F=gap profondi) |  |  |  |
| D3  | Completezza PRD nell'iter finale (attori+UC+NFR) |  |  |  |
| D4  | Coerenza schema DB (entita PRD -> tabelle, FK consistenti) |  |  |  |
| D5  | `pnpm verify` (o equivalente) passa nell'iter finale |  |  |  |
| D6  | Conta violazioni qualita cumulative (hardcoded models, emoji, unwrap, payload log) |  |  |  |
| D7  | N loop sterili intercettati |  |  |  |
| D8  | Auto-correzione interna (Nexus risolve errori senza intervento, ratio su totale errori) |  |  |  |
| D9  | Contamination zero (`git-ideai-diff.patch` vuoto in tutte le iter) |  |  |  |
| D10 | Costo totale vs valore prodotto |  |  |  |
| D11 | Tempo totale fino a successo (o N_MAX se fallisce) |  |  |  |
| D12 | Riusabilita fix CC (codice production-quality, passa verify, niente regressioni) |  |  |  |

**Totale: X / 36**

**Maturita complessiva (interpretazione)**:
- 30-36: pronto per scenari production di project-generation
- 22-29: maturo per scenari guidati, gap mirati
- 14-21: immaturo, serve roadmap di hardening
- 0-13: lontano dall'obiettivo, scenari di project-generation richiedono ridisegno
