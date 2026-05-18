//! `magic_id_prefix_for_optimistic`: hint when a model's `id` field encodes
//! "this row is a client-side optimistic placeholder" via a literal string
//! prefix (`"tmp-…"`, `"pending-…"`, `"local-…"`, …).
//!
//! Pattern: a generator hands the optimistic-render path a magic prefix
//! instead of a sidecar `pending: bool` field on the model or a parallel
//! `pending: HashSet<Id>` signal. iter03's `card.id.starts_with("tmp-")`
//! at `board_screen.rs:311` is the canonical case, paired with
//! `format!("tmp-{}", js_random_id())` in the optimistic create path.
//!
//! Detection: two complementary AST shapes, both string-literal driven so
//! we don't false-positive on real ID conventions:
//!
//!   1. Read site: `<expr>.id.starts_with("tmp-")` (or other known
//!      placeholder prefixes) — the consumer is branching on the magic
//!      prefix.
//!   2. Write site: `format!("tmp-{…}", …)` (or the same prefixes) — the
//!      optimistic-create path is forging an ID.
//!
//! Severity `info`, confidence `low` — the magic-prefix pattern works,
//! it's just brittle: a real ID that happens to start with `tmp-` would
//! be silently mis-classified. The fix is a typed flag on the model.

use std::path::PathBuf;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::spanned::Spanned;
use syn::visit::Visit;

use crate::state::State;
use crate::tools::ast::{ParseError, collect_parse_errors, walk_rs_files};
use crate::tools::scaffold::crate_root;

/// String prefixes the lint recognises as "client-side optimistic
/// placeholder" markers. Kept narrow so a real prefix like `"draft-"`
/// doesn't accidentally light up — `tmp` / `temp` / `pending` / `local`
/// are the recurring shapes generators use.
const PLACEHOLDER_PREFIXES: &[&str] = &["tmp-", "tmp_", "temp-", "temp_", "pending-", "local-"];

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct MagicIdPrefixParams {
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MagicIdFinding {
    pub code: &'static str,
    pub severity: &'static str,
    pub confidence: &'static str,
    pub file: PathBuf,
    pub line: usize,
    /// Kind of site: `read` (`.id.starts_with(…)`) or `write`
    /// (`format!("tmp-{…}", …)`).
    pub kind: &'static str,
    pub placeholder: String,
    pub message: String,
    pub fix: String,
}

#[derive(Debug, Serialize)]
pub struct MagicIdPrefixReport {
    pub findings: Vec<MagicIdFinding>,
    pub parse_errors: Vec<ParseError>,
}

pub async fn magic_id_prefix_for_optimistic(
    state: &Arc<State>,
    p: MagicIdPrefixParams,
) -> Result<MagicIdPrefixReport, String> {
    let root = crate_root(state, p.project_root.as_deref()).await?;
    let src_root = root.join("src");
    let files = walk_rs_files(&src_root);

    let mut findings: Vec<MagicIdFinding> = Vec::new();
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        let mut v = MagicIdVisitor {
            file: sf.path.clone(),
            findings: &mut findings,
        };
        v.visit_file(ast);
    }

    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.line.cmp(&b.line))
            .then(a.kind.cmp(b.kind))
    });

    Ok(MagicIdPrefixReport {
        findings,
        parse_errors: collect_parse_errors(&files),
    })
}

struct MagicIdVisitor<'a> {
    file: PathBuf,
    findings: &'a mut Vec<MagicIdFinding>,
}

impl<'a, 'ast> Visit<'ast> for MagicIdVisitor<'a> {
    fn visit_expr_method_call(&mut self, e: &'ast syn::ExprMethodCall) {
        if e.method == "starts_with"
            && let Some(prefix) = first_str_lit(&e.args)
            && matches_placeholder(&prefix)
            && receiver_ends_in_id(&e.receiver)
        {
            let line = e.method.span().start().line;
            self.findings.push(MagicIdFinding {
                code: "magic_id_prefix_for_optimistic",
                // READ side is the consumer that mis-classifies any real
                // id with the magic prefix — that's a correctness bug,
                // not a smell. iter03 follow-up bumped this from `info`
                // to `warning`; the write side stays `info` because it's
                // generally a stylistic preference (forging vs. typed
                // marker) rather than a wrong-behavior risk.
                severity: "warning",
                confidence: "low",
                file: self.file.clone(),
                line,
                kind: "read",
                placeholder: prefix.clone(),
                message: format!(
                    "`.id.starts_with({prefix:?})` branches on a magic prefix to detect a \
                     client-side optimistic placeholder. A real id that happens to start \
                     with {prefix:?} is silently mis-classified — that's a correctness bug, \
                     not just a smell.",
                ),
                fix: "Add a typed marker: `pending: bool` on the model (set true on \
                      optimistic insert, false / removed on server confirm), or maintain \
                      a sidecar signal `pending: HashSet<Id>` so the consumer reads \
                      `pending.contains(&card.id)` instead of a string match."
                    .to_string(),
            });
        }
        syn::visit::visit_expr_method_call(self, e);
    }

    fn visit_expr_macro(&mut self, e: &'ast syn::ExprMacro) {
        let last = e.mac.path.segments.last().map(|s| s.ident.to_string());
        if matches!(last.as_deref(), Some("format" | "format_args"))
            && let Some(prefix) = first_str_lit_in_macro(&e.mac)
            && let Some(p) = placeholder_at_start(&prefix)
        {
            let line = e.mac.span().start().line;
            self.findings.push(MagicIdFinding {
                code: "magic_id_prefix_for_optimistic",
                severity: "info",
                confidence: "low",
                file: self.file.clone(),
                line,
                kind: "write",
                placeholder: p.to_string(),
                message: format!(
                    "`format!({prefix:?}, …)` forges an ID with a magic prefix to mark an \
                     optimistic placeholder. The downstream consumer ends up branching on \
                     the prefix string.",
                ),
                fix: "Construct the row with `id` empty (or a stable client UUID) and \
                      a separate `pending: true` field; or hold optimistic rows in a \
                      sidecar signal keyed by client UUID and merge them with the \
                      server-confirmed list at render time."
                    .to_string(),
            });
        }
        syn::visit::visit_expr_macro(self, e);
    }
}

/// True iff the expression resolves to an `.id` field access (any depth of
/// receiver). Restricts the read-site detection to the common shape and
/// avoids false-positives on unrelated `.starts_with(...)` calls.
fn receiver_ends_in_id(e: &syn::Expr) -> bool {
    match e {
        syn::Expr::Field(f) => match &f.member {
            syn::Member::Named(ident) => ident == "id",
            _ => false,
        },
        syn::Expr::Reference(r) => receiver_ends_in_id(&r.expr),
        syn::Expr::Paren(p) => receiver_ends_in_id(&p.expr),
        // `card.id.as_str()` chain: receiver is the `.as_str()` call,
        // walk further down looking for `.id`.
        syn::Expr::MethodCall(m) => receiver_ends_in_id(&m.receiver),
        _ => false,
    }
}

fn first_str_lit(args: &syn::punctuated::Punctuated<syn::Expr, syn::Token![,]>) -> Option<String> {
    let first = args.first()?;
    if let syn::Expr::Lit(l) = first
        && let syn::Lit::Str(s) = &l.lit
    {
        return Some(s.value());
    }
    None
}

/// `format!("tmp-{}", x)` — pull out the literal format string from a
/// macro token stream by scanning for the first `Literal` token whose
/// `to_string()` parses as a string literal.
fn first_str_lit_in_macro(m: &syn::Macro) -> Option<String> {
    let tokens: Vec<proc_macro2::TokenTree> = m.tokens.clone().into_iter().collect();
    for tt in tokens {
        if let proc_macro2::TokenTree::Literal(lit) = tt {
            let s = lit.to_string();
            if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
                return Some(s[1..s.len() - 1].to_string());
            }
        }
    }
    None
}

fn matches_placeholder(s: &str) -> bool {
    PLACEHOLDER_PREFIXES.iter().any(|p| *p == s)
}

/// Return the placeholder prefix iff `s` starts with one of the
/// recognised placeholders. We want strict matches at the START so a
/// random message containing `"tmp-"` mid-string doesn't false-positive.
fn placeholder_at_start(s: &str) -> Option<&'static str> {
    for p in PLACEHOLDER_PREFIXES {
        if s.starts_with(p) {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn run(src: &str) -> MagicIdPrefixReport {
        let dir = TempDir::new().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("a.rs"), src).unwrap();
        let scanned = walk_rs_files(&src_dir);
        let mut findings: Vec<MagicIdFinding> = Vec::new();
        for sf in &scanned {
            let Ok(ast) = &sf.ast else { continue };
            let mut v = MagicIdVisitor {
                file: sf.path.clone(),
                findings: &mut findings,
            };
            v.visit_file(ast);
        }
        MagicIdPrefixReport {
            findings,
            parse_errors: Vec::new(),
        }
    }

    /// iter03's read-site shape: `card.id.starts_with("tmp-")`. Must
    /// fire at `warning` severity — a real id starting with `tmp-`
    /// would be silently mis-classified, which is a correctness bug.
    #[test]
    fn flags_starts_with_on_id_field() {
        let r = run(r#"fn is_pending(card: &Card) -> bool {
    card.id.starts_with("tmp-")
}
"#);
        let read_hits: Vec<&MagicIdFinding> =
            r.findings.iter().filter(|f| f.kind == "read").collect();
        assert_eq!(read_hits.len(), 1, "expected one read hit: {r:?}");
        assert_eq!(read_hits[0].placeholder, "tmp-");
        assert_eq!(
            read_hits[0].severity, "warning",
            "read side is a correctness bug, not info-level smell",
        );
    }

    /// iter03's write-site shape: `format!("tmp-{}", id)`. Must fire at
    /// `info` severity — the write side is a stylistic preference, not
    /// a wrong-behavior risk.
    #[test]
    fn flags_format_macro_with_placeholder_prefix() {
        let r = run(r#"fn forge(id: &str) -> String {
    format!("tmp-{}", id)
}
"#);
        let write_hits: Vec<&MagicIdFinding> =
            r.findings.iter().filter(|f| f.kind == "write").collect();
        assert_eq!(write_hits.len(), 1, "expected one write hit: {r:?}");
        assert_eq!(write_hits[0].placeholder, "tmp-");
        assert_eq!(
            write_hits[0].severity, "info",
            "write side stays info — stylistic, not correctness",
        );
    }

    /// `.starts_with("foo-")` on a non-`id` field must NOT fire. The
    /// magic-prefix smell is specifically about IDs.
    #[test]
    fn silent_when_receiver_is_not_id() {
        let r = run(r#"fn ok(s: &str) -> bool {
    s.starts_with("tmp-")
}
"#);
        assert!(r.findings.is_empty(), "non-id receiver: {r:?}");
    }

    /// `.starts_with("anything-")` with a non-placeholder prefix must
    /// stay silent — only the recurring magic markers count.
    #[test]
    fn silent_when_prefix_is_not_a_placeholder() {
        let r = run(r#"fn ok(card: &Card) -> bool {
    card.id.starts_with("user-")
}
"#);
        assert!(r.findings.is_empty(), "non-placeholder prefix: {r:?}");
    }

    /// Multiple recognised prefixes (`pending-`, `local-`) should all be
    /// picked up. Pin the prefix list against accidental removal.
    #[test]
    fn flags_pending_and_local_prefixes_too() {
        let r = run(r#"fn a(c: &Card) -> bool { c.id.starts_with("pending-") }
fn b(c: &Card) -> bool { c.id.starts_with("local-") }
"#);
        let placeholders: Vec<&str> = r.findings.iter().map(|f| f.placeholder.as_str()).collect();
        assert!(placeholders.contains(&"pending-"));
        assert!(placeholders.contains(&"local-"));
    }
}
