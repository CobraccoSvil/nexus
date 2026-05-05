# Web-IDE Styling Guide

## Architettura CSS Moderna

Il progetto web-ide usa un sistema di styling a 3 livelli:

### 1. **CSS Globale** (`app/globals.css`)
Contiene:
- Reset e base styles
- Keyframes (animazioni)
- Utility classes per layout, spacing, typography, buttons, inputs
- Classe `.no-scrollbar` per nascondere scrollbar mantenendo scroll

### 2. **CSS Variables** (`app/theme-body.tsx`)
Tutti i colori del tema sono registrati come CSS custom properties:
```css
--color-bg              /* Background principale */
--color-bgCard          /* Background card/panel */
--color-bgInput         /* Background input/textarea */
--color-bgHover         /* Hover state */
--color-bgActive        /* Active state */
--color-bgHeader        /* Header background */
--color-bgSidebar       /* Sidebar background */
--color-border          /* Border color */
--color-text            /* Testo principale */
--color-textSecondary   /* Testo secondario */
--color-textMuted       /* Testo muted/disabled */
--color-accent          /* Color primario (blu/viola) */
--color-accentBg        /* Accent background */
--color-success         /* Colore success (verde) */
--color-error           /* Colore error (rosso) */
--color-warning         /* Colore warning (arancio) */
```

I colori **cambiano automaticamente** quando l'utente switcha tema dark/light. Nessun JS supplementare richiesto.

### 3. **Helper Functions** (`lib/styles.ts`)
Forniscono stili dinamici per componenti che hanno logica complessa:
```typescript
buttonStyles(tc, "primary" | "secondary" | "ghost")
inputStyle(tc)
cardStyle(tc, "sm" | "md")
```

---

## Come Usare le Utility Classes

### Flex & Layout

```tsx
// Flex row con spacing
<div className="flex-row-gap-8">
  <Icon />
  <span>Label</span>
</div>

// Flex column
<div className="flex-col-gap-12">
  <input className="input" />
  <button className="btn btn-primary">Submit</button>
</div>

// Flex + wrap
<div className="flex-row-wrap gap-12">
  {items.map(item => <Card key={item.id} />)}
</div>
```

### Spacing

```tsx
// Padding
<div className="px-3 py-2">Padded content</div>

// Margin auto (centering)
<div className="mx-auto">Centered</div>
```

### Cards

```tsx
// Card standard
<div className="card">
  <h3>Title</h3>
  <p>Content</p>
</div>

// Card small
<div className="card-sm">Small content</div>
```

### Buttons

```tsx
// Primary button
<button className="btn btn-primary" onClick={handleClick}>
  Save
</button>

// Secondary button
<button className="btn btn-secondary" onClick={handleCancel}>
  Cancel
</button>

// Ghost button (trasparente)
<button className="btn btn-ghost" onClick={handleMore}>
  More
</button>

// Disabled button
<button className="btn btn-primary" disabled>
  Saving...
</button>
```

### Inputs

```tsx
// Text input
<input className="input" type="text" placeholder="Enter text" />

// Textarea
<textarea className="input" rows={5} placeholder="Enter content" />

// Select
<select className="input">
  <option>Option 1</option>
</select>
```

### Typography

```tsx
// Font sizes
<p className="text-sm">Small text</p>
<p className="text-base">Base text</p>
<p className="text-lg">Large text</p>
<h1 className="text-3xl font-bold">Heading</h1>

// Font weights
<p className="font-normal">Normal weight</p>
<p className="font-semibold">Semibold</p>
<p className="font-bold">Bold</p>

// Text colors
<p className="text-muted">Muted text</p>
<p className="text-secondary">Secondary text</p>
<p className="text-accent">Accent text</p>
<p className="text-error">Error text</p>

// Text alignment
<p className="text-left">Left aligned</p>
<p className="text-center">Centered</p>
<p className="text-right">Right aligned</p>
```

### Display & Visibility

```tsx
// Display
<div className="block">Block element</div>
<span className="inline-block">Inline block</span>
<div className="hidden">Hidden</div>

// Positioning
<div className="relative">
  <div className="absolute">Positioned child</div>
</div>

// Size
<div className="w-full">Full width</div>
<div className="flex-1">Flex grow</div>

// Overflow
<div className="overflow-hidden">Clipped content</div>
<div className="overflow-auto">Scrollable content</div>
<div className="no-scrollbar" style={{ overflowY: "auto" }}>Hidden scrollbar</div>
```

### Other Utilities

```tsx
// Whitespace
<div className="whitespace-nowrap">No wrapping</div>
<div className="whitespace-pre-wrap">Preserve formatting</div>

// Cursor
<div className="cursor-pointer">Clickable</div>
<button className="cursor-not-allowed" disabled>Disabled</button>

// Opacity
<div className="opacity-50">50% opacity</div>
<div className="opacity-75">75% opacity</div>

// Transitions
<div className="transition-all">Smooth transitions</div>
```

---

## Pattern: Stili Statici + Colori Dinamici

La maggior parte dei componenti seguono questo pattern:

```tsx
// CLASSE: stili statici (layout, spacing, radius)
// INLINE: solo colori dinamici + valori state-dependent

<button
  className="btn btn-primary"  // ← Classe per padding, radius, font, transition
  style={{
    borderColor: saving ? tc.border : tc.accent,  // ← Inline per colori dinamici
    opacity: saving ? 0.7 : 1,                    // ← Inline per valori state
  }}
  disabled={saving}
>
  {saving ? "Saving..." : "Save"}
</button>
```

---

## Refactoring Checklist

Quando migri un componente da inline styles:

1. **Identifica stili statici** (padding, radius, font-size, gap, display, position)
2. **Estrai in className** usando le utility classes
3. **Mantieni inline** solo:
   - Colori (da `var(--color-*)` o `${tc.*}`)
   - Dimensioni dinamiche (width/height da state)
   - Valori condizionali (ternary, opacity dynamic)
   - Transforms (scale, rotate derivati da dati)

### Esempio: Migrazione di una Card

**PRIMA**:
```tsx
<div
  style={{
    border: `1px solid ${tc.border}`,
    borderRadius: 12,
    padding: 16,
    background: tc.bgCard,
    display: "flex",
    flexDirection: "column",
    gap: 10,
    marginBottom: 20,
  }}
>
  {children}
</div>
```

**DOPO**:
```tsx
<div className="card flex-col-gap-10" style={{ marginBottom: 20 }}>
  {children}
</div>
```

Riduzione: **8 proprietà inline → 0** (tutto in classi), margin rimane inline perché dinamico.

---

## CSS Variables nei Componenti

Se devi usare un colore del theme in CSS (es. in pseudo-elementi, gradients):

```tsx
<div
  className="custom-gradient"
  style={{
    background: `linear-gradient(to right, var(--color-accent), var(--color-accentBg))`
  }}
>
  Gradient content
</div>
```

Le CSS variables sono disponibili in ogni file senza import.

---

## Media Query (Responsive Design)

Per layout responsive basato su viewport:

1. **Se il break point è fisso** (es. `< 1100px`), usa **media query CSS**:
   ```css
   @media (max-width: 1099px) {
     .flex-row { flex-direction: column; }
   }
   ```

2. **Se il break point è dinamico** (calcolato in React), continua a usare **state + inline**:
   ```tsx
   const [compact, setCompact] = useState(false);
   
   <div className="flex-col" style={{ gap: compact ? 8 : 16 }}>
     ...
   </div>
   ```

---

## Best Practices

✅ **DO:**
- Usare utility classes per stili statici
- Mantieni colori in CSS variables
- Consolida stili ripetitivi in componenti o helper functions
- Mantieni code leggibile e scannerizzabile

❌ **DON'T:**
- Non mescolare inline styles e classi per lo stesso scopo (es. padding in entrambi)
- Non aggiungere colori hardcoded (usa `var(--color-*)`)
- Non creare nuove utility classes per casi singoli (usahelper function instead)
- Non usare `!important` (indica design flaw)

---

## Text Truncation con Tooltip

Quando un elemento mostra testo che potrebbe essere troncato per motivi di spazio responsive:

### Pattern Standard

```tsx
<span
  style={{
    overflow: "hidden",
    textOverflow: "ellipsis",
    whiteSpace: "nowrap",
    maxWidth: 200  // o qualunque vincolo di spazio
  }}
  title={fullText}  // ← SEMPRE aggiungi title per mostrare il testo completo
>
  {fullText}
</span>
```

### Con Utility Class

```tsx
<span className="truncate-ellipsis" style={{ maxWidth: 200 }} title={fullText}>
  {fullText}
</span>
```

### Helper Function

Per evitare ripetizione, importa da `lib/text-utils.ts`:

```tsx
import { getTruncatePropsFull, getTruncateTitle } from "../lib/text-utils";

// Variante 1: sempre aggiungi title (per path, hash, comandi)
<span {...getTruncatePropsFull(filePath)} className="truncate-ellipsis">
  {filePath}
</span>

// Variante 2: aggiungi title solo se testo lungo
<span {...getTruncateProps(filename, 30)} className="truncate-ellipsis">
  {filename}
</span>

// Variante 3: semplice
<span title={getTruncateTitle(text)} className="truncate-ellipsis">
  {text}
</span>
```

### Casi Comuni

**File path lungo:**
```tsx
<span title={finding.filePath} className="truncate-ellipsis" style={{ maxWidth: 300 }}>
  {finding.filePath}
</span>
```

**Nome sessione/progetto:**
```tsx
<span title={activeProject?.name} className="truncate-ellipsis" style={{ maxWidth: 220 }}>
  {activeProject?.name}
</span>
```

**Comando servizio:**
```tsx
<div
  title={s.command + (s.args?.length ? " " + s.args.join(" ") : "")}
  className="truncate-ellipsis"
>
  {s.command}{s.args?.length ? " " + s.args.join(" ") : ""}
</div>
```

### Multiline Truncation

Per testi su più righe che si troncano a 2 righe:

```tsx
<p className="truncate-multiline" title={longText}>
  {longText}
</p>
```

---

## Deploy Notes

- `globals.css` è servito una sola volta dal root layout
- CSS variables sono registrati dal component `ThemeBody` a runtime
- Theme switching (dark/light) aggiorna le CSS variables automaticamente
- Bundle CSS è shared da tutte le pagine (~2KB compresso)

