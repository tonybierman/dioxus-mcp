//! `insecure_set_cookie`: flag `Set-Cookie` header values built inside
//! server fns that lack the `Secure` attribute (and call out the
//! `SameSite=None` + missing `Secure` case as a hard error since browsers
//! reject it outright).
//!
//! Heuristic: scan every string-literal expression inside a server fn body
//! that looks like a cookie value — semicolon-delimited and containing at
//! least one cookie attribute (`HttpOnly`, `SameSite=`, `Path=`,
//! `Max-Age=`, `Domain=`, `Expires=`). For each such literal:
//!   - `SameSite=None` without `Secure` → severity `error`.
//!   - Anything else without `Secure` → severity `warning`.
//!   - With `Secure` → no finding.
//!
//! False positives are possible (a string that mentions HttpOnly for
//! diagnostic reasons but isn't actually a cookie value), so the message
//! always quotes the offending literal and the file:line where it lives.

use std::path::PathBuf;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::state::State;
use crate::tools::ast::{ParseError, collect_parse_errors, walk_rs_files};
use crate::tools::scaffold::crate_root;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct InsecureSetCookieParams {
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct InsecureCookieFinding {
    pub code: &'static str,
    pub severity: &'static str,
    pub file: PathBuf,
    pub line: usize,
    pub server_fn: String,
    /// The offending string literal — quoted as it appears in source.
    pub literal: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct InsecureSetCookieReport {
    pub findings: Vec<InsecureCookieFinding>,
    pub parse_errors: Vec<ParseError>,
}

pub async fn insecure_set_cookie(
    state: &Arc<State>,
    p: InsecureSetCookieParams,
) -> Result<InsecureSetCookieReport, String> {
    let root = crate_root(state, p.project_root.as_deref()).await?;
    let src_root = root.join("src");
    let files = walk_rs_files(&src_root);

    let mut findings: Vec<InsecureCookieFinding> = Vec::new();
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        for item in &ast.items {
            let syn::Item::Fn(f) = item else { continue };
            if !is_server_fn(f) {
                continue;
            }
            let server_fn_name = f.sig.ident.to_string();
            let mut v = CookieLiteralVisitor { hits: Vec::new() };
            v.visit_block(&f.block);
            for hit in v.hits {
                let Some((severity, code)) = classify(&hit.value) else {
                    continue;
                };
                findings.push(InsecureCookieFinding {
                    code,
                    severity,
                    file: sf.path.clone(),
                    line: hit.line,
                    server_fn: server_fn_name.clone(),
                    literal: hit.value.clone(),
                    message: build_message(code, severity, &hit.value),
                });
            }
        }
    }

    Ok(InsecureSetCookieReport {
        findings,
        parse_errors: collect_parse_errors(&files),
    })
}

/// Classify a cookie-value string. Returns the `(severity, code)` pair
/// to emit, or `None` when the literal isn't a cookie at all. Codes:
///
///   * `insecure_set_cookie` — the existing finding. `error` when
///     `SameSite=None` + missing `Secure` (browsers reject), `warning`
///     when `Secure` is missing for any other shape.
///   * `samesite_lax_session_hint` — `info` when the cookie has `Secure`
///     and `SameSite=Lax` (or `SameSite` unset, which defaults to Lax in
///     modern browsers). Lax is acceptable for most session flows but
///     `Strict` is tighter — surface so reviewers can decide.
fn classify(literal: &str) -> Option<(&'static str, &'static str)> {
    let attrs = parse_attrs(literal);
    if attrs.is_empty() {
        return None;
    }
    let has_secure = attrs.iter().any(|a| a.eq_ignore_ascii_case("Secure"));
    let lower_attrs: Vec<String> = attrs.iter().map(|a| a.to_ascii_lowercase()).collect();
    let samesite_value = lower_attrs.iter().find_map(|a| {
        let stripped = a.strip_prefix("samesite=")?;
        Some(stripped.trim().to_string())
    });
    let same_site_none = matches!(samesite_value.as_deref(), Some("none"));
    let same_site_lax = matches!(samesite_value.as_deref(), Some("lax"));
    let same_site_unset = samesite_value.is_none();
    let same_site_strict = matches!(samesite_value.as_deref(), Some("strict"));
    if !has_secure {
        if same_site_none {
            return Some(("error", "insecure_set_cookie"));
        }
        return Some(("warning", "insecure_set_cookie"));
    }
    // Secure is present. Hint on Lax / unset session cookies that could
    // be Strict. Skip Strict (already gold) and skip None (handled
    // above; with Secure set it's actually fine for cross-site).
    let _ = same_site_strict;
    if (same_site_lax || same_site_unset) && is_session_cookie(literal) {
        return Some(("info", "samesite_lax_session_hint"));
    }
    None
}

/// Treat the literal as a session cookie when its `name=` prefix or path
/// hints at a session — `sid`, `session`, `auth`, `token`, or the
/// `__Host-`/`__Secure-` prefix. We're deliberately narrow: the Lax /
/// Strict trade-off is only interesting on cookies that gate access. A
/// preference cookie like `theme=dark` is fine on Lax.
fn is_session_cookie(literal: &str) -> bool {
    let head = literal.split(';').next().unwrap_or("").trim();
    let lower = head.to_ascii_lowercase();
    let name = lower.split('=').next().unwrap_or("");
    let stripped = name
        .strip_prefix("__host-")
        .or_else(|| name.strip_prefix("__secure-"))
        .unwrap_or(name);
    matches!(
        stripped,
        "sid"
            | "session"
            | "session_id"
            | "sessionid"
            | "auth"
            | "auth_token"
            | "token"
            | "access_token"
            | "csrf"
            | "csrftoken"
    ) || stripped.contains("session")
        || stripped.contains("auth")
        || stripped.contains("token")
}

fn parse_attrs(literal: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in literal.split(';') {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        // First segment is `name=value`. We only care about the cookie
        // attributes that come after — but accept all segments for the
        // detector so a literal like `name=v; Secure` still works.
        // Anything containing `=` we keep verbatim; bare flags (`Secure`,
        // `HttpOnly`) likewise.
        if !is_likely_attribute_or_pair(trimmed) {
            continue;
        }
        out.push(trimmed.to_string());
    }
    // A real cookie has at least one *attribute* — a `name=value` alone is
    // ambiguous (could be any URL-encoded form). Require at least one
    // recognised attribute keyword.
    if !out.iter().any(|a| is_known_cookie_attr(a)) {
        return Vec::new();
    }
    out
}

fn is_likely_attribute_or_pair(s: &str) -> bool {
    // Reject characters that wouldn't appear in a Set-Cookie value (newline,
    // whitespace beyond `=` interior, quotes, etc). We're conservative —
    // anything weird drops the literal out of consideration so false
    // positives stay rare.
    !s.contains('\n') && !s.contains('\r') && !s.is_empty()
}

fn is_known_cookie_attr(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    matches!(lower.as_str(), "secure" | "httponly" | "partitioned")
        || lower.starts_with("samesite=")
        || lower.starts_with("path=")
        || lower.starts_with("max-age=")
        || lower.starts_with("expires=")
        || lower.starts_with("domain=")
}

fn build_message(code: &str, severity: &str, literal: &str) -> String {
    let escaped = literal.replace('\n', "\\n").replace('\r', "\\r");
    if code == "samesite_lax_session_hint" {
        return format!(
            "Set-Cookie value `{escaped}` ships a session cookie with `SameSite=Lax` \
             (or default, which is Lax in modern browsers). Lax is fine for most \
             session flows — top-level GET navigations still carry the cookie, so \
             standard logged-in browsing works. `Strict` is tighter: the cookie is \
             never sent on any cross-site request, including the user clicking a \
             link from another site. Use `Strict` for cookies that gate \
             state-changing actions and don't need to survive cross-site navigation \
             (admin sessions, payment checkout). Stay with `Lax` if users routinely \
             land on your site from external links and need to be logged in."
        );
    }
    if severity == "error" {
        format!(
            "Set-Cookie value `{escaped}` declares `SameSite=None` but lacks `Secure`. \
             Modern browsers reject this combination outright — the cookie is dropped. \
             Add `; Secure` (and serve over HTTPS) or change `SameSite=None` to `Lax` \
             / `Strict` if cross-site delivery isn't required."
        )
    } else {
        format!(
            "Set-Cookie value `{escaped}` is missing the `Secure` attribute. \
             Without `Secure` the browser will send the cookie over plain HTTP, \
             so a network observer can lift the session. For session cookies, \
             prefer the `__Host-` prefix (forces Secure + Path=/ + no Domain). \
             Also consider tightening the fixed `Max-Age` if it's a long lifetime."
        )
    }
}

fn is_server_fn(f: &syn::ItemFn) -> bool {
    f.attrs.iter().any(|a| {
        let last = a.path().segments.last().map(|s| s.ident.to_string());
        matches!(
            last.as_deref(),
            Some("server" | "get" | "post" | "put" | "delete" | "patch")
        )
    })
}

struct LiteralHit {
    value: String,
    line: usize,
}

struct CookieLiteralVisitor {
    hits: Vec<LiteralHit>,
}

impl<'ast> Visit<'ast> for CookieLiteralVisitor {
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        let name = m
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default();
        // `format!("...", ...)` is the canonical way these strings get
        // built. We accept any macro and pluck the first string literal
        // from its token stream — works for `format!`, `write!`, `print!`,
        // etc. without baking in the macro name.
        let _ = name;
        for tt in m.tokens.clone() {
            if let proc_macro2::TokenTree::Literal(lit) = tt {
                let s = lit.to_string();
                if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
                    let value = unquote(&s);
                    self.hits.push(LiteralHit {
                        value,
                        line: lit.span().start().line,
                    });
                }
                break; // only the first literal — the format string
            }
        }
        syn::visit::visit_macro(self, m);
    }

    fn visit_expr_lit(&mut self, el: &'ast syn::ExprLit) {
        // Bare string literals — `HeaderValue::from_static("…")` and
        // friends land here.
        if let syn::Lit::Str(s) = &el.lit {
            self.hits.push(LiteralHit {
                value: s.value(),
                line: s.span().start().line,
            });
        }
        syn::visit::visit_expr_lit(self, el);
    }
}

fn unquote(s: &str) -> String {
    // proc_macro2 stringifies a string literal with its surrounding quotes
    // and Rust escapes — turn it back into the runtime value via
    // `syn::Lit::Str`. Falls back to trimming quotes if parsing fails.
    if let Ok(parsed) = syn::parse_str::<syn::LitStr>(s) {
        parsed.value()
    } else {
        s.trim_matches('"').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_missing_secure_is_warning() {
        let result = classify("sid=abc; Path=/; HttpOnly; SameSite=Lax; Max-Age=86400");
        assert_eq!(result, Some(("warning", "insecure_set_cookie")));
    }

    #[test]
    fn classify_samesite_none_without_secure_is_error() {
        let result = classify("sid=abc; Path=/; SameSite=None");
        assert_eq!(result, Some(("error", "insecure_set_cookie")));
    }

    /// Session cookie with `Secure` + `SameSite=Strict` is the gold
    /// standard — no finding.
    #[test]
    fn classify_with_secure_strict_is_clean() {
        let result = classify("sid=abc; Path=/; HttpOnly; SameSite=Strict; Secure; Max-Age=86400");
        assert_eq!(result, None);
    }

    /// iter03 shape: session cookie with `Secure` + `SameSite=Lax`.
    /// Modern lint should emit the info-level `samesite_lax_session_hint`
    /// so the reviewer can decide Lax vs Strict.
    #[test]
    fn classify_secure_lax_session_emits_info_hint() {
        let result = classify("sid=abc; Path=/; HttpOnly; SameSite=Lax; Secure; Max-Age=86400");
        assert_eq!(result, Some(("info", "samesite_lax_session_hint")));
    }

    /// `Secure` + `SameSite=Lax` on a non-session cookie (e.g. `theme=dark`)
    /// stays silent — the Lax/Strict trade-off only matters for cookies
    /// that gate access.
    #[test]
    fn classify_secure_lax_non_session_is_clean() {
        let result = classify("theme=dark; Path=/; SameSite=Lax; Secure; Max-Age=86400");
        assert_eq!(result, None);
    }

    /// iter03 logout: empty session cookie with `Secure` + Lax. Still a
    /// session-name cookie — surface the hint.
    #[test]
    fn classify_session_cookie_with_default_samesite_emits_hint() {
        // No explicit SameSite — modern browsers default to Lax. Treat
        // the same as explicit Lax for session cookies.
        let result = classify("sid=abc; Path=/; HttpOnly; Secure; Max-Age=86400");
        assert_eq!(result, Some(("info", "samesite_lax_session_hint")));
    }

    #[test]
    fn classify_non_cookie_string_is_ignored() {
        // Plain message strings should not match — no cookie attrs.
        assert_eq!(classify("login failed"), None);
        // Even with a semicolon — if no recognised attribute appears, skip.
        assert_eq!(classify("hello; world"), None);
    }

    #[test]
    fn classify_secure_case_insensitive() {
        // RFC says attributes are case-insensitive; respect that.
        // A non-session cookie with secure + no SameSite is clean.
        let result = classify("pref=x; Path=/; HttpOnly; secure");
        assert_eq!(result, None, "lowercase secure should still count");
    }

    #[test]
    fn classify_samesite_none_case_insensitive() {
        // `samesite=none` lowercase also rejected when Secure missing.
        let result = classify("sid=x; samesite=none");
        assert_eq!(result, Some(("error", "insecure_set_cookie")));
    }
}
