#![allow(unused_imports)]
use super::*;

fn cargo_toml_with_fullstack(name: &str) -> String {
    format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = {{ version = "0.7", features = ["fullstack"] }}
"#
    )
}

mod client_crud;
mod dx_components_install;
mod get_dsl_spec;
mod modify_and_persistence;
mod plan_and_preflight;
mod resources;
mod scaffold_core;
