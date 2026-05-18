use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use quote::ToTokens;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::state::State;
use crate::tools::ast::{ParseError, collect_parse_errors, walk_rs_files};
use crate::tools::inspect::project_index::{ProjectIndexParams, ServerFnEntry, project_index};
use crate::tools::inspect::route_map::{RouteEntry, RouteMapParams, route_map};
use crate::tools::scaffold::{crate_root, has_derive};
use crate::tools::tighten_type;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct OpenapiSpecParams {
    /// Absolute path to the Dioxus project root. Defaults to the path the MCP server was
    /// started in.
    pub project_root: Option<String>,
    /// Value for `info.title`. Defaults to the crate name from Cargo.toml.
    pub title: Option<String>,
    /// Value for `info.version`. Defaults to the crate version from Cargo.toml.
    pub version: Option<String>,
    /// Path prefix prepended to each server-fn endpoint. Default: "/api".
    pub server_fn_prefix: Option<String>,
    /// If true, also emit GET entries for the router routes (mostly documentary —
    /// they return HTML rather than JSON). Default: false.
    #[serde(default)]
    pub include_routes: bool,
    /// Override the file containing the Routable enum (forwarded to route_map).
    pub router_file: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OpenapiSpecReport {
    /// The generated OpenAPI 3.1 document.
    pub spec: Value,
    /// Type names referenced by server fns that the schema walker couldn't resolve
    /// (no local definition, no primitive mapping). Their schemas in the spec fall
    /// back to `{"type": "object"}`.
    pub unresolved_types: Vec<String>,
    /// Server fns whose path is the function name because no explicit `#[server(Name)]`
    /// was provided — Dioxus may hash these at runtime, so the spec's path is a guess.
    pub guessed_paths: Vec<String>,
    pub parse_errors: Vec<ParseError>,
    pub notes: Vec<&'static str>,
}

pub async fn openapi_spec(
    state: &Arc<State>,
    p: OpenapiSpecParams,
) -> Result<OpenapiSpecReport, String> {
    let crate_root = crate_root(state, p.project_root.as_deref()).await?;
    let src_root = crate_root.join("src");

    let index = project_index(
        state,
        ProjectIndexParams {
            path: None,
            kind: None,
            project_root: p.project_root.clone(),
        },
    )
    .await?;

    let routes = if p.include_routes {
        Some(
            route_map(
                state,
                RouteMapParams {
                    router_file: p.router_file.clone(),
                    project_root: p.project_root.clone(),
                },
            )
            .await?,
        )
    } else {
        None
    };

    let files = walk_rs_files(&src_root);
    let mut resolver = TypeResolver::new();
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        resolver.ingest_file(ast);
    }

    let (title, default_version) = read_crate_metadata(&crate_root);
    let title = p.title.unwrap_or(title);
    let version = p.version.unwrap_or(default_version);
    let prefix = p
        .server_fn_prefix
        .as_deref()
        .unwrap_or("/api")
        .trim_end_matches('/')
        .to_string();

    let mut paths = Map::new();
    let mut guessed_paths = Vec::new();

    for sf in &index.server_fns {
        let (path, guessed) = server_fn_path(&prefix, sf);
        if guessed {
            guessed_paths.push(sf.name.clone());
        }
        let item = server_fn_path_item(sf, &mut resolver);
        paths.insert(path, item);
    }

    if let Some(rm) = &routes {
        for r in &rm.routes {
            let (path, item) = route_path_item(r);
            paths.insert(path, item);
        }
    }

    resolver.ensure_server_fn_error();
    let schemas = resolver.into_schemas();

    let spec = json!({
        "openapi": "3.1.0",
        "info": {
            "title": title,
            "version": version,
        },
        "paths": paths,
        "components": {
            "schemas": schemas,
        },
    });

    let mut unresolved: Vec<String> = resolver_unresolved(&spec);
    unresolved.sort();
    unresolved.dedup();

    let mut notes = vec![
        "server-fn endpoint paths assume the JSON-POST default codec; custom codecs are not detected",
        "for #[server] fns without an explicit Name, the path is the fn ident — Dioxus may hash this at runtime",
        "#[get/post/put/delete/patch(\"/path\")] server fns use their literal path verbatim (no /api prefix applied)",
        "schemas come from local #[derive(Serialize)] / #[derive(Deserialize)] types; unknowns fall back to {type: object}",
    ];
    if routes.is_some() {
        notes.push("router routes are emitted as documentary GETs returning text/html, not JSON");
    }

    Ok(OpenapiSpecReport {
        spec,
        unresolved_types: unresolved,
        guessed_paths,
        parse_errors: collect_parse_errors(&files),
        notes,
    })
}

fn read_crate_metadata(crate_root: &std::path::Path) -> (String, String) {
    let toml_path = crate_root.join("Cargo.toml");
    let Ok(text) = std::fs::read_to_string(&toml_path) else {
        return ("dioxus-app".into(), "0.0.0".into());
    };
    let Ok(parsed) = toml::from_str::<toml::Value>(&text) else {
        return ("dioxus-app".into(), "0.0.0".into());
    };
    let pkg = parsed.get("package");
    let name = pkg
        .and_then(|p| p.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("dioxus-app")
        .to_string();
    let version = pkg
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
        .unwrap_or("0.0.0")
        .to_string();
    (name, version)
}

/// Peel `Result<T, E>` (or any path ending in `Result`) into its generic args.
/// Returns `(success_type_str, error_type_str)`. When the return type doesn't
/// look like a `Result<…>`, returns the input untouched as the success and
/// `None` for the error — letting callers fall back to the `ServerFnError`
/// $ref for the 500 schema.
fn split_server_fn_result(ret: &str) -> (String, Option<String>) {
    if !ret.contains("Result") || !ret.contains('<') {
        return (ret.to_string(), None);
    }
    let Ok(ty) = syn::parse_str::<syn::Type>(ret.trim()) else {
        return (ret.to_string(), None);
    };
    let syn::Type::Path(tp) = ty else {
        return (ret.to_string(), None);
    };
    let Some(seg) = tp.path.segments.last() else {
        return (ret.to_string(), None);
    };
    if seg.ident != "Result" {
        return (ret.to_string(), None);
    }
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return (ret.to_string(), None);
    };
    let type_args: Vec<&syn::Type> = args
        .args
        .iter()
        .filter_map(|a| match a {
            syn::GenericArgument::Type(t) => Some(t),
            _ => None,
        })
        .collect();
    let Some(ok_ty) = type_args.first() else {
        return (ret.to_string(), None);
    };
    let ok_str = tighten_type(&ok_ty.to_token_stream().to_string());
    let err_str = type_args
        .get(1)
        .map(|t| tighten_type(&t.to_token_stream().to_string()));
    (ok_str, err_str)
}

fn server_fn_path(prefix: &str, sf: &ServerFnEntry) -> (String, bool) {
    if let Some(path) = &sf.route_path {
        (path.clone(), false)
    } else if let Some(name) = &sf.server_name {
        (format!("{prefix}/{name}"), false)
    } else {
        (format!("{prefix}/{}", sf.name), true)
    }
}

fn server_fn_path_item(sf: &ServerFnEntry, resolver: &mut TypeResolver) -> Value {
    let mut properties = Map::new();
    let mut required: Vec<String> = Vec::new();
    for a in &sf.args {
        let (schema, optional) = resolver.resolve(&a.ty);
        properties.insert(a.name.clone(), schema);
        if !optional {
            required.push(a.name.clone());
        }
    }
    let mut request_schema = Map::new();
    request_schema.insert("type".into(), json!("object"));
    request_schema.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        request_schema.insert("required".into(), json!(required));
    }

    // Server fns commonly return `Result<T, ServerFnError>` (or an alias);
    // `project_index` already unwraps the `ServerFnResult<T>` alias but a bare
    // `Result<T, E>` arrives here intact. Peel it: T -> 200 schema, E -> 500
    // schema. When E is just `ServerFnError`, we keep the canonical $ref
    // emitted by `ensure_server_fn_error()`; when E names a local serde type
    // we resolve it normally so the spec reflects the real error shape.
    let (success_ty, error_ty) = split_server_fn_result(&sf.return_type);
    // `resolver.resolve` returns `(schema, top_level_optional)`. For
    // *argument* types the optional flag drives whether the property lands
    // in `required`; for *return* types the same flag means the value can
    // be JSON `null`, so we wrap the schema in `nullable(…)` to surface it.
    // Before this, `who_am_i -> Result<Option<String>, ServerFnError>`
    // rendered as `{type: "string"}` and lost the null possibility entirely.
    let (response_schema_raw, response_nullable) = resolver.resolve(&success_ty);
    let response_schema = if response_nullable {
        nullable(response_schema_raw)
    } else {
        response_schema_raw
    };
    let error_schema: Value = match error_ty.as_deref() {
        Some(e) if e.trim() == "ServerFnError" => {
            json!({"$ref": "#/components/schemas/ServerFnError"})
        }
        Some(e) => resolver.resolve(e).0,
        None => json!({"$ref": "#/components/schemas/ServerFnError"}),
    };

    let summary = format!(
        "Dioxus server fn `{}` defined at {}:{}",
        sf.name,
        sf.file.display(),
        sf.line
    );

    let method = sf.method.as_deref().unwrap_or("post");
    let has_body = !matches!(method, "get" | "delete");
    let mut op = Map::new();
    op.insert("operationId".into(), json!(sf.name));
    op.insert("summary".into(), json!(summary));
    if has_body {
        op.insert(
            "requestBody".into(),
            json!({
                "required": true,
                "content": {
                    "application/json": {
                        "schema": Value::Object(request_schema),
                    }
                }
            }),
        );
    }
    op.insert(
        "responses".into(),
        json!({
            "200": {
                "description": "Success",
                "content": {
                    "application/json": {
                        "schema": response_schema,
                    }
                }
            },
            "500": {
                "description": "Server function error",
                "content": {
                    "application/json": {
                        "schema": error_schema,
                    }
                }
            }
        }),
    );

    let mut item = Map::new();
    item.insert(method.to_string(), Value::Object(op));
    Value::Object(item)
}

fn route_path_item(r: &RouteEntry) -> (String, Value) {
    let mut path = String::new();
    for seg in r.full_path.trim_start_matches('/').split('/') {
        path.push('/');
        if let Some(rest) = seg.strip_prefix(':') {
            path.push('{');
            path.push_str(rest);
            path.push('}');
        } else {
            path.push_str(seg);
        }
    }
    if path.is_empty() {
        path.push('/');
    }

    let parameters: Vec<Value> = r
        .params
        .iter()
        .map(|p| {
            json!({
                "name": p.name,
                "in": "path",
                "required": true,
                "schema": primitive_schema(&p.ty)
                    .unwrap_or_else(|| json!({"type": "string"})),
            })
        })
        .collect();

    let item = json!({
        "get": {
            "operationId": format!("route_{}", r.component),
            "summary": format!("Dioxus route -> component `{}`", r.component),
            "parameters": parameters,
            "responses": {
                "200": {
                    "description": "Rendered page",
                    "content": {"text/html": {}},
                }
            }
        }
    });
    (path, item)
}

// ---------------- schema resolver ----------------

#[derive(Default)]
struct TypeResolver {
    /// Local struct/enum defs the resolver can $ref to.
    defs: BTreeMap<String, TypeDef>,
    /// Schemas already emitted by name (the components.schemas map).
    emitted: BTreeMap<String, Value>,
    /// Type names referenced but not resolvable.
    unresolved: BTreeSet<String>,
    /// Names currently being expanded — prevents infinite recursion on cyclic types.
    in_progress: BTreeSet<String>,
}

#[derive(Clone)]
enum TypeDef {
    Struct(syn::ItemStruct),
    Enum(syn::ItemEnum),
}

impl TypeResolver {
    fn new() -> Self {
        Self::default()
    }

    fn ingest_file(&mut self, file: &syn::File) {
        for item in &file.items {
            match item {
                syn::Item::Struct(s) if is_serde_type(&s.attrs) => {
                    self.defs
                        .insert(s.ident.to_string(), TypeDef::Struct(s.clone()));
                }
                syn::Item::Enum(e) if is_serde_type(&e.attrs) => {
                    self.defs
                        .insert(e.ident.to_string(), TypeDef::Enum(e.clone()));
                }
                _ => {}
            }
        }
    }

    /// Resolve a stringified type to a schema. Returns `(schema, top_level_optional)`
    /// — when `top_level_optional` is true the field comes from an `Option<T>` and
    /// should not be marked `required` in its parent object.
    fn resolve(&mut self, ty_str: &str) -> (Value, bool) {
        let trimmed = ty_str.trim();
        if trimmed.is_empty() || trimmed == "()" {
            return (json!({"type": "null"}), false);
        }
        match syn::parse_str::<syn::Type>(trimmed) {
            Ok(t) => self.resolve_ty(&t, true),
            Err(_) => {
                self.unresolved.insert(trimmed.to_string());
                (
                    json!({
                        "type": "object",
                        "description": format!("unresolved type: {trimmed}"),
                    }),
                    false,
                )
            }
        }
    }

    fn resolve_ty(&mut self, ty: &syn::Type, top_level: bool) -> (Value, bool) {
        match ty {
            syn::Type::Path(tp) => {
                let segs = &tp.path.segments;
                let Some(last) = segs.last() else {
                    return (json!({"type": "object"}), false);
                };
                let ident = last.ident.to_string();
                let generics = generic_args(&last.arguments);

                if ident == "Option"
                    && let Some(inner) = generics.first()
                {
                    let (inner_schema, _) = self.resolve_ty(inner, false);
                    if top_level {
                        return (inner_schema, true);
                    }
                    return (nullable(inner_schema), true);
                }

                if matches!(
                    ident.as_str(),
                    "Vec" | "VecDeque" | "HashSet" | "BTreeSet" | "LinkedList"
                ) && let Some(inner) = generics.first()
                {
                    let (items, _) = self.resolve_ty(inner, false);
                    return (json!({"type": "array", "items": items}), false);
                }

                if matches!(ident.as_str(), "HashMap" | "BTreeMap" | "IndexMap")
                    && let Some(v) = generics.get(1)
                {
                    let (inner, _) = self.resolve_ty(v, false);
                    return (
                        json!({"type": "object", "additionalProperties": inner}),
                        false,
                    );
                }

                if matches!(
                    ident.as_str(),
                    "Box" | "Rc" | "Arc" | "Cow" | "RefCell" | "Cell" | "Mutex" | "RwLock"
                ) {
                    let inner_idx = if ident == "Cow" { 1 } else { 0 };
                    if let Some(inner) = generics.get(inner_idx) {
                        return self.resolve_ty(inner, top_level);
                    }
                }

                if let Some(prim) = primitive_schema(&ident) {
                    return (prim, false);
                }

                if self.defs.contains_key(&ident) {
                    self.materialize(&ident);
                    return (
                        json!({"$ref": format!("#/components/schemas/{ident}")}),
                        false,
                    );
                }

                self.unresolved.insert(ident.clone());
                (
                    json!({
                        "type": "object",
                        "description": format!("unresolved type: {ident}"),
                    }),
                    false,
                )
            }
            syn::Type::Reference(r) => self.resolve_ty(&r.elem, top_level),
            syn::Type::Tuple(t) => {
                if t.elems.is_empty() {
                    return (json!({"type": "null"}), false);
                }
                let items: Vec<Value> = t
                    .elems
                    .iter()
                    .map(|e| self.resolve_ty(e, false).0)
                    .collect();
                (
                    json!({
                        "type": "array",
                        "prefixItems": items,
                        "minItems": t.elems.len(),
                        "maxItems": t.elems.len(),
                    }),
                    false,
                )
            }
            syn::Type::Array(a) => {
                let (items, _) = self.resolve_ty(&a.elem, false);
                (json!({"type": "array", "items": items}), false)
            }
            syn::Type::Slice(s) => {
                let (items, _) = self.resolve_ty(&s.elem, false);
                (json!({"type": "array", "items": items}), false)
            }
            other => {
                let s = tighten_type(&other.to_token_stream().to_string());
                self.unresolved.insert(s.clone());
                (
                    json!({
                        "type": "object",
                        "description": format!("unresolved type: {s}"),
                    }),
                    false,
                )
            }
        }
    }

    fn materialize(&mut self, name: &str) {
        if self.emitted.contains_key(name) || self.in_progress.contains(name) {
            return;
        }
        let Some(def) = self.defs.get(name).cloned() else {
            return;
        };
        self.in_progress.insert(name.to_string());
        let schema = match def {
            TypeDef::Struct(s) => self.struct_schema(&s),
            TypeDef::Enum(e) => self.enum_schema(&e),
        };
        self.in_progress.remove(name);
        self.emitted.insert(name.to_string(), schema);
    }

    fn struct_schema(&mut self, s: &syn::ItemStruct) -> Value {
        match &s.fields {
            syn::Fields::Named(named) => {
                let mut props = Map::new();
                let mut required = Vec::new();
                for f in &named.named {
                    if has_serde_skip(&f.attrs) {
                        continue;
                    }
                    let raw = f.ident.as_ref().map(|i| i.to_string()).unwrap_or_default();
                    let name = serde_rename(&f.attrs).unwrap_or(raw);
                    let ty_str = tighten_type(&f.ty.to_token_stream().to_string());
                    let (schema, optional) = self.resolve(&ty_str);
                    props.insert(name.clone(), schema);
                    if !optional {
                        required.push(name);
                    }
                }
                let mut obj = Map::new();
                obj.insert("type".into(), json!("object"));
                obj.insert("properties".into(), Value::Object(props));
                if !required.is_empty() {
                    obj.insert("required".into(), json!(required));
                }
                Value::Object(obj)
            }
            syn::Fields::Unnamed(unnamed) if unnamed.unnamed.len() == 1 => {
                let only = unnamed.unnamed.first().unwrap();
                let ty_str = tighten_type(&only.ty.to_token_stream().to_string());
                self.resolve(&ty_str).0
            }
            syn::Fields::Unnamed(unnamed) => {
                let items: Vec<Value> = unnamed
                    .unnamed
                    .iter()
                    .map(|f| {
                        let ty_str = tighten_type(&f.ty.to_token_stream().to_string());
                        self.resolve(&ty_str).0
                    })
                    .collect();
                let n = unnamed.unnamed.len();
                json!({
                    "type": "array",
                    "prefixItems": items,
                    "minItems": n,
                    "maxItems": n,
                })
            }
            syn::Fields::Unit => json!({"type": "null"}),
        }
    }

    fn enum_schema(&mut self, e: &syn::ItemEnum) -> Value {
        let all_unit = e
            .variants
            .iter()
            .all(|v| matches!(v.fields, syn::Fields::Unit));
        if all_unit {
            let names: Vec<String> = e
                .variants
                .iter()
                .map(|v| serde_rename(&v.attrs).unwrap_or_else(|| v.ident.to_string()))
                .collect();
            return json!({"type": "string", "enum": names});
        }
        // Externally-tagged serde default: { "VariantName": <inner> }
        let one_of: Vec<Value> = e
            .variants
            .iter()
            .map(|v| {
                let name = serde_rename(&v.attrs).unwrap_or_else(|| v.ident.to_string());
                let inner = match &v.fields {
                    syn::Fields::Unit => json!({"type": "null"}),
                    syn::Fields::Unnamed(u) if u.unnamed.len() == 1 => {
                        let ty_str = tighten_type(
                            &u.unnamed.first().unwrap().ty.to_token_stream().to_string(),
                        );
                        self.resolve(&ty_str).0
                    }
                    syn::Fields::Unnamed(u) => {
                        let items: Vec<Value> = u
                            .unnamed
                            .iter()
                            .map(|f| {
                                let ty_str = tighten_type(&f.ty.to_token_stream().to_string());
                                self.resolve(&ty_str).0
                            })
                            .collect();
                        let n = u.unnamed.len();
                        json!({
                            "type": "array",
                            "prefixItems": items,
                            "minItems": n,
                            "maxItems": n,
                        })
                    }
                    syn::Fields::Named(n) => {
                        let mut props = Map::new();
                        let mut required = Vec::new();
                        for f in &n.named {
                            if has_serde_skip(&f.attrs) {
                                continue;
                            }
                            let raw = f.ident.as_ref().map(|i| i.to_string()).unwrap_or_default();
                            let fname = serde_rename(&f.attrs).unwrap_or(raw);
                            let ty_str = tighten_type(&f.ty.to_token_stream().to_string());
                            let (schema, optional) = self.resolve(&ty_str);
                            props.insert(fname.clone(), schema);
                            if !optional {
                                required.push(fname);
                            }
                        }
                        let mut obj = Map::new();
                        obj.insert("type".into(), json!("object"));
                        obj.insert("properties".into(), Value::Object(props));
                        if !required.is_empty() {
                            obj.insert("required".into(), json!(required));
                        }
                        Value::Object(obj)
                    }
                };
                json!({
                    "type": "object",
                    "properties": {name.clone(): inner},
                    "required": [name],
                })
            })
            .collect();
        json!({"oneOf": one_of})
    }

    fn ensure_server_fn_error(&mut self) {
        self.emitted
            .entry("ServerFnError".to_string())
            .or_insert_with(|| {
                json!({
                    "type": "object",
                    "properties": {
                        "message": {"type": "string"}
                    },
                    "required": ["message"],
                })
            });
    }

    fn into_schemas(self) -> Map<String, Value> {
        self.emitted.into_iter().collect()
    }
}

fn primitive_schema(name: &str) -> Option<Value> {
    Some(match name {
        "bool" => json!({"type": "boolean"}),
        "String" | "str" | "char" | "PathBuf" | "Path" | "OsString" | "OsStr" => {
            json!({"type": "string"})
        }
        "u8" | "u16" | "u32" | "i8" | "i16" | "i32" => {
            json!({"type": "integer", "format": "int32"})
        }
        "u64" | "u128" | "usize" | "i64" | "i128" | "isize" => {
            json!({"type": "integer", "format": "int64"})
        }
        "f32" => json!({"type": "number", "format": "float"}),
        "f64" => json!({"type": "number", "format": "double"}),
        "Uuid" => json!({"type": "string", "format": "uuid"}),
        "DateTime" | "OffsetDateTime" | "PrimitiveDateTime" => {
            json!({"type": "string", "format": "date-time"})
        }
        "NaiveDate" | "Date" => json!({"type": "string", "format": "date"}),
        "NaiveTime" | "Time" => json!({"type": "string", "format": "time"}),
        "Value" | "JsonValue" => json!({}),
        _ => return None,
    })
}

fn nullable(schema: Value) -> Value {
    // OpenAPI 3.1: prefer {"type": ["X", "null"]} when possible; otherwise oneOf.
    if let Value::Object(ref obj) = schema
        && let Some(Value::String(t)) = obj.get("type")
    {
        let mut next = obj.clone();
        next.insert("type".into(), json!([t, "null"]));
        return Value::Object(next);
    }
    json!({"oneOf": [schema, {"type": "null"}]})
}

fn generic_args(args: &syn::PathArguments) -> Vec<syn::Type> {
    let syn::PathArguments::AngleBracketed(ab) = args else {
        return Vec::new();
    };
    ab.args
        .iter()
        .filter_map(|a| match a {
            syn::GenericArgument::Type(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

fn is_serde_type(attrs: &[syn::Attribute]) -> bool {
    attrs
        .iter()
        .any(|a| has_derive(a, "Serialize") || has_derive(a, "Deserialize"))
}

fn has_serde_skip(attrs: &[syn::Attribute]) -> bool {
    for a in attrs {
        if !a.path().is_ident("serde") {
            continue;
        }
        let mut found = false;
        let _ = a.parse_nested_meta(|m| {
            if m.path.is_ident("skip")
                || m.path.is_ident("skip_serializing")
                || m.path.is_ident("skip_deserializing")
            {
                found = true;
            }
            Ok(())
        });
        if found {
            return true;
        }
    }
    false
}

fn serde_rename(attrs: &[syn::Attribute]) -> Option<String> {
    for a in attrs {
        if !a.path().is_ident("serde") {
            continue;
        }
        let mut name: Option<String> = None;
        let _ = a.parse_nested_meta(|m| {
            if m.path.is_ident("rename") {
                let v: syn::LitStr = m.value()?.parse()?;
                name = Some(v.value());
            }
            Ok(())
        });
        if name.is_some() {
            return name;
        }
    }
    None
}

/// Crawl the final spec for `unresolved type: X` descriptions to surface in the report.
fn resolver_unresolved(spec: &Value) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(map) => {
                if let Some(Value::String(d)) = map.get("description")
                    && let Some(rest) = d.strip_prefix("unresolved type: ")
                {
                    out.push(rest.to_string());
                }
                for (_, child) in map {
                    walk(child, out);
                }
            }
            Value::Array(arr) => {
                for child in arr {
                    walk(child, out);
                }
            }
            _ => {}
        }
    }
    walk(spec, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Result<T, ServerFnError> -> success T, error "ServerFnError" — the
    /// canonical shape every dioxus 0.7 fullstack server fn ends up with.
    #[test]
    fn split_result_extracts_success_and_error() {
        let (ok, err) = split_server_fn_result("Result<Board, ServerFnError>");
        assert_eq!(ok, "Board");
        assert_eq!(err.as_deref(), Some("ServerFnError"));
    }

    /// Nested generics on the success side are preserved verbatim — the
    /// resolver picks them apart further once we hand them back as a string.
    #[test]
    fn split_result_preserves_nested_generics() {
        let (ok, err) = split_server_fn_result("Result<Vec<Card>, ServerFnError>");
        assert_eq!(ok, "Vec<Card>");
        assert_eq!(err.as_deref(), Some("ServerFnError"));
    }

    /// Anything that isn't a `Result<…>` flows through untouched, so plain
    /// return types (e.g. `String`, when a server fn isn't fallible) keep
    /// hitting the resolver exactly as they did before.
    #[test]
    fn split_result_passes_non_result_types_through() {
        let (ok, err) = split_server_fn_result("Board");
        assert_eq!(ok, "Board");
        assert!(err.is_none());
    }

    /// A `Result` without explicit type args (shouldn't actually happen on a
    /// real signature, but we don't want to panic if it does) keeps the
    /// passthrough behavior so the rest of the spec still emits.
    #[test]
    fn split_result_on_unparseable_passes_through() {
        let (ok, err) = split_server_fn_result("Result");
        assert_eq!(ok, "Result");
        assert!(err.is_none());
    }

    /// Reproduces the standup `who_am_i` case: a server fn whose return
    /// type is `Result<Option<String>, ServerFnError>`. The TODO called out
    /// the bug — the resolver dropped the Option entirely and emitted
    /// `{type: "string"}`. With the `nullable(…)` wrap on top-level
    /// optional success types, the spec now emits a 3.1-compliant
    /// `{type: ["string", "null"]}` so consumers can model the absent case.
    #[test]
    fn option_return_type_emits_nullable_string() {
        let sf = ServerFnEntry {
            file: std::path::PathBuf::from("src/server/auth.rs"),
            line: 10,
            name: "who_am_i".into(),
            method: Some("get".into()),
            server_name: None,
            route_path: None,
            args: Vec::new(),
            return_type: "Result<Option<String>, ServerFnError>".into(),
        };
        let mut resolver = TypeResolver::default();
        let path_item = server_fn_path_item(&sf, &mut resolver);

        // Drill down into `responses.200.content.application/json.schema`.
        let schema = path_item
            .get("get")
            .and_then(|v| v.get("responses"))
            .and_then(|v| v.get("200"))
            .and_then(|v| v.get("content"))
            .and_then(|v| v.get("application/json"))
            .and_then(|v| v.get("schema"))
            .expect("200 response schema present");

        // OpenAPI 3.1 nullable shape: `{"type": ["string", "null"]}`.
        // `nullable(…)` widens `type` to an array — assert on both
        // members so a regression that loses the `"null"` is caught.
        let ty = schema.get("type").expect("schema.type present");
        let ty_arr = ty.as_array().unwrap_or_else(|| {
            panic!("expected type to widen to an array under Option<T>; got {schema}")
        });
        let names: Vec<&str> = ty_arr.iter().filter_map(|v| v.as_str()).collect();
        assert!(
            names.contains(&"string"),
            "schema should retain the string type; got {ty:?}"
        );
        assert!(
            names.contains(&"null"),
            "Option<String> in return position must mark the schema nullable; got {ty:?}"
        );
    }

    /// Counter-test: a plain `Result<String, ServerFnError>` keeps the
    /// non-nullable schema. Without this, an over-eager wrap would mark
    /// every success schema nullable.
    #[test]
    fn non_option_return_type_stays_non_nullable() {
        let sf = ServerFnEntry {
            file: std::path::PathBuf::from("src/server/echo.rs"),
            line: 1,
            name: "echo".into(),
            method: Some("post".into()),
            server_name: None,
            route_path: None,
            args: Vec::new(),
            return_type: "Result<String, ServerFnError>".into(),
        };
        let mut resolver = TypeResolver::default();
        let path_item = server_fn_path_item(&sf, &mut resolver);
        let schema = path_item
            .get("post")
            .and_then(|v| v.get("responses"))
            .and_then(|v| v.get("200"))
            .and_then(|v| v.get("content"))
            .and_then(|v| v.get("application/json"))
            .and_then(|v| v.get("schema"))
            .expect("200 response schema present");
        assert_eq!(
            schema.get("type"),
            Some(&json!("string")),
            "non-Option return type stays as a plain string schema; got {schema}"
        );
    }
}
