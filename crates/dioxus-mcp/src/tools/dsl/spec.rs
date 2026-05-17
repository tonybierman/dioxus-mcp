use std::collections::BTreeSet;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::specs::*;
use crate::state::State;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GetDslSpecParams {
    /// Optional list of extension modules to include. One or more of:
    /// "crud", "realtime", "auth". Empty / omitted returns core only.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Optional list of individual section names to include (case-insensitive).
    /// Valid core names: model, store, client_store, resource, components,
    /// component, screen, server_fn, modify, remove. Valid extension names:
    /// form, list, table (crud), signal, socket, feed (realtime),
    /// session_state, login_screen, protected_route (auth). When non-empty,
    /// only the listed sections are emitted; extension blocks are
    /// auto-included as needed. Use this to fetch a slim subset (e.g. just
    /// `model` + `client_store`) instead of the full payload. The
    /// `components` section is an informational catalog of the 45 official
    /// Dioxus components installable via `dx components add` — pull it
    /// before scaffolding UI primitives like buttons, dialogs, or calendars.
    #[serde(default)]
    pub sections: Vec<String>,
    /// When true, return a compact index (primitive name + one-line summary)
    /// instead of full spec blocks. Useful for deciding which `sections:` to
    /// pull next without paying for the full ~10KB payload. `extensions:`
    /// still controls which extension groups appear; `sections:` is ignored
    /// in this mode (the index is always the full set within the requested
    /// extension scope).
    #[serde(default)]
    pub index_only: bool,
    /// When false, omit the ~5KB authoring-guide preamble (workflow notes,
    /// envelope conventions, etc.). When omitted, the server picks a
    /// default: `true` on the first `get_dsl_spec` call of the session,
    /// `false` on follow-ups — the prologue is most useful exactly once,
    /// and re-shipping it on every refresh wastes agent context. Pass
    /// `true` explicitly to force the full payload.
    #[serde(default)]
    pub include_prologue: Option<bool>,
    /// When false, strip the per-primitive `example:` block from each section
    /// body. The field schema (`fields:`, `kinds:`, etc.) is still emitted.
    /// Useful when the caller only needs to know what fields a primitive
    /// accepts. Defaults to true.
    #[serde(default = "default_true")]
    pub include_examples: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub struct GetDslSpecResult {
    pub spec: String,
}

pub async fn get_dsl_spec(
    state: &Arc<State>,
    p: GetDslSpecParams,
) -> Result<GetDslSpecResult, String> {
    // Auto-pace the prologue: emit it on the first call of the session and
    // skip it on follow-ups, unless the caller pinned the choice explicitly.
    let include_prologue = match p.include_prologue {
        Some(v) => v,
        None => !state
            .dsl_spec_prologue_seen
            .load(std::sync::atomic::Ordering::Relaxed),
    };
    // Canonical (snake_case) name → (group, body). The group decides whether
    // a section is emitted under `core:` or under an `extensions: <group>:`
    // block; the body is the constant text already authored above.
    const SECTIONS: &[(&str, &str, &str)] = &[
        ("model", "core", CORE_MODEL),
        ("store", "core", CORE_STORE),
        ("client_store", "core", CORE_CLIENT_STORE),
        ("resource", "core", CORE_RESOURCE),
        ("components", "core", CORE_COMPONENTS),
        ("component", "core", CORE_COMPONENT),
        ("screen", "core", CORE_SCREEN),
        ("server_fn", "core", CORE_SERVER_FN),
        ("modify", "core", CORE_MODIFY),
        ("remove", "core", CORE_REMOVE),
        ("form", "crud", CRUD_FORM),
        ("list", "crud", CRUD_LIST),
        ("table", "crud", CRUD_TABLE),
        ("signal", "realtime", REALTIME_SIGNAL),
        ("socket", "realtime", REALTIME_SOCKET),
        ("feed", "realtime", REALTIME_FEED),
        ("session_state", "auth", AUTH_SESSION),
        ("login_screen", "auth", AUTH_LOGIN),
        ("protected_route", "auth", AUTH_PROTECTED),
    ];

    // Validate `extensions:` first so the error message is the same regardless
    // of whether `sections:` is also set.
    for e in &p.extensions {
        let lc = e.to_ascii_lowercase();
        if !matches!(lc.as_str(), "crud" | "realtime" | "auth") {
            return Err(format!(
                "unknown extension {e:?}; valid: crud, realtime, auth"
            ));
        }
    }

    // Resolve `sections:` to canonical names. Empty => no filter.
    let section_filter: Option<BTreeSet<String>> = if p.sections.is_empty() {
        None
    } else {
        let known: BTreeSet<&str> = SECTIONS.iter().map(|(n, _, _)| *n).collect();
        let mut set = BTreeSet::new();
        for s in &p.sections {
            let lc = s.to_ascii_lowercase();
            if !known.contains(lc.as_str()) {
                let mut valid: Vec<&str> = SECTIONS.iter().map(|(n, _, _)| *n).collect();
                valid.sort();
                return Err(format!(
                    "unknown section {s:?}; valid: {}",
                    valid.join(", ")
                ));
            }
            set.insert(lc);
        }
        Some(set)
    };

    let want_extension = |k: &str| p.extensions.iter().any(|e| e.eq_ignore_ascii_case(k));

    // A section is included when (a) no filter is active and its group is
    // either "core" or a requested extension, or (b) a filter is active and
    // names the section. Filters auto-pull in their parent extension block.
    let include = |name: &str, group: &str| -> bool {
        match &section_filter {
            None => match group {
                "core" => true,
                ext => want_extension(ext),
            },
            Some(set) => set.contains(name),
        }
    };

    // index_only mode: emit a compact name + one-line summary per primitive,
    // in the same core/extensions shape, without the spec blocks themselves.
    // `sections:` is ignored — the index always covers everything within the
    // requested extension scope so callers can scan it and decide what to
    // pull next.
    if p.index_only {
        let mut out = String::new();
        out.push_str("# Dioxus-MCP DSL spec — compact index\n");
        out.push_str(
            "# One line per primitive. Re-call get_dsl_spec with `sections: [name, ...]`\n",
        );
        out.push_str("# (and optionally `extensions: [...]`) to fetch the full block(s).\n");
        out.push_str(&format!("\nversion: \"{SPEC_VERSION}\"\n"));
        let any_core = SECTIONS.iter().any(|(_, g, _)| *g == "core");
        if any_core {
            out.push_str("\ncore:\n");
            for (name, group, body) in SECTIONS.iter().filter(|(_, g, _)| *g == "core") {
                let _ = group;
                let _ = name;
                let (key, summary) = spec_index_line(body);
                out.push_str(&format!("  {key}: {summary}\n"));
            }
        }
        let ext_groups = ["crud", "realtime", "auth"];
        let any_ext = ext_groups.iter().any(|g| want_extension(g));
        if any_ext {
            out.push_str("\nextensions:\n");
            for g in ext_groups {
                if !want_extension(g) {
                    continue;
                }
                out.push_str(&format!(" {g}:\n"));
                for (_, _, body) in SECTIONS.iter().filter(|(_, sg, _)| sg == &g) {
                    let (key, summary) = spec_index_line(body);
                    out.push_str(&format!("  {key}: {summary}\n"));
                }
            }
        }
        return Ok(GetDslSpecResult { spec: out });
    }

    let render = |body: &str| -> String {
        if p.include_examples {
            body.to_string()
        } else {
            strip_examples(body)
        }
    };

    let mut out = String::new();
    if include_prologue {
        out.push_str(CORE_PREAMBLE);
        // Mark the session as having seen the prologue so the next call
        // (without an explicit override) skips it. Relaxed ordering is
        // fine: the only consumer is the read at the top of this fn,
        // and we don't care about cross-thread ordering against other
        // tool calls — a brief race that re-emits the prologue once is
        // strictly less bad than a missed update.
        state
            .dsl_spec_prologue_seen
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    out.push_str(&format!("\nversion: \"{SPEC_VERSION}\"\n"));

    let any_core = SECTIONS
        .iter()
        .any(|(n, g, _)| *g == "core" && include(n, g));
    if any_core {
        out.push_str("\ncore:\n");
        for (name, group, body) in SECTIONS.iter().filter(|(_, g, _)| *g == "core") {
            if include(name, group) {
                out.push_str(&render(body));
            }
        }
    }

    let ext_groups = ["crud", "realtime", "auth"];
    let any_ext = ext_groups
        .iter()
        .any(|g| SECTIONS.iter().any(|(n, sg, _)| sg == g && include(n, sg)));
    if any_ext {
        out.push_str("\nextensions:\n");
        for g in ext_groups {
            let group_active = SECTIONS.iter().any(|(n, sg, _)| sg == &g && include(n, sg));
            if !group_active {
                continue;
            }
            out.push_str(&format!(" {g}:\n"));
            for (name, group, body) in SECTIONS.iter().filter(|(_, sg, _)| sg == &g) {
                if include(name, group) {
                    out.push_str(&indent(&render(body), " "));
                }
            }
        }
    }

    Ok(GetDslSpecResult { spec: out })
}

/// Strip the `example:` block from a spec section body. Each block is the
/// constant text in the `CORE_*` / `CRUD_*` / etc. statics, where the example
/// is a 4-space-indented `example:` line followed by 6+-space-indented content
/// (the example YAML) until the next 4-space sibling key or end-of-block.
fn strip_examples(block: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in block.lines() {
        if skipping {
            let leading = line.chars().take_while(|c| *c == ' ').count();
            if line.is_empty() || leading > 4 {
                continue;
            }
            skipping = false;
        }
        if line.starts_with("    example:") {
            skipping = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Pull the primitive name (first non-empty line, stripped of indentation and
/// trailing colon) and the first sentence of its `description:` field from a
/// spec block. Used by `index_only` mode to emit a compact line per primitive.
fn spec_index_line(block: &str) -> (String, String) {
    let mut key = String::new();
    let mut summary = String::new();
    let mut in_desc = false;
    let mut desc_buf = String::new();
    for line in block.lines() {
        let trimmed = line.trim();
        if key.is_empty()
            && !trimmed.is_empty()
            && let Some(stripped) = trimmed.strip_suffix(':')
        {
            key = stripped.to_string();
            continue;
        }
        if !in_desc && let Some(rest) = trimmed.strip_prefix("description:") {
            in_desc = true;
            desc_buf.push_str(rest.trim());
            continue;
        }
        if in_desc {
            // Stop at the next top-level key (a line starting with "fields:",
            // "kinds:", "example:", "field_types:", "template_kinds:", etc.).
            if trimmed.ends_with(':') && !trimmed.contains(' ') {
                break;
            }
            // Multi-line descriptions continue as indented text — append.
            if line.starts_with("    ") || line.starts_with("\t") {
                if !desc_buf.is_empty() && !desc_buf.ends_with(' ') {
                    desc_buf.push(' ');
                }
                desc_buf.push_str(trimmed);
            } else {
                break;
            }
        }
    }
    // Strip surrounding quotes and take the first sentence (up to the first
    // ". " or end of buffer).
    let cleaned = desc_buf.trim().trim_matches('"').to_string();
    let first_sentence = match cleaned.find(". ") {
        Some(i) => &cleaned[..i + 1],
        None => cleaned.as_str(),
    };
    summary.push_str(first_sentence);
    (key, summary)
}

fn indent(block: &str, prefix: &str) -> String {
    block
        .lines()
        .map(|l| {
            if l.is_empty() {
                String::from("\n")
            } else {
                format!("{prefix}{l}\n")
            }
        })
        .collect()
}
