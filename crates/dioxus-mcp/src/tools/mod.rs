pub mod analysis;
pub mod asset_audit;
pub mod dead_components;
pub mod docs;
pub mod dsl;
pub mod lint_project;
pub mod openapi_spec;
pub mod project_index;
pub mod project_tour;
pub mod prop_drill;
pub mod props_lint;
pub mod route_map;
pub mod runtime_events;
pub mod scaffold;
pub mod scan;
pub mod server_fn_call_graph;
pub mod server_fn_summary;
pub mod signal_lint;

pub(crate) fn tighten_type(s: &str) -> String {
    // proc-macro2's stringification spaces every punct: `Vec < String >`, `T :: U`.
    s.replace(" < ", "<")
        .replace(" > ", ">")
        .replace("< ", "<")
        .replace(" >", ">")
        .replace(" , ", ", ")
        .replace(" :: ", "::")
}
