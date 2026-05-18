//! Documentation and example lookups.
//!
//! `search_docs` queries dioxuslabs.com (15-min cache). `find_example` ranks
//! upstream Dioxus example folders plus a small local registry of patterns
//! the upstream repo doesn't ship.

pub mod find_example;
pub mod search_docs;
