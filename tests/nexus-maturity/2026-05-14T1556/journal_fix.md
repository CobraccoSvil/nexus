# Journal cumulativo dei fix applicati a Nexus durante il test di maturita

**Sessione TS**: 2026-05-14T1556
**Branch fix**: `test/nexus-maturity-2026-05-14T1556`
**Baseline IDEAI**: 77dd0929503f1b57cd627195c839f0d33cf95219

Ogni entry: categoria (A-H), file modificati, motivazione, esito post-restart.

---

## Fix #1 — pre-iter 1 (creazione progetto da UI)

**Categoria**: G (Frontend AI Workspace)
**Data**: 2026-05-14T16:24

### Sintomo
Dalla dialog "Progetti" del web-ide (apertura col bottone "⌘" della top navbar) NON e' possibile creare un nuovo progetto registrando una directory locale esistente. L'unico flow disponibile e' "Clone da GitHub". L'utente, per testare il flow standard di creazione progetto, non ha campi sufficienti.

### Diagnosi
Il componente `ProjectImportWizard` ([apps/web-ide/components/project-import-wizard.tsx:541](apps/web-ide/components/project-import-wizard.tsx:541)) esiste ed e' completo (directory browser, register, analyze, db-config), ma **non e' istanziato** in nessuna pagina/componente visibile. La prop `onRegister` di `ProjectSwitcher` ([apps/web-ide/components/project-switcher.tsx:49](apps/web-ide/components/project-switcher.tsx:49)) era dichiarata ma mai usata.

Inoltre i 3 nuovi tool nexus appena committati (`project_register_existing_dir.rs`, `project_register_from_git.rs`, `project_workspace_init.rs`) non hanno wiring lato UI ma sono indipendenti da questo fix.

### File modificati
- [apps/web-ide/components/project-switcher.tsx](apps/web-ide/components/project-switcher.tsx) — 4 hunk:
  1. Import di `ProjectImportWizard`
  2. Nuovo state `importWizardOpen`
  3. Sezione "Importa cartella locale" nella dialog projects con bottone che apre il wizard
  4. Render condizionale del `ProjectImportWizard` come overlay, con handler `onComplete` che chiude wizard, fa refresh lista progetti e switch al nuovo progetto

Nessuna modifica al backend o ai prop publici esistenti — modifica puramente additiva.

### Verifica
- `tsc --noEmit` sul workspace `apps/web-ide`: exit 0
- `git diff` mostra 45 righe aggiunte, 0 rimosse (eccetto cambio mode 100755→100644 di Linux)
- Rebuild web-ide via `./deploy/deploy-local.sh --web` (cache `.next/.turbo` ripulita)

### Esito atteso
Dopo rebuild, in `http://localhost:3000/ide` -> bottone "⌘" -> appare sezione "Importa cartella locale" con bottone "Importa cartella locale..." che apre il wizard (directory browser → analizza → conferma).

### Note per follow-up consolidamento
- I tool agente `project_register_*` rimangono non cablati lato UI ma sono richiamabili come tool da agenti Nexus stessi (consistent con il pattern "tool agente, non UI").
- Lo stub `chat-service:4020` resta da rimuovere o completare separatamente — non bloccante per il test.
- `dev-login` Next.js blocca in production (`NODE_ENV=production`) e `dev_login_server.py` ha `JWT_SECRET` hardcoded diverso dal DB — entrambi da consolidare in fix successivi.

---

## Fix #2 — pre-iter 1 (backdrop overlay wizard incompleto)

**Categoria**: G (Frontend AI Workspace)
**Data**: 2026-05-14T16:40

### Sintomo
Quando si apre il wizard "Importa progetto esistente" (cliccando "Importa cartella locale..." nella dialog projects), l'overlay scuro che dovrebbe oscurare la pagina sotto il modale NON copre tutta la pagina: la sidebar SOURCE CONTROL, l'Editor Workspace a destra, i pannelli Problemi/Terminale e la status bar in basso restano completamente visibili e cliccabili dietro il wizard.

L'utente lo ha notato a colpo d'occhio nel test UI automatico.

### Diagnosi
Il root container del `ProjectImportWizard` ([apps/web-ide/components/project-import-wizard.tsx:657](apps/web-ide/components/project-import-wizard.tsx:657)) usava `className="fixed inset-0 flex-row"` come se ci fosse Tailwind CSS. Il progetto **NON usa Tailwind** (`grep -r tailwindcss` nel web-ide ritorna 0 match, niente import `@tailwind` in `globals.css`). Quindi:
- `fixed` e `inset-0` erano stringhe inerti -> il div restava `position: static` in-flow nel suo parent (l'`<>` fragment dentro `ProjectSwitcher`)
- Il `background: "rgba(0,0,0,0.5)"` veniva applicato ma occupava solo l'area in-flow, non l'intera pagina

Altri modali del web-ide (es. `ProjectSwitcher`, [apps/web-ide/components/project-switcher.tsx:191-201](apps/web-ide/components/project-switcher.tsx:191)) usano correttamente inline style `position: "fixed", inset: 0` e funzionano bene.

Le altre utility classes nel wizard (`flex-col-gap-16`, `text-muted`, `text-base`, ecc.) sono definite in [apps/web-ide/app/globals.css](apps/web-ide/app/globals.css) e funzionano — solo `fixed` e `inset-0` mancavano.

### File modificati
- [apps/web-ide/components/project-import-wizard.tsx:657](apps/web-ide/components/project-import-wizard.tsx:657) — sostituito `className="fixed inset-0 flex-row"` con `style={{ position: "fixed", inset: 0, display: "flex", ... }}` mantenendo tutto il resto invariato.

### Verifica
- Rebuild `./deploy/deploy-local.sh --web` con `.next` e `.turbo` puliti
- Verifica visiva: apri "⌘" -> "Importa cartella locale..." -> backdrop oscura tutta la pagina

### Note per follow-up
Audit suggerito: `grep -rn 'className="[^"]*\b(fixed|inset-0|absolute|relative|flex|grid|hidden)\b' apps/web-ide/**/*.tsx` per individuare altri usi di utility Tailwind non supportate. Una regola lint custom (Fix categoria F futuro) potrebbe intercettarli prima del merge.

---
