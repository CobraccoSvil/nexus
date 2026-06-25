//! `#[derive(GraphState)]` — genera il reducer di canale dello stato del grafo.
//!
//! PUNTO UNICO (regola L) della semantica di merge di LangGraph. Per ogni campo
//! dello struct annotato:
//!
//! - `#[reduce(append)]` -> canale "add": il delta porta un `Option<Vec<T>>`; se
//!   `Some(v)` il reducer fa `self.campo.extend(v)` (accumulo cross-nodo). Solo
//!   `messages` e `meta_steps` lo usano (vedi `state.py:17,24`).
//! - tutti gli altri campi -> canale di default "overwrite-by-last-write": il
//!   delta porta un `Option<T>`; se `Some(v)` il reducer fa `self.campo = v`. La
//!   distinzione `None` (non toccare, no-op) vs `Some(vuoto)` (azzera) e'
//!   LOAD-BEARING e viene preservata bit-per-bit (es. `discovered_tools_next_turn`).
//!
//! Il derive genera un metodo INERENTE `pub fn merge_typed(&mut self, delta: D)`
//! dove `D` e' lo struct dei delta. L'impl del trait `nexus_graph::GraphState`
//! (che lavora su un delta JSON opaco lato runtime) delega a questo metodo, cosi'
//! la semantica vive in un solo posto. Il nome del tipo del delta e' configurabile
//! via `#[graph_state(delta = "StateDelta")]` (default: `StateDelta`).

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Fields, Ident, LitStr};

/// Deriva il reducer (`merge_typed`) per uno struct di stato del grafo.
#[proc_macro_derive(GraphState, attributes(reduce, graph_state))]
pub fn derive_graph_state(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(input) {
        Ok(ts) => ts.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Espansione vera e propria, separata per propagare gli errori con `?`.
fn expand(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let struct_ident = &input.ident;

    // Solo struct con campi nominati: lo stato del grafo e' uno struct piatto.
    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return Err(syn::Error::new_spanned(
                    struct_ident,
                    "GraphState richiede uno struct con campi nominati",
                ))
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                struct_ident,
                "GraphState e' derivabile solo su struct",
            ))
        }
    };

    // Tipo del delta: default `StateDelta`, override via #[graph_state(delta = "...")].
    let delta_ident = resolve_delta_ident(&input)?;

    // Genera una riga di merge per campo, distinguendo append (extend) da
    // overwrite (assegnazione se Some).
    let mut merge_stmts = Vec::new();
    for field in fields {
        let field_ident = field
            .ident
            .as_ref()
            .ok_or_else(|| syn::Error::new_spanned(field, "campo senza identificatore"))?;

        if is_append(field)? {
            // Canale "add": delta.<campo> e' Option<Vec<T>>. Some -> extend.
            merge_stmts.push(quote! {
                if let Some(__items) = delta.#field_ident {
                    self.#field_ident.extend(__items);
                }
            });
        } else {
            // Canale "overwrite": delta.<campo> e' Option<T>. Some -> assegna
            // (anche un valore vuoto azzera; None = no-op, non tocca).
            merge_stmts.push(quote! {
                if let Some(__value) = delta.#field_ident {
                    self.#field_ident = __value;
                }
            });
        }
    }

    let expanded = quote! {
        impl #struct_ident {
            /// Reducer di canale tipizzato (PUNTO UNICO, regola L). Applica il
            /// delta secondo la semantica LangGraph: append per i canali `add`,
            /// overwrite-se-`Some` per tutti gli altri. `None` su un campo del
            /// delta e' un no-op (il campo non viene toccato).
            pub fn merge_typed(&mut self, delta: #delta_ident) {
                #(#merge_stmts)*
            }
        }
    };

    Ok(expanded)
}

/// Risolve il nome del tipo del delta. Default `StateDelta`; configurabile con
/// `#[graph_state(delta = "NomeStruct")]` a livello di struct.
fn resolve_delta_ident(input: &DeriveInput) -> syn::Result<Ident> {
    let mut delta_name: Option<Ident> = None;
    for attr in &input.attrs {
        if !attr.path().is_ident("graph_state") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("delta") {
                let value = meta.value()?;
                let lit: LitStr = value.parse()?;
                delta_name = Some(Ident::new(&lit.value(), lit.span()));
                Ok(())
            } else {
                Err(meta.error("attributo graph_state sconosciuto (atteso `delta`)"))
            }
        })?;
    }
    Ok(delta_name.unwrap_or_else(|| Ident::new("StateDelta", proc_macro2::Span::call_site())))
}

/// `true` se il campo ha `#[reduce(append)]`. Riconosce solo `append`: ogni
/// altra modalita' e' un errore di compilazione (evita typo silenziosi).
fn is_append(field: &syn::Field) -> syn::Result<bool> {
    let mut append = false;
    for attr in &field.attrs {
        if !attr.path().is_ident("reduce") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("append") {
                append = true;
                Ok(())
            } else {
                Err(meta.error("modalita' reduce sconosciuta (atteso `append`)"))
            }
        })?;
    }
    Ok(append)
}
