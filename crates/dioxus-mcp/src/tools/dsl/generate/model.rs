use std::collections::BTreeSet;
use std::path::Path;

use heck::{ToPascalCase, ToSnakeCase};
use minijinja::context;

use crate::tools::scaffold::ScaffoldResult;

use super::super::render::*;
use super::super::templates::*;
use super::super::types::*;

/// All sibling-type lookups the model generator can resolve into a
/// `use crate::*` import. Models live under `src/model/{snake}.rs`,
/// view-state enums under `src/state/{snake}.rs`.
#[derive(Debug, Default)]
pub(crate) struct ModelImportCtx {
    /// PascalCase model names → snake-case file stem. Excludes the model
    /// currently being generated (the caller drops it before passing).
    pub models: BTreeSet<String>,
    /// PascalCase enum names declared via `ViewState { enum_variants: [...] }`
    /// → snake-case file stem.
    pub view_state_enums: BTreeSet<(String, String)>,
}

pub(crate) fn generate_model(
    crate_root: &Path,
    m: &DslModel,
    imports: &ModelImportCtx,
) -> Result<ScaffoldResult, String> {
    let pascal = m.name.to_pascal_case();
    let snake = m.name.to_snake_case();

    let defaults = ["Debug", "Clone", "PartialEq", "Serialize", "Deserialize"];
    let mut derives: Vec<String> = defaults.iter().map(|s| (*s).to_string()).collect();
    for extra in &m.derives {
        let t = extra.trim();
        if !t.is_empty() && !derives.iter().any(|d| d == t) {
            derives.push(t.to_string());
        }
    }
    let derives_str = derives.join(", ");

    let fields_ctx: Vec<_> = m
        .fields
        .iter()
        .map(|f| {
            context! {
                name => f.name.to_snake_case(),
                ty => f.ty.clone(),
                optional => f.optional,
            }
        })
        .collect();

    // Gather every bare identifier appearing inside the field types. Anything
    // matching another declared Model / ViewState-enum gets a `use crate::...`
    // import so authors can write `column: Column` without remembering the
    // fully-qualified path.
    let mut referenced: BTreeSet<String> = BTreeSet::new();
    for f in &m.fields {
        // Wrap with Option<...> if optional so the parser sees the same shape
        // the template emits; ignore parse errors (the compiler will surface
        // them downstream — we don't want to swallow type typos here).
        let to_parse = if f.optional {
            format!("Option<{}>", f.ty)
        } else {
            f.ty.clone()
        };
        if let Ok(ty) = syn::parse_str::<syn::Type>(&to_parse) {
            collect_type_idents(&ty, &mut referenced);
        }
    }
    // Drop the current model name and anything that's already a path-qualified
    // reference (those land naturally via `referenced` only as the last segment,
    // but the field-type string already names the path, so re-importing is
    // harmless yet noisy — only emit when the field-type uses the bare ident).
    referenced.remove(&pascal);

    let mut cross_imports: Vec<String> = Vec::new();
    for ident in &referenced {
        if let Some(target_snake) = imports.models.iter().find(|s| {
            // imports.models stores snake-case stems; compare via PascalCase.
            s.to_pascal_case() == *ident
        }) {
            let line = format!("use crate::model::{target_snake}::{ident};");
            if !cross_imports.iter().any(|l| l == &line) {
                cross_imports.push(line);
            }
        } else if let Some((enum_snake, _)) = imports
            .view_state_enums
            .iter()
            .find(|(_, pascal)| pascal == ident)
        {
            let line = format!("use crate::state::{enum_snake}::{ident};");
            if !cross_imports.iter().any(|l| l == &line) {
                cross_imports.push(line);
            }
        }
    }
    cross_imports.sort();

    let body = render(
        "model",
        MODEL_TPL,
        context! {
            pascal => pascal,
            derives => derives_str,
            fields => fields_ctx,
            cross_imports => cross_imports,
        },
    )?;
    write_module_file(crate_root, "src/model", &snake, body)
}

/// Walk a parsed [`syn::Type`] and collect every bare identifier appearing in
/// type positions (including generic arguments). Used by the cross-model auto-
/// import pass to discover which sibling models / enums need a `use` line.
fn collect_type_idents(ty: &syn::Type, out: &mut BTreeSet<String>) {
    match ty {
        syn::Type::Path(tp) => {
            // For a single-segment path (`Column`, `Vec<...>`) the last segment
            // *is* the type. For a multi-segment path
            // (`crate::model::column::Column`) the field type already names
            // the path explicitly — drop the leading segments because emitting
            // a duplicate `use` would shadow the path-qualified one.
            if tp.path.segments.len() == 1
                && let Some(last) = tp.path.segments.last()
            {
                out.insert(last.ident.to_string());
            }
            for seg in &tp.path.segments {
                if let syn::PathArguments::AngleBracketed(a) = &seg.arguments {
                    for arg in &a.args {
                        if let syn::GenericArgument::Type(inner) = arg {
                            collect_type_idents(inner, out);
                        }
                    }
                }
            }
        }
        syn::Type::Reference(r) => collect_type_idents(&r.elem, out),
        syn::Type::Tuple(t) => {
            for elem in &t.elems {
                collect_type_idents(elem, out);
            }
        }
        syn::Type::Slice(s) => collect_type_idents(&s.elem, out),
        syn::Type::Array(a) => collect_type_idents(&a.elem, out),
        syn::Type::Paren(p) => collect_type_idents(&p.elem, out),
        syn::Type::Group(g) => collect_type_idents(&g.elem, out),
        _ => {}
    }
}
