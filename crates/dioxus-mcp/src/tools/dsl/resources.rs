use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use heck::{ToPascalCase, ToSnakeCase};
use minijinja::context;

use crate::state::State;
use crate::tools::scaffold::{ModUpsert, ScaffoldResult, upsert_mod_entry};

use super::render::render;
use super::templates::*;
use super::types::*;

#[derive(Debug, Clone)]
pub(super) struct SynthServerFn {
    pub(super) name: String,
    args: Vec<(String, String)>,
    return_type: String,
    method: &'static str,
    path: String,
    body: String,
}

/// For every `screens:` entry with `template.kind: client_crud`, find the
/// model the screen will construct (via the referenced client_store's
/// `item_type`) and ensure `Default` is in its `derives:` list. The generated
/// body uses `..Default::default()` on the rest of the struct, which silently
/// breaks compilation when the user-authored model only derives the usual
/// `Debug, Clone, Serialize, Deserialize, PartialEq` set.
///
/// Case-insensitive dedup so users who already typed `derives: [Default]`
/// don't end up with `derives: [Default, Default]`.
pub(super) fn ensure_default_on_client_crud_models(doc: &mut DslDoc) {
    if doc.screens.is_empty() {
        return;
    }
    // Collect (item_type) names from client_crud screens that resolve through
    // a known client_store. Iterate immutably first so we can mutate `models`
    // afterwards without aliasing.
    let mut needs_default: BTreeSet<String> = BTreeSet::new();
    for sc in &doc.screens {
        let Some(t) = sc.template.as_ref() else {
            continue;
        };
        if t.kind != "client_crud" {
            continue;
        }
        let item_type = t.item_type.clone().or_else(|| {
            t.store.as_ref().and_then(|store_ref| {
                let key = store_ref.to_snake_case();
                doc.client_stores
                    .iter()
                    .find(|cs| cs.name.to_snake_case() == key)
                    .map(|cs| cs.item_type.clone())
            })
        });
        if let Some(it) = item_type {
            needs_default.insert(it.to_snake_case());
        }
    }
    for m in &mut doc.models {
        if !needs_default.contains(&m.name.to_snake_case()) {
            continue;
        }
        let has_default = m.derives.iter().any(|d| d.eq_ignore_ascii_case("Default"));
        if !has_default {
            m.derives.push("Default".to_string());
        }
    }
}

/// Companion to [`ensure_default_on_client_crud_models`] for the on-disk case:
/// when a `client_crud` Screen references a model that is *not* declared in
/// the same doc but already exists at `src/model/{snake}.rs`, patch its
/// `#[derive(...)]` line to include `Default`. Returns the list of files
/// modified (empty when no patching was needed).
///
/// Idempotent. Never touches a file outside the conventional model path —
/// callers using a non-standard model layout still need to hand-edit, but
/// the next_steps surface a hint elsewhere in the response.
pub(super) fn patch_on_disk_models_for_client_crud_default(
    doc: &DslDoc,
    crate_root: &Path,
) -> Result<Vec<std::path::PathBuf>, String> {
    if doc.screens.is_empty() {
        return Ok(Vec::new());
    }
    // Same shape as ensure_default_on_client_crud_models: collect every model
    // type-name a client_crud screen will construct.
    let mut needed: BTreeSet<String> = BTreeSet::new();
    for sc in &doc.screens {
        let Some(t) = sc.template.as_ref() else {
            continue;
        };
        if t.kind != "client_crud" {
            continue;
        }
        let item_type = t.item_type.clone().or_else(|| {
            t.store.as_ref().and_then(|store_ref| {
                let key = store_ref.to_snake_case();
                doc.client_stores
                    .iter()
                    .find(|cs| cs.name.to_snake_case() == key)
                    .map(|cs| cs.item_type.clone())
            })
        });
        if let Some(it) = item_type {
            needed.insert(it);
        }
    }
    // Drop names that the doc itself declares — the in-doc pre-pass already
    // handles those, and re-deriving on a freshly-generated file would just
    // double-fire.
    let in_doc: BTreeSet<String> = doc.models.iter().map(|m| m.name.clone()).collect();
    needed.retain(|n| {
        !in_doc
            .iter()
            .any(|m| m.to_snake_case() == n.to_snake_case())
    });

    let mut modified: Vec<std::path::PathBuf> = Vec::new();
    for type_name in &needed {
        let snake = type_name.to_snake_case();
        let path = crate_root.join(format!("src/model/{snake}.rs"));
        if !path.exists() {
            // Not at the conventional location — leave it alone; the user
            // either keeps the model elsewhere or hasn't authored it yet.
            continue;
        }
        let src =
            std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        if let Some(new_src) = add_default_to_derive(&src, type_name) {
            std::fs::write(&path, &new_src)
                .map_err(|e| format!("write {}: {e}", path.display()))?;
            modified.push(path);
        }
    }
    Ok(modified)
}

/// Locate `#[derive(...)]` on `struct {type_name}` in `src` and append
/// `Default` to its derive list if it isn't already there. Returns the
/// modified source, or `None` when no change is needed (Default is already
/// derived, or the file doesn't carry a matching struct).
///
/// Uses textual splicing on the first `#[derive(...)]` line that precedes
/// the target struct definition. Robust enough for hand-authored model files
/// and the shape we emit ourselves; bails out (and reports no change) if the
/// struct sits without a derive attribute or the file fails to parse.
pub(super) fn add_default_to_derive(src: &str, type_name: &str) -> Option<String> {
    let file = syn::parse_file(src).ok()?;
    let target = type_name.to_pascal_case();
    let item = file.items.iter().find_map(|it| match it {
        syn::Item::Struct(s) if s.ident == target => Some(s),
        _ => None,
    })?;
    let derive = item.attrs.iter().find(|a| a.path().is_ident("derive"))?;
    let mut has_default = false;
    let _ = derive.parse_nested_meta(|m| {
        if m.path.is_ident("Default") {
            has_default = true;
        }
        Ok(())
    });
    if has_default {
        return None;
    }

    // Splice textually: find the `#[derive(` opener nearest the struct's
    // span (so multi-derive files don't cross-match), then locate the
    // matching `)]` and insert `, Default` before it.
    let struct_line = item.ident.span().start().line;
    // Scan `#[derive(` occurrences and keep the one whose line is closest to
    // (but not after) the struct definition.
    let needle = "#[derive(";
    let mut chosen: Option<usize> = None;
    let mut cursor = 0;
    while let Some(off) = src[cursor..].find(needle) {
        let abs = cursor + off;
        // Line of `abs` byte: count newlines up to abs.
        let line = src[..abs].bytes().filter(|&b| b == b'\n').count() + 1;
        if line <= struct_line {
            chosen = Some(abs);
        } else {
            break;
        }
        cursor = abs + needle.len();
    }
    let open = chosen?;
    // Find the matching `)` for this derive — track paren depth so we don't
    // get fooled by `derive(Foo<Bar>)`.
    let after_open = open + needle.len();
    let mut depth = 1usize;
    let mut close: Option<usize> = None;
    for (i, ch) in src[after_open..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(after_open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let mut out = String::with_capacity(src.len() + 10);
    out.push_str(&src[..close]);
    let trimmed_before = src[..close].trim_end();
    if trimmed_before.ends_with('(') {
        out.push_str("Default");
    } else {
        out.push_str(", Default");
    }
    out.push_str(&src[close..]);
    Some(out)
}

/// Expand each `resources:` entry into the equivalent model + store + 5 server
/// fns + 2 screens. Synth server fns are returned separately because they
/// carry custom bodies that the standard server-fn generator can't emit.
pub(super) fn expand_resources(doc: &mut DslDoc) -> Result<Vec<SynthServerFn>, String> {
    let resources = std::mem::take(&mut doc.resources);
    let mut synth = Vec::new();
    let mut existing_models: BTreeSet<String> =
        doc.models.iter().map(|m| m.name.to_snake_case()).collect();
    let mut existing_stores: BTreeSet<String> =
        doc.stores.iter().map(|s| s.name.to_snake_case()).collect();

    for r in &resources {
        let res_pascal = r.name.to_pascal_case();
        let res_snake = r.name.to_snake_case();
        let id_field = r.id_field.as_deref().unwrap_or("id").to_snake_case();
        if !r.fields.iter().any(|f| f.name.to_snake_case() == id_field) {
            return Err(format!(
                "resource {:?} must declare its id field {id_field:?} in `fields`",
                r.name
            ));
        }
        let id_type = r
            .fields
            .iter()
            .find(|f| f.name.to_snake_case() == id_field)
            .map(|f| f.ty.clone())
            .unwrap_or_else(|| "i64".into());
        // Explicit override wins; otherwise fall back to the built-in
        // pluralizer. Snake-case the override too so irregular forms still
        // produce valid URL slugs / fn names.
        let plural = r
            .plural
            .as_deref()
            .map(|p| p.to_snake_case())
            .unwrap_or_else(|| pluralize(&res_snake));
        // Default URL slugs are kebab-case (web convention): a model named
        // `StockMovement` lands at `/stock-movements`, not `/stock_movements`.
        // User-supplied `route_base` is taken verbatim.
        let route_base = r
            .route_base
            .clone()
            .unwrap_or_else(|| format!("/{}", plural.replace('_', "-")));
        let store_pascal = format!("{res_pascal}Store");
        let store_snake = format!("{res_snake}_store");

        // 1. Model — synthesize unless already declared. Default is forced
        // (here AND when patching an in-doc pre-declared model below) because
        // resource expansion turns on emit_tests for the store, and the
        // synthesized CRUD tests call `Model::default()`. Without this, tests
        // wouldn't compile.
        if existing_models.insert(res_snake.clone()) {
            let mut derives = r.derives.clone();
            if !derives.iter().any(|d| d == "Default") {
                derives.push("Default".into());
            }
            doc.models.push(DslModel {
                name: res_pascal.clone(),
                fields: r.fields.clone(),
                derives,
            });
        } else if let Some(m) = doc
            .models
            .iter_mut()
            .find(|m| m.name.to_snake_case() == res_snake)
            && !m.derives.iter().any(|d| d == "Default")
        {
            m.derives.push("Default".into());
        }

        // 2. Store — synthesize unless already declared.
        if existing_stores.insert(store_snake.clone()) {
            doc.stores.push(DslStore {
                name: store_pascal.clone(),
                resource: res_pascal.clone(),
                kind: Some("in_memory".into()),
                id_field: Some(id_field.clone()),
                id_type: Some(id_type.clone()),
                // Resource expansion forces Default on the synthesized model,
                // so the auto-generated CRUD tests will compile.
                emit_tests: Some(true),
            });
        }

        // 3. Server fns
        let store_path = format!("crate::state::{store_snake}::{store_pascal}");
        let list_name = format!("list_{plural}");
        let get_name = format!("get_{res_snake}");
        let create_name = format!("create_{res_snake}");
        let update_name = format!("update_{res_snake}");
        let delete_name = format!("delete_{res_snake}");

        let mk_body = |call: &str| {
            format!(
                "    #[cfg(feature = \"server\")]\n    {{\n        return Ok({call});\n    }}\n    #[cfg(not(feature = \"server\"))]\n    {{\n        unreachable!()\n    }}"
            )
        };

        synth.push(SynthServerFn {
            name: list_name.clone(),
            args: vec![],
            return_type: format!("Vec<crate::model::{res_pascal}>"),
            method: "get",
            path: format!("/api{route_base}"),
            body: mk_body(&format!("{store_path}::global().list()")),
        });
        synth.push(SynthServerFn {
            name: get_name.clone(),
            args: vec![("id".into(), id_type.clone())],
            return_type: format!("Option<crate::model::{res_pascal}>"),
            method: "post",
            path: format!("/api{route_base}/get"),
            body: mk_body(&format!("{store_path}::global().get(id)")),
        });
        synth.push(SynthServerFn {
            name: create_name.clone(),
            args: vec![("item".into(), format!("crate::model::{res_pascal}"))],
            return_type: format!("crate::model::{res_pascal}"),
            method: "post",
            path: format!("/api{route_base}"),
            body: mk_body(&format!("{store_path}::global().create(item)")),
        });
        synth.push(SynthServerFn {
            name: update_name.clone(),
            args: vec![("item".into(), format!("crate::model::{res_pascal}"))],
            return_type: format!("Option<crate::model::{res_pascal}>"),
            method: "post",
            path: format!("/api{route_base}/update"),
            body: mk_body(&format!("{store_path}::global().update(item)")),
        });
        synth.push(SynthServerFn {
            name: delete_name.clone(),
            args: vec![("id".into(), id_type.clone())],
            return_type: "bool".into(),
            method: "post",
            path: format!("/api{route_base}/delete"),
            body: mk_body(&format!("{store_path}::global().delete(id)")),
        });

        // 4. Screens: list + new + edit. The edit screen takes an `id`
        //    path-param so the Routable variant has `{ id: <id_type> }`.
        let list_screen = format!("{res_pascal}ListScreen");
        let new_screen = format!("{res_pascal}NewScreen");
        let edit_screen = format!("{res_pascal}EditScreen");
        let new_route = format!("{route_base}/new");
        let non_id_fields: Vec<DslFieldDef> = r
            .fields
            .iter()
            .filter(|f| f.name.to_snake_case() != id_field)
            .map(|f| DslFieldDef {
                name: f.name.clone(),
                ty: field_type_for_model_field(&f.ty),
                validation: None,
                rust_type: Some(f.ty.clone()),
                optional: f.optional,
            })
            .collect();

        let crud = CrudCtx {
            model_pascal: res_pascal.clone(),
            model_fields: r.fields.clone(),
            id_field: id_field.clone(),
            id_type: id_type.clone(),
            list_endpoint: list_name.clone(),
            get_endpoint: get_name.clone(),
            update_endpoint: update_name.clone(),
            delete_endpoint: delete_name.clone(),
            list_route: route_base.clone(),
            new_route: new_route.clone(),
        };

        doc.screens.push(DslScreen {
            name: list_screen,
            route: route_base.clone(),
            wrap_with: None,
            template: Some(DslScreenTemplate {
                kind: "resource_list".into(),
                endpoint: Some(list_name.clone()),
                item_type: Some(res_pascal.clone()),
                on_submit: None,
                redirect_to: None,
                fields: vec![],
                store: None,
                label_field: None,
                checkbox_field: None,
                class: None,
                body: None,
                styled: None,
                crud: Some(crud.clone()),
            }),
            route_params: Vec::new(),
            replace_route: false,
        });
        doc.screens.push(DslScreen {
            name: new_screen,
            route: new_route.clone(),
            wrap_with: None,
            template: Some(DslScreenTemplate {
                kind: "resource_form".into(),
                endpoint: Some(create_name.clone()),
                // Bare model name — the screen template emits the
                // `use crate::model::{item_type};` import itself.
                item_type: Some(res_pascal.clone()),
                on_submit: Some(create_name.clone()),
                redirect_to: Some(route_base.clone()),
                fields: non_id_fields.clone(),
                store: None,
                label_field: None,
                checkbox_field: None,
                class: None,
                body: None,
                styled: None,
                crud: Some(crud.clone()),
            }),
            route_params: Vec::new(),
            replace_route: false,
        });
        doc.screens.push(DslScreen {
            name: edit_screen,
            route: format!("{route_base}/:id/edit"),
            wrap_with: None,
            template: Some(DslScreenTemplate {
                kind: "resource_edit_form".into(),
                endpoint: Some(get_name.clone()),
                item_type: Some(res_pascal.clone()),
                on_submit: Some(update_name.clone()),
                redirect_to: Some(route_base.clone()),
                fields: non_id_fields,
                store: None,
                label_field: None,
                checkbox_field: None,
                class: None,
                body: None,
                styled: None,
                crud: Some(crud),
            }),
            route_params: vec![("id".to_string(), id_type.clone())],
            replace_route: false,
        });
    }
    Ok(synth)
}

/// Map a model field type onto the form-input kind used by the form template.
/// Anything non-trivial defaults to "text" — the user can post-edit.
pub(super) fn field_type_for_model_field(ty: &str) -> String {
    match ty {
        "bool" => "checkbox".into(),
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "isize" | "f32"
        | "f64" => "number".into(),
        _ => "text".into(),
    }
}

pub(super) fn pluralize(snake: &str) -> String {
    if snake.ends_with('s')
        || snake.ends_with("sh")
        || snake.ends_with("ch")
        || snake.ends_with('x')
        || snake.ends_with('z')
    {
        format!("{snake}es")
    } else if snake.ends_with('y') {
        let chars: Vec<char> = snake.chars().collect();
        if chars.len() >= 2 && !"aeiou".contains(chars[chars.len() - 2]) {
            let mut s = snake.to_string();
            s.pop();
            s.push_str("ies");
            return s;
        }
        format!("{snake}s")
    } else {
        format!("{snake}s")
    }
}

pub(super) async fn generate_synth_server_fn(
    state: &Arc<State>,
    crate_root: &Path,
    sf: &SynthServerFn,
    project_root: Option<&str>,
) -> Result<ScaffoldResult, String> {
    // Reuse the fullstack-capable check by detecting through ProjectInfo.
    let project = match project_root {
        Some(root) => crate::project::ProjectInfo::detect(std::path::Path::new(root)),
        None => state.project.lock().await.clone(),
    };
    let active = &project.dioxus_features;
    let fullstack_capable = active.iter().any(|f| f == "fullstack")
        || (active.iter().any(|f| f == "server") && active.iter().any(|f| f == "web"));
    if !fullstack_capable {
        return Err(
            "this project does not have `fullstack` (or `web`+`server`) enabled on the dioxus dep; \
             resource: server fns require a fullstack setup. Run audit_feature_flags for guidance."
                .into(),
        );
    }

    let snake = sf.name.to_snake_case();
    let server_dir = crate_root.join("src/server");
    std::fs::create_dir_all(&server_dir).map_err(|e| e.to_string())?;
    let target = server_dir.join(format!("{snake}.rs"));
    if target.exists() {
        return Err(format!("{} already exists", target.display()));
    }
    let body = render(
        "server_fn_body",
        SERVER_FN_WITH_BODY_TPL,
        context! {
            snake => snake.clone(),
            ret => sf.return_type.clone(),
            method => sf.method,
            path => sf.path.clone(),
            args => sf.args.iter().map(|(n, t)| context!{ name => n.clone(), ty => t.clone() }).collect::<Vec<_>>(),
            body => sf.body.clone(),
            extra_uses => Vec::<String>::new(),
        },
    )?;
    std::fs::write(&target, body).map_err(|e| e.to_string())?;
    let mod_rs = server_dir.join("mod.rs");
    let upsert = upsert_mod_entry(&mod_rs, &snake, None, true)?;
    let (files_created, files_modified) = match upsert {
        ModUpsert::Created => (vec![target, mod_rs], vec![]),
        ModUpsert::Modified => (vec![target], vec![mod_rs]),
        ModUpsert::Unchanged => (vec![target], vec![]),
    };
    Ok(ScaffoldResult {
        files_created,
        files_modified,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_crud_screen_auto_adds_default_to_referenced_model() {
        // The client_crud Screen body uses `..Default::default()` in the
        // "add" form constructor. If the user-authored model didn't include
        // `Default` in `derives:`, we should add it during the pre-pass so
        // the generated code compiles.
        let mut doc: DslDoc = serde_yml::from_str(
            r#"version: "1"
models:
  - name: Todo
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
      - {name: done, type: bool}
client_stores:
  - name: TodoStore
    item_type: Todo
    id_field: id
    id_type: i64
screens:
  - name: TodoScreen
    route: /
    template:
      kind: client_crud
      store: TodoStore
      item_type: Todo
      label_field: title
"#,
        )
        .unwrap();
        // Sanity: user didn't ask for Default.
        assert!(doc.models[0].derives.is_empty());

        ensure_default_on_client_crud_models(&mut doc);
        assert!(
            doc.models[0].derives.iter().any(|d| d == "Default"),
            "expected `Default` auto-added to Todo model, got derives = {:?}",
            doc.models[0].derives
        );

        // Idempotent: running the pre-pass again is a no-op.
        ensure_default_on_client_crud_models(&mut doc);
        let default_count = doc.models[0]
            .derives
            .iter()
            .filter(|d| *d == "Default")
            .count();
        assert_eq!(
            default_count, 1,
            "auto-add must be idempotent, got derives = {:?}",
            doc.models[0].derives
        );
    }

    #[test]
    fn client_crud_screen_respects_existing_default_derive() {
        let mut doc: DslDoc = serde_yml::from_str(
            r#"version: "1"
models:
  - name: Todo
    derives: [Default]
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
client_stores:
  - name: TodoStore
    item_type: Todo
    id_field: id
    id_type: i64
screens:
  - name: TodoScreen
    route: /
    template:
      kind: client_crud
      store: TodoStore
      item_type: Todo
      label_field: title
"#,
        )
        .unwrap();
        ensure_default_on_client_crud_models(&mut doc);
        assert_eq!(doc.models[0].derives, vec!["Default".to_string()]);
    }
}
