use std::collections::BTreeSet;
use std::path::Path;

use heck::{ToPascalCase, ToSnakeCase};

use crate::tools::scaffold::ScaffoldResult;

use super::types::*;
use super::util::leaf_for;

pub(super) fn apply_modify(
    crate_root: &Path,
    m: &DslModify,
    if_missing: bool,
    result: &mut ScaffoldResult,
) -> Result<(), String> {
    match m {
        DslModify::AddModelField { model, fields } => {
            let path = leaf_for(crate_root, "src/model", model);
            let struct_name = model.to_pascal_case();
            modify_struct_fields(&path, &struct_name, fields, if_missing, result, "model")
        }
        DslModify::AddComponentProp { component, props } => {
            let path = leaf_for(crate_root, "src/components", component);
            let props_name = format!("{}Props", component.to_pascal_case());
            modify_props_struct(&path, &props_name, props, if_missing, result)
        }
        DslModify::AddServerFnArg { server_fn, args } => {
            let path = leaf_for(crate_root, "src/server", server_fn);
            let snake = server_fn.to_snake_case();
            modify_fn_args(&path, &snake, args, if_missing, result)
        }
        DslModify::RemoveModelField { model, fields } => {
            let path = leaf_for(crate_root, "src/model", model);
            let struct_name = model.to_pascal_case();
            remove_struct_fields(&path, &struct_name, fields, if_missing, result, "model")
        }
        DslModify::RemoveComponentProp { component, props } => {
            let path = leaf_for(crate_root, "src/components", component);
            let props_name = format!("{}Props", component.to_pascal_case());
            remove_struct_fields(&path, &props_name, props, if_missing, result, "component")
        }
    }
}

pub(super) fn missing_target(
    path: &Path,
    kind: &str,
    if_missing: bool,
    result: &mut ScaffoldResult,
) -> Result<bool, String> {
    if path.exists() {
        return Ok(false);
    }
    if if_missing {
        result.collisions.push(path.to_path_buf());
        Ok(true)
    } else {
        Err(format!(
            "modify: target {} for {kind} does not exist on disk; create it first or pass `if_missing: true` to skip",
            path.display()
        ))
    }
}

pub(super) fn modify_struct_fields(
    path: &Path,
    struct_name: &str,
    fields: &[DslModelField],
    if_missing: bool,
    result: &mut ScaffoldResult,
    kind_label: &str,
) -> Result<(), String> {
    if missing_target(path, kind_label, if_missing, result)? {
        return Ok(());
    }
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let parsed =
        syn::parse_file(&src).map_err(|e| format!("modify: parse {}: {e}", path.display()))?;
    let target = parsed
        .items
        .iter()
        .find_map(|it| match it {
            syn::Item::Struct(s) if s.ident == struct_name => Some(s),
            _ => None,
        })
        .ok_or_else(|| format!("modify: no struct {struct_name} in {}", path.display()))?;
    let existing: BTreeSet<String> = target
        .fields
        .iter()
        .filter_map(|f| f.ident.as_ref().map(|i| i.to_string()))
        .collect();
    let new_fields: Vec<&DslModelField> = fields
        .iter()
        .filter(|f| !existing.contains(&f.name.to_snake_case()))
        .collect();
    if new_fields.is_empty() {
        return Ok(());
    }
    let insert_at = find_close_delim(&src, &format!("struct {struct_name}"), '{', '}')?;
    let mut insertion = String::new();
    for f in &new_fields {
        let n = f.name.to_snake_case();
        if f.optional {
            insertion.push_str(&format!("    pub {n}: Option<{}>,\n", f.ty));
        } else {
            insertion.push_str(&format!("    pub {n}: {},\n", f.ty));
        }
    }
    let new_src = splice(&src, insert_at, &insertion);
    std::fs::write(path, new_src).map_err(|e| e.to_string())?;
    if !result.files_modified.iter().any(|p| p == path) {
        result.files_modified.push(path.to_path_buf());
    }
    Ok(())
}

pub(super) fn modify_props_struct(
    path: &Path,
    struct_name: &str,
    props: &[DslPropDef],
    if_missing: bool,
    result: &mut ScaffoldResult,
) -> Result<(), String> {
    if missing_target(path, "component", if_missing, result)? {
        return Ok(());
    }
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let parsed =
        syn::parse_file(&src).map_err(|e| format!("modify: parse {}: {e}", path.display()))?;
    let target = parsed.items.iter().find_map(|it| match it {
        syn::Item::Struct(s) if s.ident == struct_name => Some(s),
        _ => None,
    });
    let Some(target) = target else {
        return Err(format!(
            "modify: no struct {struct_name} in {} — convert the component to take props first (re-create it with `props:` declared) before adding more",
            path.display()
        ));
    };
    let existing: BTreeSet<String> = target
        .fields
        .iter()
        .filter_map(|f| f.ident.as_ref().map(|i| i.to_string()))
        .collect();
    let new_props: Vec<&DslPropDef> = props
        .iter()
        .filter(|p| !existing.contains(&p.name.to_snake_case()))
        .collect();
    if new_props.is_empty() {
        return Ok(());
    }
    let insert_at = find_close_delim(&src, &format!("struct {struct_name}"), '{', '}')?;
    let mut insertion = String::new();
    for p in &new_props {
        let n = p.name.to_snake_case();
        if p.optional {
            insertion.push_str(&format!(
                "    #[props(default)]\n    pub {n}: Option<{}>,\n",
                p.ty
            ));
        } else {
            insertion.push_str(&format!("    pub {n}: {},\n", p.ty));
        }
    }
    let new_src = splice(&src, insert_at, &insertion);
    std::fs::write(path, new_src).map_err(|e| e.to_string())?;
    if !result.files_modified.iter().any(|p| p == path) {
        result.files_modified.push(path.to_path_buf());
    }
    Ok(())
}

pub(super) fn modify_fn_args(
    path: &Path,
    snake_name: &str,
    args: &[DslArgDef],
    if_missing: bool,
    result: &mut ScaffoldResult,
) -> Result<(), String> {
    if missing_target(path, "server_fn", if_missing, result)? {
        return Ok(());
    }
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let parsed =
        syn::parse_file(&src).map_err(|e| format!("modify: parse {}: {e}", path.display()))?;
    let target_fn = parsed
        .items
        .iter()
        .find_map(|it| match it {
            syn::Item::Fn(f) if f.sig.ident == snake_name => Some(f),
            _ => None,
        })
        .ok_or_else(|| format!("modify: no fn {snake_name} in {}", path.display()))?;
    let existing: BTreeSet<String> = target_fn
        .sig
        .inputs
        .iter()
        .filter_map(|arg| match arg {
            syn::FnArg::Typed(pt) => match pt.pat.as_ref() {
                syn::Pat::Ident(pi) => Some(pi.ident.to_string()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let new_args: Vec<&DslArgDef> = args
        .iter()
        .filter(|a| !existing.contains(&a.name.to_snake_case()))
        .collect();
    if new_args.is_empty() {
        return Ok(());
    }
    let insert_at = find_close_delim(&src, &format!("fn {snake_name}"), '(', ')')?;
    // Preserve the parameter list's trailing-comma convention. If the existing
    // last non-whitespace before the closing `)` is `,`, we just append. If
    // it's `(` (no args), we still emit fields with leading newline + indent.
    // Either way the generated lines below carry their own trailing commas.
    let mut insertion = String::new();
    for a in &new_args {
        insertion.push_str(&format!("    {}: {},\n", a.name.to_snake_case(), a.ty));
    }
    let new_src = splice(&src, insert_at, &insertion);
    std::fs::write(path, new_src).map_err(|e| e.to_string())?;
    if !result.files_modified.iter().any(|p| p == path) {
        result.files_modified.push(path.to_path_buf());
    }
    Ok(())
}

/// Drop the named fields from `struct {struct_name} { ... }` in the file at
/// `path`. Idempotent: names that are already absent are silently skipped. The
/// match uses snake_case comparison so callers can pass any-case names.
///
/// Each removal is byte-level: we locate the field by syn-parsing, then walk
/// the source to find the trailing `,` (or `\n` for trailing-comma-less files)
/// and any preceding `#[...]` attribute lines so the whole field — attribute,
/// type, comma, leading whitespace — disappears together. Adjacent blank lines
/// are preserved.
pub(super) fn remove_struct_fields(
    path: &Path,
    struct_name: &str,
    names: &[String],
    if_missing: bool,
    result: &mut ScaffoldResult,
    kind_label: &str,
) -> Result<(), String> {
    if missing_target(path, kind_label, if_missing, result)? {
        return Ok(());
    }
    let mut src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let to_drop: BTreeSet<String> = names.iter().map(|n| n.to_snake_case()).collect();
    if to_drop.is_empty() {
        return Ok(());
    }

    use syn::spanned::Spanned;
    let mut any_removed = false;
    loop {
        let parsed =
            syn::parse_file(&src).map_err(|e| format!("modify: parse {}: {e}", path.display()))?;
        let target = parsed
            .items
            .iter()
            .find_map(|it| match it {
                syn::Item::Struct(s) if s.ident == struct_name => Some(s),
                _ => None,
            })
            .ok_or_else(|| format!("modify: no struct {struct_name} in {}", path.display()))?;
        // Find the next field present in `to_drop`. We re-parse after each
        // removal because byte spans shift; a single pass over the struct fields
        // is simpler than tracking offset deltas.
        let target_field = target.fields.iter().find(|f| {
            f.ident
                .as_ref()
                .map(|i| to_drop.contains(&i.to_string()))
                .unwrap_or(false)
        });
        let Some(field) = target_field else {
            break;
        };
        // Compute the byte range to cut. syn 2.x spans are reliable for
        // structured items in non-macro source.
        let span = Spanned::span(field);
        let start = span.byte_range().start;
        let end = span.byte_range().end;
        // Extend back over any attached `#[...]` attribute lines and leading
        // whitespace on the field's own line.
        let mut cut_start = start;
        for attr in &field.attrs {
            let s = Spanned::span(attr).byte_range().start;
            if s < cut_start {
                cut_start = s;
            }
        }
        // Walk left through indentation/newline so we delete the whole line.
        let bytes = src.as_bytes();
        while cut_start > 0 {
            let prev = bytes[cut_start - 1];
            if prev == b' ' || prev == b'\t' {
                cut_start -= 1;
            } else {
                break;
            }
        }
        if cut_start > 0 && bytes[cut_start - 1] == b'\n' {
            cut_start -= 1;
        }
        // Extend forward to include the trailing comma (if any).
        let mut cut_end = end;
        while cut_end < bytes.len() && (bytes[cut_end] == b' ' || bytes[cut_end] == b'\t') {
            cut_end += 1;
        }
        if cut_end < bytes.len() && bytes[cut_end] == b',' {
            cut_end += 1;
        }
        let mut new_src = String::with_capacity(src.len());
        new_src.push_str(&src[..cut_start]);
        new_src.push_str(&src[cut_end..]);
        src = new_src;
        any_removed = true;
    }

    if any_removed {
        std::fs::write(path, &src).map_err(|e| e.to_string())?;
        if !result.files_modified.iter().any(|p| p == path) {
            result.files_modified.push(path.to_path_buf());
        }
    }
    Ok(())
}

/// Locate the byte position of the matching close delimiter for the opening
/// `open` that appears after `anchor` in `src`. Naive depth count — adequate
/// for the generated files we operate on (no string/char literals containing
/// raw braces or parens). The caller has already syn-parsed the source.
pub(super) fn find_close_delim(
    src: &str,
    anchor: &str,
    open: char,
    close: char,
) -> Result<usize, String> {
    let start = src
        .find(anchor)
        .ok_or_else(|| format!("could not locate {anchor:?} in source"))?;
    let after_open = src[start..]
        .find(open)
        .map(|i| start + i + open.len_utf8())
        .ok_or_else(|| format!("malformed {anchor}: no {open:?}"))?;
    let mut depth: i32 = 1;
    for (i, ch) in src[after_open..].char_indices() {
        if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                return Ok(after_open + i);
            }
        }
    }
    Err(format!("malformed {anchor}: no {close:?}"))
}

pub(super) fn splice(src: &str, at: usize, insertion: &str) -> String {
    let mut out = String::with_capacity(src.len() + insertion.len());
    out.push_str(&src[..at]);
    out.push_str(insertion);
    out.push_str(&src[at..]);
    out
}
