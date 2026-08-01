//! Tool `nexus_install_shadcn_components`: crea stub TSX dei componenti shadcn
//! piu' usati senza richiedere `npx shadcn add` (che spesso fallisce per peer
//! dependency, ENOENT su cache npx, o rebrand `shadcn-ui` -> `shadcn`).
//!
//! Caso d'uso: il modello (gemini-2.5-flash/pro) entrava in loop su
//! `npx shadcn-ui add button card input` con errori a cascata. Esponendo un
//! tool builtin che scrive stub funzionali, il modello evita 5+ iterazioni
//! di npm fallite e produce un'app che builda subito.
//!
//! Gli stub usano Tailwind classes minime, no dipendenza da @radix-ui o cva.
//! Producono UI funzionale (non bella, ma cliccabile/leggibile) che basta per
//! avviare la dev experience. L'utente puo' poi sostituirli con `shadcn add`
//! reale quando il setup npm e' stabile.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio::fs;

use super::ToolContextCore;

/// Componenti supportati e relativo contenuto stub. Lista deliberatamente
/// piccola: copre i componenti usati nel 95% delle dashboard tipiche
/// (login, tabella, form, alert). Per componenti rari, l'utente puo'
/// passare a `npx shadcn@latest add ...` manuale.
fn stub_content(name: &str) -> Option<&'static str> {
    match name {
        "button" => Some(BUTTON_TSX),
        "input" => Some(INPUT_TSX),
        "label" => Some(LABEL_TSX),
        "card" => Some(CARD_TSX),
        "alert" => Some(ALERT_TSX),
        "tabs" => Some(TABS_TSX),
        "table" => Some(TABLE_TSX),
        "badge" => Some(BADGE_TSX),
        "separator" => Some(SEPARATOR_TSX),
        "sonner" => Some(SONNER_TSX),
        "dialog" => Some(DIALOG_TSX),
        "dropdown-menu" => Some(DROPDOWN_TSX),
        "select" => Some(SELECT_TSX),
        "popover" => Some(POPOVER_TSX),
        "textarea" => Some(TEXTAREA_TSX),
        _ => None,
    }
}

const SUPPORTED_LIST: &[&str] = &[
    "button",
    "input",
    "label",
    "card",
    "alert",
    "tabs",
    "table",
    "badge",
    "separator",
    "sonner",
    "dialog",
    "dropdown-menu",
    "select",
    "popover",
    "textarea",
];

pub async fn tool_nexus_install_shadcn_components(
    ctx: &ToolContextCore,
    input: &Value,
) -> String {
    // components: array opzionale; se vuoto, installa il set di base
    let components: Vec<String> = match input.get("components") {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_ascii_lowercase()))
            .filter(|s| !s.is_empty())
            .collect(),
        _ => vec![
            "button".into(),
            "input".into(),
            "label".into(),
            "card".into(),
            "alert".into(),
            "tabs".into(),
            "sonner".into(),
        ],
    };

    // target_dir relativo al project root. Default: src/components/ui (convenzione shadcn).
    // Per progetti con src/app/components/ui (figma export) passare explicit.
    let target_rel = input
        .get("target_dir")
        .and_then(Value::as_str)
        .map(|s| s.trim().trim_start_matches('/'))
        .unwrap_or("src/components/ui");
    // Punto unico (regola L): de-duplica la root se l'agente l'ha inclusa nel
    // path e blocca il traversal ".." (normalize_into_root).
    let project_root: &Path = &ctx.root_path;
    let target_dir: PathBuf =
        match nexus_types::workspace_paths::normalize_into_root(project_root, target_rel) {
            Ok(clean) => project_root.join(&clean),
            Err(e) => {
                return crate::errore_json(format!("target_dir non valido: {}", e.message()))
            }
        };

    if let Err(e) = fs::create_dir_all(&target_dir).await {
        return crate::errore_json(format!(
            "creazione directory '{}' fallita: {e}",
            target_dir.display()
        ));
    }

    let mut written: Vec<String> = Vec::new();
    let mut skipped_existing: Vec<String> = Vec::new();
    let mut unsupported: Vec<String> = Vec::new();
    let overwrite = input
        .get("overwrite")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    for name in &components {
        let Some(content) = stub_content(name) else {
            unsupported.push(name.clone());
            continue;
        };
        let file_path = target_dir.join(format!("{name}.tsx"));
        let exists = fs::metadata(&file_path).await.is_ok();
        if exists && !overwrite {
            skipped_existing.push(name.clone());
            continue;
        }
        match fs::write(&file_path, content).await {
            Ok(()) => written.push(name.clone()),
            Err(e) => {
                tracing::warn!(
                    "nexus_install_shadcn_components: write {} fallita: {e}",
                    file_path.display()
                );
                unsupported.push(format!("{name} (write_error)"));
            }
        }
    }

    json!({
        "target_dir": target_rel,
        "written": written,
        "skipped_existing": skipped_existing,
        "unsupported": unsupported,
        "supported_full_list": SUPPORTED_LIST,
        "next_step": "import nei file applicativi: import { Button } from '@/components/ui/button' \
                       (o path relativo). Gli stub usano solo Tailwind classes base. Per UI ricca, \
                       sostituiscili con shadcn ufficiale quando npm install e' stabile."
    })
    .to_string()
}

// ── Stub TSX (Tailwind minimi, no @radix-ui, no cva) ────────────────────────

const BUTTON_TSX: &str = r#"import * as React from "react";

type Variant = "default" | "outline" | "ghost" | "destructive" | "secondary" | "link";
type Size = "default" | "sm" | "lg" | "icon";

interface Props extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  size?: Size;
  asChild?: boolean;
}

const variantClass: Record<Variant, string> = {
  default: "bg-blue-600 text-white hover:bg-blue-700",
  outline: "border border-gray-300 bg-white text-gray-900 hover:bg-gray-100",
  ghost: "bg-transparent text-gray-900 hover:bg-gray-100",
  destructive: "bg-red-600 text-white hover:bg-red-700",
  secondary: "bg-gray-200 text-gray-900 hover:bg-gray-300",
  link: "bg-transparent text-blue-600 underline-offset-4 hover:underline",
};

const sizeClass: Record<Size, string> = {
  default: "h-10 px-4 py-2 text-sm",
  sm: "h-8 px-3 text-xs",
  lg: "h-12 px-6 text-base",
  icon: "h-10 w-10 p-0",
};

export const Button = React.forwardRef<HTMLButtonElement, Props>(
  ({ className = "", variant = "default", size = "default", ...props }, ref) => (
    <button
      ref={ref}
      className={`inline-flex items-center justify-center rounded-md font-medium transition-colors focus:outline-none focus:ring-2 focus:ring-blue-500 disabled:opacity-50 disabled:pointer-events-none ${variantClass[variant]} ${sizeClass[size]} ${className}`}
      {...props}
    />
  )
);
Button.displayName = "Button";

export default Button;
"#;

const INPUT_TSX: &str = r#"import * as React from "react";

export const Input = React.forwardRef<HTMLInputElement, React.InputHTMLAttributes<HTMLInputElement>>(
  ({ className = "", type = "text", ...props }, ref) => (
    <input
      ref={ref}
      type={type}
      className={`flex h-10 w-full rounded-md border border-gray-300 bg-white px-3 py-2 text-sm placeholder:text-gray-400 focus:outline-none focus:ring-2 focus:ring-blue-500 disabled:opacity-50 ${className}`}
      {...props}
    />
  )
);
Input.displayName = "Input";

export default Input;
"#;

const LABEL_TSX: &str = r#"import * as React from "react";

export const Label = React.forwardRef<HTMLLabelElement, React.LabelHTMLAttributes<HTMLLabelElement>>(
  ({ className = "", ...props }, ref) => (
    <label
      ref={ref}
      className={`text-sm font-medium leading-none text-gray-900 ${className}`}
      {...props}
    />
  )
);
Label.displayName = "Label";

export default Label;
"#;

const CARD_TSX: &str = r#"import * as React from "react";

type DivProps = React.HTMLAttributes<HTMLDivElement>;

export const Card = React.forwardRef<HTMLDivElement, DivProps>(({ className = "", ...p }, ref) => (
  <div ref={ref} className={`rounded-lg border border-gray-200 bg-white shadow-sm ${className}`} {...p} />
));
Card.displayName = "Card";

export const CardHeader = React.forwardRef<HTMLDivElement, DivProps>(({ className = "", ...p }, ref) => (
  <div ref={ref} className={`flex flex-col space-y-1.5 p-4 ${className}`} {...p} />
));
CardHeader.displayName = "CardHeader";

export const CardTitle = React.forwardRef<HTMLHeadingElement, React.HTMLAttributes<HTMLHeadingElement>>(({ className = "", ...p }, ref) => (
  <h3 ref={ref} className={`text-lg font-semibold leading-none ${className}`} {...p} />
));
CardTitle.displayName = "CardTitle";

export const CardDescription = React.forwardRef<HTMLParagraphElement, React.HTMLAttributes<HTMLParagraphElement>>(({ className = "", ...p }, ref) => (
  <p ref={ref} className={`text-sm text-gray-600 ${className}`} {...p} />
));
CardDescription.displayName = "CardDescription";

export const CardContent = React.forwardRef<HTMLDivElement, DivProps>(({ className = "", ...p }, ref) => (
  <div ref={ref} className={`p-4 pt-0 ${className}`} {...p} />
));
CardContent.displayName = "CardContent";

export const CardFooter = React.forwardRef<HTMLDivElement, DivProps>(({ className = "", ...p }, ref) => (
  <div ref={ref} className={`flex items-center p-4 pt-0 ${className}`} {...p} />
));
CardFooter.displayName = "CardFooter";

export default Card;
"#;

const ALERT_TSX: &str = r#"import * as React from "react";

type Variant = "default" | "destructive" | "warning" | "success";

interface Props extends React.HTMLAttributes<HTMLDivElement> {
  variant?: Variant;
}

const variantClass: Record<Variant, string> = {
  default: "bg-gray-50 border-gray-300 text-gray-900",
  destructive: "bg-red-50 border-red-300 text-red-900",
  warning: "bg-yellow-50 border-yellow-300 text-yellow-900",
  success: "bg-green-50 border-green-300 text-green-900",
};

export const Alert = React.forwardRef<HTMLDivElement, Props>(
  ({ className = "", variant = "default", ...props }, ref) => (
    <div
      ref={ref}
      role="alert"
      className={`relative w-full rounded-lg border p-4 ${variantClass[variant]} ${className}`}
      {...props}
    />
  )
);
Alert.displayName = "Alert";

export const AlertTitle = React.forwardRef<HTMLHeadingElement, React.HTMLAttributes<HTMLHeadingElement>>(
  ({ className = "", ...p }, ref) => (
    <h5 ref={ref} className={`mb-1 font-medium leading-none tracking-tight ${className}`} {...p} />
  )
);
AlertTitle.displayName = "AlertTitle";

export const AlertDescription = React.forwardRef<HTMLDivElement, React.HTMLAttributes<HTMLDivElement>>(
  ({ className = "", ...p }, ref) => (
    <div ref={ref} className={`text-sm ${className}`} {...p} />
  )
);
AlertDescription.displayName = "AlertDescription";

export default Alert;
"#;

const TABS_TSX: &str = r#"import * as React from "react";

interface TabsCtxValue {
  value: string;
  onChange: (next: string) => void;
}
const TabsCtx = React.createContext<TabsCtxValue>({ value: "", onChange: () => {} });

interface TabsProps {
  defaultValue?: string;
  value?: string;
  onValueChange?: (v: string) => void;
  className?: string;
  children: React.ReactNode;
}
export const Tabs: React.FC<TabsProps> = ({ defaultValue = "", value, onValueChange, className = "", children }) => {
  const [internal, setInternal] = React.useState(value ?? defaultValue);
  const current = value ?? internal;
  const onChange = (next: string) => {
    if (value === undefined) setInternal(next);
    onValueChange?.(next);
  };
  return (
    <TabsCtx.Provider value={{ value: current, onChange }}>
      <div className={className}>{children}</div>
    </TabsCtx.Provider>
  );
};

export const TabsList: React.FC<{ className?: string; children: React.ReactNode }> = ({ className = "", children }) => (
  <div className={`inline-flex items-center justify-center gap-1 rounded-md bg-gray-100 p-1 ${className}`}>{children}</div>
);

interface TriggerProps { value: string; className?: string; children: React.ReactNode }
export const TabsTrigger: React.FC<TriggerProps> = ({ value, className = "", children }) => {
  const { value: current, onChange } = React.useContext(TabsCtx);
  const active = current === value;
  return (
    <button
      type="button"
      onClick={() => onChange(value)}
      className={`inline-flex items-center justify-center whitespace-nowrap rounded-sm px-3 py-1.5 text-sm font-medium transition-all ${active ? "bg-white text-gray-900 shadow" : "text-gray-600 hover:text-gray-900"} ${className}`}
    >
      {children}
    </button>
  );
};

export const TabsContent: React.FC<{ value: string; className?: string; children: React.ReactNode }> = ({ value, className = "", children }) => {
  const { value: current } = React.useContext(TabsCtx);
  if (current !== value) return null;
  return <div className={`mt-2 ${className}`}>{children}</div>;
};
"#;

const TABLE_TSX: &str = r#"import * as React from "react";

export const Table = React.forwardRef<HTMLTableElement, React.HTMLAttributes<HTMLTableElement>>(({ className = "", ...p }, ref) => (
  <div className="relative w-full overflow-auto">
    <table ref={ref} className={`w-full caption-bottom text-sm ${className}`} {...p} />
  </div>
));
Table.displayName = "Table";

export const TableHeader = React.forwardRef<HTMLTableSectionElement, React.HTMLAttributes<HTMLTableSectionElement>>(({ className = "", ...p }, ref) => (
  <thead ref={ref} className={`[&_tr]:border-b ${className}`} {...p} />
));
TableHeader.displayName = "TableHeader";

export const TableBody = React.forwardRef<HTMLTableSectionElement, React.HTMLAttributes<HTMLTableSectionElement>>(({ className = "", ...p }, ref) => (
  <tbody ref={ref} className={`[&_tr:last-child]:border-0 ${className}`} {...p} />
));
TableBody.displayName = "TableBody";

export const TableRow = React.forwardRef<HTMLTableRowElement, React.HTMLAttributes<HTMLTableRowElement>>(({ className = "", ...p }, ref) => (
  <tr ref={ref} className={`border-b transition-colors hover:bg-gray-50 ${className}`} {...p} />
));
TableRow.displayName = "TableRow";

export const TableHead = React.forwardRef<HTMLTableCellElement, React.ThHTMLAttributes<HTMLTableCellElement>>(({ className = "", ...p }, ref) => (
  <th ref={ref} className={`h-10 px-2 text-left align-middle font-medium text-gray-600 ${className}`} {...p} />
));
TableHead.displayName = "TableHead";

export const TableCell = React.forwardRef<HTMLTableCellElement, React.TdHTMLAttributes<HTMLTableCellElement>>(({ className = "", ...p }, ref) => (
  <td ref={ref} className={`p-2 align-middle ${className}`} {...p} />
));
TableCell.displayName = "TableCell";

export const TableCaption = React.forwardRef<HTMLTableCaptionElement, React.HTMLAttributes<HTMLTableCaptionElement>>(({ className = "", ...p }, ref) => (
  <caption ref={ref} className={`mt-4 text-sm text-gray-500 ${className}`} {...p} />
));
TableCaption.displayName = "TableCaption";
"#;

const BADGE_TSX: &str = r#"import * as React from "react";

type Variant = "default" | "secondary" | "outline" | "destructive";

interface Props extends React.HTMLAttributes<HTMLSpanElement> {
  variant?: Variant;
}

const variantClass: Record<Variant, string> = {
  default: "bg-blue-600 text-white",
  secondary: "bg-gray-200 text-gray-900",
  outline: "border border-gray-300 text-gray-900",
  destructive: "bg-red-600 text-white",
};

export const Badge: React.FC<Props> = ({ className = "", variant = "default", ...p }) => (
  <span
    className={`inline-flex items-center rounded-full px-2.5 py-0.5 text-xs font-semibold ${variantClass[variant]} ${className}`}
    {...p}
  />
);

export default Badge;
"#;

const SEPARATOR_TSX: &str = r#"import * as React from "react";

interface Props extends React.HTMLAttributes<HTMLDivElement> {
  orientation?: "horizontal" | "vertical";
}

export const Separator: React.FC<Props> = ({ className = "", orientation = "horizontal", ...p }) => (
  <div
    role="separator"
    className={`bg-gray-200 ${orientation === "horizontal" ? "h-px w-full" : "h-full w-px"} ${className}`}
    {...p}
  />
);

export default Separator;
"#;

const SONNER_TSX: &str = r#"import * as React from "react";

interface ToastInfo { id: number; text: string; variant?: string; }
let listeners: Array<(t: ToastInfo) => void> = [];
let counter = 0;

export const toast = (text: string, opts?: { variant?: string }) => {
  const id = ++counter;
  const t: ToastInfo = { id, text, variant: opts?.variant };
  listeners.forEach((l) => l(t));
  return id;
};
toast.success = (text: string) => toast(text, { variant: "success" });
toast.error = (text: string) => toast(text, { variant: "error" });
toast.info = (text: string) => toast(text, { variant: "info" });

interface ToasterProps {
  position?: "top-center" | "top-right" | "bottom-right" | "bottom-center";
}

export const Toaster: React.FC<ToasterProps> = ({ position = "top-center" }) => {
  const [items, setItems] = React.useState<ToastInfo[]>([]);
  React.useEffect(() => {
    const onPush = (t: ToastInfo) => {
      setItems((cur) => [...cur, t]);
      setTimeout(() => setItems((cur) => cur.filter((x) => x.id !== t.id)), 3500);
    };
    listeners.push(onPush);
    return () => { listeners = listeners.filter((l) => l !== onPush); };
  }, []);
  const posClass: Record<string, string> = {
    "top-center": "top-4 left-1/2 -translate-x-1/2",
    "top-right": "top-4 right-4",
    "bottom-right": "bottom-4 right-4",
    "bottom-center": "bottom-4 left-1/2 -translate-x-1/2",
  };
  const variantClass = (v?: string) => {
    if (v === "success") return "bg-green-600 text-white";
    if (v === "error") return "bg-red-600 text-white";
    return "bg-gray-900 text-white";
  };
  return (
    <div className={`fixed z-50 flex flex-col gap-2 ${posClass[position] || posClass["top-center"]}`}>
      {items.map((t) => (
        <div key={t.id} className={`rounded-md px-4 py-2 text-sm shadow-lg ${variantClass(t.variant)}`}>
          {t.text}
        </div>
      ))}
    </div>
  );
};

export default Toaster;
"#;

const DIALOG_TSX: &str = r#"import * as React from "react";

interface DialogCtxValue { open: boolean; setOpen: (b: boolean) => void; }
const DialogCtx = React.createContext<DialogCtxValue>({ open: false, setOpen: () => {} });

interface DialogProps {
  open?: boolean;
  defaultOpen?: boolean;
  onOpenChange?: (b: boolean) => void;
  children: React.ReactNode;
}
export const Dialog: React.FC<DialogProps> = ({ open, defaultOpen = false, onOpenChange, children }) => {
  const [internal, setInternal] = React.useState(defaultOpen);
  const cur = open ?? internal;
  const setOpen = (b: boolean) => {
    if (open === undefined) setInternal(b);
    onOpenChange?.(b);
  };
  return <DialogCtx.Provider value={{ open: cur, setOpen }}>{children}</DialogCtx.Provider>;
};

export const DialogTrigger: React.FC<{ children: React.ReactElement; asChild?: boolean }> = ({ children }) => {
  const { setOpen } = React.useContext(DialogCtx);
  return React.cloneElement(children, { onClick: () => setOpen(true) });
};

export const DialogContent: React.FC<{ className?: string; children: React.ReactNode }> = ({ className = "", children }) => {
  const { open, setOpen } = React.useContext(DialogCtx);
  if (!open) return null;
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50" onClick={() => setOpen(false)}>
      <div className={`relative max-w-lg rounded-lg bg-white p-6 shadow-xl ${className}`} onClick={(e) => e.stopPropagation()}>
        {children}
      </div>
    </div>
  );
};

export const DialogHeader: React.FC<{ children: React.ReactNode }> = ({ children }) => <div className="mb-4">{children}</div>;
export const DialogFooter: React.FC<{ children: React.ReactNode }> = ({ children }) => <div className="mt-4 flex justify-end gap-2">{children}</div>;
export const DialogTitle: React.FC<{ children: React.ReactNode }> = ({ children }) => <h2 className="text-lg font-semibold">{children}</h2>;
export const DialogDescription: React.FC<{ children: React.ReactNode }> = ({ children }) => <p className="text-sm text-gray-600">{children}</p>;
"#;

const DROPDOWN_TSX: &str = r#"import * as React from "react";

interface DDCtxValue { open: boolean; setOpen: (b: boolean) => void; }
const DDCtx = React.createContext<DDCtxValue>({ open: false, setOpen: () => {} });

export const DropdownMenu: React.FC<{ children: React.ReactNode }> = ({ children }) => {
  const [open, setOpen] = React.useState(false);
  return (
    <DDCtx.Provider value={{ open, setOpen }}>
      <div className="relative inline-block">{children}</div>
    </DDCtx.Provider>
  );
};

export const DropdownMenuTrigger: React.FC<{ children: React.ReactElement; asChild?: boolean }> = ({ children }) => {
  const { open, setOpen } = React.useContext(DDCtx);
  return React.cloneElement(children, { onClick: () => setOpen(!open) });
};

export const DropdownMenuContent: React.FC<{ className?: string; children: React.ReactNode; align?: "start"|"end" }> = ({ className = "", children, align = "start" }) => {
  const { open, setOpen } = React.useContext(DDCtx);
  if (!open) return null;
  const alignClass = align === "end" ? "right-0" : "left-0";
  return (
    <>
      <div className="fixed inset-0 z-40" onClick={() => setOpen(false)} />
      <div className={`absolute z-50 mt-1 min-w-[160px] rounded-md border border-gray-200 bg-white p-1 shadow-md ${alignClass} ${className}`}>{children}</div>
    </>
  );
};

export const DropdownMenuItem: React.FC<{ onClick?: () => void; className?: string; children: React.ReactNode }> = ({ onClick, className = "", children }) => {
  const { setOpen } = React.useContext(DDCtx);
  return (
    <button
      type="button"
      className={`flex w-full items-center rounded-sm px-2 py-1.5 text-sm hover:bg-gray-100 ${className}`}
      onClick={() => { onClick?.(); setOpen(false); }}
    >
      {children}
    </button>
  );
};

export const DropdownMenuLabel: React.FC<{ children: React.ReactNode }> = ({ children }) => (
  <div className="px-2 py-1.5 text-xs font-semibold text-gray-500">{children}</div>
);

export const DropdownMenuSeparator: React.FC = () => <div className="my-1 h-px bg-gray-200" />;
"#;

const SELECT_TSX: &str = r#"import * as React from "react";

interface Props extends React.SelectHTMLAttributes<HTMLSelectElement> {
  onValueChange?: (v: string) => void;
}

export const Select = React.forwardRef<HTMLSelectElement, Props>(({ className = "", onValueChange, onChange, ...p }, ref) => (
  <select
    ref={ref}
    className={`h-10 w-full rounded-md border border-gray-300 bg-white px-3 py-2 text-sm ${className}`}
    onChange={(e) => { onChange?.(e); onValueChange?.(e.target.value); }}
    {...p}
  />
));
Select.displayName = "Select";

export const SelectItem: React.FC<{ value: string; children: React.ReactNode }> = ({ value, children }) => (
  <option value={value}>{children}</option>
);

// Shim per API shadcn (Trigger/Content/Value): non implementati, ma esportati come no-op
export const SelectTrigger: React.FC<{ className?: string; children?: React.ReactNode }> = ({ children }) => <>{children}</>;
export const SelectContent: React.FC<{ children?: React.ReactNode }> = ({ children }) => <>{children}</>;
export const SelectValue: React.FC<{ placeholder?: string }> = ({ placeholder }) => <option value="">{placeholder ?? ""}</option>;

export default Select;
"#;

const POPOVER_TSX: &str = r#"import * as React from "react";

interface PCtxValue { open: boolean; setOpen: (b: boolean) => void; }
const PCtx = React.createContext<PCtxValue>({ open: false, setOpen: () => {} });

export const Popover: React.FC<{ children: React.ReactNode }> = ({ children }) => {
  const [open, setOpen] = React.useState(false);
  return (
    <PCtx.Provider value={{ open, setOpen }}>
      <div className="relative inline-block">{children}</div>
    </PCtx.Provider>
  );
};

export const PopoverTrigger: React.FC<{ children: React.ReactElement; asChild?: boolean }> = ({ children }) => {
  const { open, setOpen } = React.useContext(PCtx);
  return React.cloneElement(children, { onClick: () => setOpen(!open) });
};

export const PopoverContent: React.FC<{ className?: string; children: React.ReactNode; align?: "start"|"end" }> = ({ className = "", children, align = "start" }) => {
  const { open, setOpen } = React.useContext(PCtx);
  if (!open) return null;
  const alignClass = align === "end" ? "right-0" : "left-0";
  return (
    <>
      <div className="fixed inset-0 z-40" onClick={() => setOpen(false)} />
      <div className={`absolute z-50 mt-1 w-72 rounded-md border border-gray-200 bg-white p-4 shadow-md ${alignClass} ${className}`}>{children}</div>
    </>
  );
};
"#;

const TEXTAREA_TSX: &str = r#"import * as React from "react";

export const Textarea = React.forwardRef<HTMLTextAreaElement, React.TextareaHTMLAttributes<HTMLTextAreaElement>>(
  ({ className = "", ...props }, ref) => (
    <textarea
      ref={ref}
      className={`flex min-h-[80px] w-full rounded-md border border-gray-300 bg-white px-3 py-2 text-sm placeholder:text-gray-400 focus:outline-none focus:ring-2 focus:ring-blue-500 disabled:opacity-50 ${className}`}
      {...props}
    />
  )
);
Textarea.displayName = "Textarea";

export default Textarea;
"#;
