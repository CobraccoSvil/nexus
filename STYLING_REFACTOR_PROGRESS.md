# Styling Refactor Progress

## Stato corrente (aggiornato 2026-05-19, branch `chore/backlog-closure`)

**Riconteggio reale** (script `scripts/count-inline-styles.sh`):

  Totale inline styles attualmente:  **2884** in **92 file** `.tsx`
  (vs. baseline storica del piano: 1665 — il codice e' cresciuto)

I numeri precedenti ("446/1665 ridotti, 27%") non riflettono lo stato attuale del
codice: alcuni file dichiarati come completati nelle fasi 1-2 (es. `routing-config.tsx`,
`plugin-manager.tsx`, `chat-panel.tsx`, `ide-shell.tsx`) sono cresciuti ben oltre il
post-refactor a causa di feature aggiunte successivamente.

### Infrastruttura — disponibile e in uso

- CSS variables: `apps/web-ide/components/theme-body.tsx`
- Utility classes (120+): `apps/web-ide/app/globals.css`
- Helper functions: `apps/web-ide/lib/styles.ts`
- Theme switching dark/light: funzionante
- Build verification: `pnpm verify` chiude EXIT 0 al commit corrente

## Top 15 file per concentrazione inline styles

| count | file |
|---|---|
| 118 | `apps/web-ide/components/project-db/project-db-panel.tsx` |
| 117 | `apps/web-ide/app/admin/prompts/page.tsx` |
| 101 | `apps/web-ide/components/panels/run-panel.tsx` |
| 99  | `apps/web-ide/components/git/source-control-panel.tsx` |
| 94  | `apps/web-ide/components/sidebar/sidebar-manager.tsx` |
| 91  | `apps/web-ide/components/settings/routing-config.tsx` |
| 88  | `apps/web-ide/app/admin/profiles/page.tsx` |
| 84  | `apps/web-ide/components/settings/plugin-manager.tsx` |
| 81  | `apps/web-ide/components/chat/agent-steps-panel.tsx` |
| 80  | `apps/web-ide/components/chat-panel.tsx` |
| 79  | `apps/web-ide/components/settings/infrastructure-settings.tsx` |
| 70  | `apps/web-ide/components/ide-shell.tsx` |
| 67  | `apps/web-ide/components/panels/bottom-panel-manager.tsx` |
| 63  | `apps/web-ide/components/settings/provider-settings.tsx` |
| 63  | `apps/web-ide/app/page.tsx` |

Il top 15 concentra ~1300 inline styles (45% del totale).

## Strategia operativa rivista

Il refactor styling e' un **lavoro visivo**: cambiare stile inline → utility class
non e' una pura sostituzione testuale, perche':
- la specificita' CSS puo' creare regressioni invisibili al typecheck/build,
- l'ordine `style={{...}} + className` o solo `className` cambia il merge,
- alcune utility (es. `.flex-row`) hanno `align-items: center` implicito che
  puo' non corrispondere alle inline,
- regressioni di layout (overflow, gap, padding) sono visibili solo a browser.

Il piano `competent-wu-2db7bc` ha esplicitamente classificato Fase 5 come
"richiede preview server attivo per verifica" — ma CLAUDE.md vieta `preview_start`
("Tutto gira in locale su WSL"). Senza preview, refactor "alla cieca" sono
rischiosi e a bassissimo ROI per file.

**Conseguenza operativa**: la Fase 5 va affrontata in una sessione dedicata
con accesso a un browser di sviluppo locale (Next.js dev server avviato
manualmente dall'utente), batch da 5-10 file per commit, con verifica visiva
su tutte le rotte chiave (chat, ide-shell, admin/*, settings/*).

## Refactor Template (immutato)

```tsx
// BEFORE: All inline
<div
  style={{
    display: "flex",
    gap: 8,
    alignItems: "center",
    padding: "12px 14px",
    borderRadius: 6,
    border: `1px solid ${tc.border}`,
    background: tc.bgInput,
    color: tc.text,
  }}
>
  Content
</div>

// AFTER: Classes + Inline
<div
  className="flex-row-gap-8 px-3 py-2 rounded"
  style={{
    border: `1px solid ${tc.border}`,
    background: tc.bgInput,
    color: tc.text,
  }}
>
  Content
</div>
```

## Build Verification

Ogni file refactorizzato deve passare:
- `pnpm verify` exit 0 (CI=1 o senza)
- Visual regression test su tutte le viewport
- Theme switching (dark/light)

## Note

- CSS variables sono live: theme changes update automatici.
- Utility classes: ~2KB compressed, aggiunte una volta via globals.css.
- Refactor incrementale: non blocca altro lavoro.
- Stima risparmio: ~40-60KB minified JS bundle dopo refactor completo dei top 30 file.

## Raccomandazione per evitare regressione del progresso

Refactor sostenibile richiede:
- regola di review che imponga utility classes per pattern ripetuti (es.
  >3 occorrenze identiche di `display:flex; gap:N`),
- lint custom o `eslint-plugin-jsx-style` per bloccare nuove inline su
  patterns gia' coperti da utility,
- script `scripts/count-inline-styles.sh` come check periodico in CI.

## Tracker storico (sessioni precedenti)

Refactor parziali documentati:

- Fase 1 dichiarata complete su 5 file (admin-sidebar, session-tab-bar,
  routing-config, plugin-manager, prompts/page) — di questi `routing-config` e
  `plugin-manager` hanno oggi 91 e 84 styles, quindi sono ricresciuti dopo il
  refactor.
- Fase 2 dichiarata complete su 3 file (project-import-wizard, chat-panel,
  ide-shell) — `chat-panel` ha oggi 80, `ide-shell` ha 70.
- Fase 3 dichiarata in progress su `infrastructure-settings` — ora a 79 styles.

Il pattern (utility + theme variables) e' applicato in modo selettivo nei file
"completati" ma il tetto del codice e' cresciuto.
