use std::sync::Arc;

use heck::ToSnakeCase;
use minijinja::{Environment, context};

use crate::state::State;

use super::mod_tree::upsert_mod_entry;
use super::types::{ArgSpec, CreateServerFnParams, ModUpsert, ScaffoldResult};

const SERVER_FN_TPL: &str = r#"use dioxus::prelude::*;

#[{{ method }}("{{ path }}"
{%- for e in extractors %}, {{ e.name }}: {{ e.ty }}{% endfor -%}
)]
pub async fn {{ snake }}(
{%- for a in args %}
    {{ a.name }}: {{ a.ty }},
{%- endfor %}
) -> Result<{{ ret }}, ServerFnError> {
{%- if auth_required %}
    let session_id = cookies
        .get("{{ session_cookie }}")
        .ok_or_else(|| ServerFnError::new("not logged in"))?
        .to_string();
    // TODO touch_session(&session_id).await?; — wire to your session store
    // (extend, refresh, or invalidate). Returning ServerFnError::new("session
    // expired") is the canonical mapping when the lookup fails.
    let _ = session_id;
{%- endif %}
    Ok(Default::default())
}
"#;

/// The server-fn template already wraps the return type in
/// `Result<_, ServerFnError>`. Reject callers that pre-wrap it themselves —
/// the resulting `Result<Result<_, ServerFnError>, ServerFnError>` compiles but
/// is silently wrong. Returns `Some(error_message)` if the caller's type is
/// already a server-fn result wrapper, else `None`.
pub fn check_inner_return_type(ret: &str) -> Option<String> {
    let parsed: syn::Type = match syn::parse_str(ret) {
        Ok(t) => t,
        // If it doesn't parse, leave validation to the compiler — we don't want
        // to swallow surprising errors here.
        Err(_) => return None,
    };
    let path = match &parsed {
        syn::Type::Path(tp) => &tp.path,
        _ => return None,
    };
    let last = path.segments.last()?;
    let ident = last.ident.to_string();
    let args = match &last.arguments {
        syn::PathArguments::AngleBracketed(a) => a,
        _ => return None,
    };
    let type_args: Vec<&syn::Type> = args
        .args
        .iter()
        .filter_map(|a| match a {
            syn::GenericArgument::Type(t) => Some(t),
            _ => None,
        })
        .collect();
    match (ident.as_str(), type_args.as_slice()) {
        ("ServerFnResult", [inner]) => {
            let inner_str = quote::quote!(#inner).to_string();
            Some(format!(
                "return_type `{ret}` already wraps the inner type in ServerFnResult; pass just `{inner_str}` instead — the template wraps it in `Result<_, ServerFnError>` for you."
            ))
        }
        ("Result", [inner, err]) => {
            let err_str = quote::quote!(#err).to_string();
            let err_norm = err_str.replace(' ', "");
            if err_norm.ends_with("ServerFnError") {
                let inner_str = quote::quote!(#inner).to_string();
                Some(format!(
                    "return_type `{ret}` already wraps the inner type in `Result<_, ServerFnError>`; pass just `{inner_str}` instead — the template wraps it for you."
                ))
            } else {
                None
            }
        }
        _ => None,
    }
}

pub async fn create_server_fn(
    state: &Arc<State>,
    p: CreateServerFnParams,
) -> Result<ScaffoldResult, String> {
    let project = match p.project_root.as_deref() {
        Some(root) => crate::project::ProjectInfo::detect(std::path::Path::new(root)),
        None => state.project.lock().await.clone(),
    };
    if !project.fullstack_capable() {
        let hint = if p.project_root.is_none() && !project.is_dioxus_project {
            " (the MCP server's cwd has no Dioxus Cargo.toml — pass `project_root` so the audit \
              reads your real manifest)"
        } else {
            ""
        };
        return Err(format!(
            "this project does not have `fullstack` (or `web`+`server`, or an opt-in \
             `server = [\"dioxus/server\"]` sibling feature) enabled on the dioxus dep; \
             server fns require a fullstack setup. Run audit_feature_flags for guidance.{hint}"
        ));
    }

    let crate_root = project
        .manifest_dir()
        .ok_or_else(|| "no manifest dir".to_string())?;
    let server_dir = crate_root.join("src/server");
    std::fs::create_dir_all(&server_dir).map_err(|e| e.to_string())?;

    let snake = p.name.to_snake_case();
    let ret = p.return_type.unwrap_or_else(|| "String".into());
    if let Some(reason) = check_inner_return_type(&ret) {
        return Err(reason);
    }
    let method = match p.method.as_deref().map(str::to_ascii_lowercase) {
        Some(m) => {
            if !matches!(m.as_str(), "get" | "post") {
                return Err(format!("method must be \"get\" or \"post\", got {m:?}"));
            }
            m
        }
        None => {
            if p.args.is_empty() {
                "get".to_string()
            } else {
                "post".to_string()
            }
        }
    };
    let route_path = p.path.clone().unwrap_or_else(|| format!("/api/{snake}"));
    let target = server_dir.join(format!("{snake}.rs"));
    if target.exists() {
        return Err(format!("{} already exists", target.display()));
    }

    // When auth_required is set, ensure a `cookies: TypedHeader<Cookie>`
    // extractor is present so the prologue compiles. Idempotent — a caller-
    // supplied `cookies` extractor wins (lets them swap in `CookieJar`).
    let mut extractors = p.extractors.clone();
    if p.auth_required && !extractors.iter().any(|e| e.name == "cookies") {
        extractors.insert(
            0,
            ArgSpec {
                name: "cookies".into(),
                ty: "TypedHeader<Cookie>".into(),
            },
        );
    }
    let session_cookie = p
        .session_cookie
        .clone()
        .unwrap_or_else(|| "session_id".into());

    let mut env = Environment::new();
    env.add_template("server_fn", SERVER_FN_TPL).unwrap();
    let tpl = env.get_template("server_fn").unwrap();
    let rendered = tpl
        .render(context! {
            snake => snake.clone(),
            ret => ret.clone(),
            method => method,
            path => route_path,
            args => p.args.iter().map(|a| context!{ name => a.name.clone(), ty => a.ty.clone() }).collect::<Vec<_>>(),
            extractors => extractors.iter().map(|a| context!{ name => a.name.clone(), ty => a.ty.clone() }).collect::<Vec<_>>(),
            auth_required => p.auth_required,
            session_cookie => session_cookie,
        })
        .map_err(|e| e.to_string())?;
    std::fs::write(&target, rendered).map_err(|e| e.to_string())?;

    // ensure src/server/mod.rs declares it
    let mod_rs = server_dir.join("mod.rs");
    let upsert = upsert_mod_entry(&mod_rs, &snake, None)?;
    let (files_created, files_modified) = match upsert {
        ModUpsert::Created => (vec![target, mod_rs], vec![]),
        ModUpsert::Modified => (vec![target], vec![mod_rs]),
        ModUpsert::Unchanged => (vec![target], vec![]),
    };

    let next_steps = vec![
        format!("call `{snake}(...)` from a client component to invoke it"),
        "ensure `mod server;` is declared in your crate root".into(),
    ];

    Ok(ScaffoldResult {
        files_created,
        files_modified,
        next_steps,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::check_inner_return_type;

    #[test]
    fn accepts_bare_types() {
        assert!(check_inner_return_type("String").is_none());
        assert!(check_inner_return_type("Vec<Product>").is_none());
        assert!(check_inner_return_type("Option<i64>").is_none());
        assert!(check_inner_return_type("crate::model::Product").is_none());
    }

    #[test]
    fn rejects_result_serverfnerror() {
        let e = check_inner_return_type("Result<Vec<String>, ServerFnError>").unwrap();
        assert!(e.contains("already wraps"));
        assert!(e.contains("Vec < String >") || e.contains("Vec<String>"));
    }

    #[test]
    fn rejects_serverfnresult() {
        let e = check_inner_return_type("ServerFnResult<Vec<String>>").unwrap();
        assert!(e.contains("ServerFnResult"));
    }

    #[test]
    fn rejects_qualified_serverfnerror() {
        let e =
            check_inner_return_type("Result<Vec<String>, dioxus::prelude::ServerFnError>").unwrap();
        assert!(e.contains("already wraps"));
    }

    #[test]
    fn accepts_result_with_different_error() {
        assert!(check_inner_return_type("Result<Vec<String>, MyError>").is_none());
    }
}
