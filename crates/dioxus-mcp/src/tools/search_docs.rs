use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::state::{CachedDoc, State};

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SearchDocsParams {
    pub query: String,
    /// Major.minor version, e.g. "0.7". Defaults to the project's detected version.
    pub version: Option<String>,
    /// Max results (default 5).
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct DocHit {
    /// Human-friendly URL with the section anchor — opens in a browser,
    /// but 404s via WebFetch because dioxuslabs.com serves SPA HTML, not
    /// the anchored fragment. Use `raw_url` for programmatic fetches.
    pub url: String,
    /// WebFetch-safe URL pointing at the canonical llms-full.txt dump the
    /// search index was built from. Fetching it returns the entire corpus —
    /// the agent can scan for the section heading to recover full prose
    /// when `body` was truncated.
    pub raw_url: String,
    pub title: Option<String>,
    pub score: f32,
    /// Best 240-char excerpt around the matched query term — for quick triage.
    pub snippet: String,
    /// Full section text (capped at 4000 chars) so the agent doesn't have
    /// to re-fetch the corpus for typical lookups. Truncation is signaled by
    /// a trailing `... [truncated]` marker.
    pub body: String,
    /// For MCP-curated supplemental snippets, the Dioxus version the snippet
    /// was last verified against. Absent for upstream corpus sections.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_verified: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchDocsResult {
    pub query: String,
    pub version: String,
    pub hits: Vec<DocHit>,
}

pub async fn search_docs(
    state: &Arc<State>,
    p: SearchDocsParams,
) -> Result<SearchDocsResult, String> {
    let version = match p.version.clone() {
        Some(v) => v,
        None => detect_version(state).await,
    };
    let limit = p.limit.unwrap_or(5);

    let qterms = tokenize(&p.query);
    if qterms.is_empty() {
        return Err("query is empty".into());
    }

    let corpus = fetch_llms_full(state, &version).await?;
    let sections = split_sections(&corpus.body);

    let mut hits: Vec<DocHit> = Vec::with_capacity(sections.len());
    for sec in &sections {
        let head_terms = tokenize(&sec.heading);
        let body_terms = tokenize(&sec.body);
        let score = score_terms(&qterms, &head_terms) * 3.0 + score_terms(&qterms, &body_terms);
        if score <= 0.0 {
            continue;
        }
        let snippet = best_snippet(&sec.body, &qterms);
        hits.push(DocHit {
            url: section_url(&version, &sec.heading),
            raw_url: raw_corpus_url(&version),
            title: Some(sec.heading.clone()),
            score,
            snippet,
            body: section_body_capped(&sec.body),
            version_verified: None,
        });
    }

    // Bolt in MCP-curated snippets that fill known gaps in the upstream
    // corpus (e.g. cookie writes via FullstackContext). Scored against the
    // same query, but with a small boost since these are deliberately
    // hand-picked to answer specific questions.
    for sup in supplemental_sections() {
        let head_terms = tokenize(sup.heading);
        let body_terms = tokenize(sup.body);
        let score = score_terms(&qterms, &head_terms) * 4.0 + score_terms(&qterms, &body_terms);
        if score <= 0.0 {
            continue;
        }
        let snippet = best_snippet(sup.body, &qterms);
        hits.push(DocHit {
            url: format!("dioxus-mcp://docs/{}", slugify(sup.heading)),
            raw_url: format!("dioxus-mcp://docs/{}", slugify(sup.heading)),
            title: Some(format!("{} (mcp curated)", sup.heading)),
            score,
            snippet,
            body: section_body_capped(sup.body),
            version_verified: Some(sup.version_verified.to_string()),
        });
    }

    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(limit);

    Ok(SearchDocsResult {
        query: p.query,
        version,
        hits,
    })
}

struct Section {
    heading: String,
    body: String,
}

/// MCP-curated documentation snippets surfaced alongside the upstream
/// corpus. Each entry should have a heading that contains the keywords an
/// agent would type, and a body with a runnable Rust snippet.
struct SupplementalSection {
    heading: &'static str,
    body: &'static str,
    /// Dioxus version the snippet was last verified against (e.g. "0.7.3").
    /// Bump this whenever the snippet is touched so stale snippets are
    /// auditable from the registry source.
    version_verified: &'static str,
}

fn supplemental_sections() -> &'static [SupplementalSection] {
    &[
        SupplementalSection {
            heading: "Writing cookies and response headers from server fns (set cookie, Set-Cookie, login)",
            body: "Server fns can write response headers (and cookies) by reaching for the per-request `FullstackContext`. This is the symmetric write-side to parsing `TypedHeader<Cookie>` for reads — `search_docs` covers the read side via the auth extension of `get_dsl_spec`.\n\n```rust\nuse dioxus::fullstack::FullstackContext;\n\n#[server]\nasync fn login(user: String, password: String) -> ServerFnResult<()> {\n    // …authenticate, mint a session id…\n    let cookie = format!(\n        \"sid={sid}; Path=/; HttpOnly; SameSite=Lax\"\n    );\n    let ctx = FullstackContext::current()\n        .ok_or_else(|| ServerFnError::new(\"no request context\"))?;\n    ctx.add_response_header(\"set-cookie\", cookie);\n    Ok(())\n}\n```\n\nNotes:\n- `FullstackContext::current()` returns `Option<Self>` — `None` outside a per-request scope (e.g. on the client side of a fullstack call). Unwrap with `ok_or_else(|| ServerFnError::new(...))` rather than `.unwrap()` so the client-side branch surfaces a typed error.\n- `add_response_header` takes `impl Into<HeaderName>, impl Into<HeaderValue>` and returns `()` — no `?` or `.map_err(...)` needed. Header name is lowercased; multiple `Set-Cookie` calls accumulate.\n- Construct `ServerFnError` with the `::new(\"msg\")` helper (dioxus-fullstack-core 0.7.3 made it a struct with `{ message, code, details }` fields). The pre-0.7.3 `ServerFnError::ServerError(\"msg\".into())` tuple variant no longer exists.\n- For verb-macro fns (`#[post(\"/api/login\")]`), the same call works — the FullstackContext is bound to the per-request scope.\n- To *delete* a cookie, set its `Max-Age=0` and an empty value.\n",
            version_verified: "0.7.3",
        },
        SupplementalSection {
            heading: "Reading cookies from server fns (TypedHeader, parse Cookie, session)",
            body: "Use `TypedHeader<Cookie>` as a server-fn extractor (or `cookies` arg on a verb-macro fn) to read incoming cookies.\n\n```rust\nuse axum_extra::{headers::Cookie, TypedHeader};\n\n#[get(\"/api/me\", cookies: TypedHeader<Cookie>)]\nasync fn me() -> ServerFnResult<Option<User>> {\n    let sid = cookies.get(\"sid\").unwrap_or_default();\n    // …look up the session…\n    Ok(None)\n}\n```\n\nNote: the extractor is declared only inside the verb-macro attribute — the Dioxus 0.7.9 macro binds `cookies` into scope itself, so adding `cookies: TypedHeader<Cookie>` to the rust fn signature would break `FromRequest` for the body tuple. Pair with [Writing cookies and response headers from server fns] for the login/logout side. The dioxus-mcp `get_dsl_spec` auth extension wires both sides into scaffolded `Resource` and `ServerFn` primitives via the `auth_required: true` flag.\n",
            version_verified: "0.7.9",
        },
    ]
}

fn split_sections(md: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let mut cur_heading: Option<String> = None;
    let mut cur_body = String::new();
    let mut in_fence = false;

    for line in md.lines() {
        let trimmed = line.trim_start();
        // Toggle on any fence of 3+ backticks (covers ``` and ````).
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            cur_body.push_str(line);
            cur_body.push('\n');
            continue;
        }
        if !in_fence {
            let is_h1 = line.starts_with("# ");
            let is_h2 = line.starts_with("## ");
            if is_h1 || is_h2 {
                if let Some(h) = cur_heading.take() {
                    out.push(Section {
                        heading: h,
                        body: std::mem::take(&mut cur_body),
                    });
                }
                let strip = if is_h1 { 2 } else { 3 };
                cur_heading = Some(line[strip..].trim().to_string());
                continue;
            }
        }
        cur_body.push_str(line);
        cur_body.push('\n');
    }
    if let Some(h) = cur_heading {
        out.push(Section {
            heading: h,
            body: cur_body,
        });
    }
    out
}

fn section_url(version: &str, heading: &str) -> String {
    format!(
        "https://dioxuslabs.com/learn/{}/#{}",
        version,
        slugify(heading)
    )
}

fn raw_corpus_url(version: &str) -> String {
    format!("https://dioxuslabs.com/learn/{version}/llms-full.txt")
}

const SECTION_BODY_CAP: usize = 4000;

fn section_body_capped(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.len() <= SECTION_BODY_CAP {
        return trimmed.to_string();
    }
    let end = floor_char_boundary(trimmed, SECTION_BODY_CAP);
    let mut out = trimmed[..end].to_string();
    out.push_str("\n\n… [truncated; fetch `raw_url` for the full corpus]");
    out
}

fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_dash = true;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}

async fn fetch_llms_full(state: &Arc<State>, version: &str) -> Result<Arc<CachedDoc>, String> {
    let url = format!("https://dioxuslabs.com/learn/{version}/llms-full.txt");
    if let Some(cached) = state.doc_cache.get(&url).await {
        return Ok(cached);
    }
    let resp = state
        .http
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("llms-full fetch: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "llms-full {} returned HTTP {}",
            version,
            resp.status()
        ));
    }
    let body = resp
        .text()
        .await
        .map_err(|e| format!("llms-full body: {e}"))?;
    let cached = Arc::new(CachedDoc { body });
    state.doc_cache.insert(url, cached.clone()).await;
    Ok(cached)
}

pub(crate) fn tokenize(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(|t| t.to_string())
        .collect()
}

pub(crate) fn score_terms(query: &[String], doc: &[String]) -> f32 {
    let mut score = 0.0_f32;
    for q in query {
        // exact term hits
        let exact = doc.iter().filter(|t| *t == q).count();
        if exact > 0 {
            score += 1.0 + (exact as f32).ln();
            continue;
        }
        // prefix / substring fallback: "router" matches "routing", "signal" matches "signals"
        let stem_len = q.len().saturating_sub(2).max(3);
        let stem = &q[..q.len().min(stem_len)];
        let approx = doc
            .iter()
            .filter(|t| t.starts_with(stem) || t.contains(q.as_str()))
            .count();
        if approx > 0 {
            score += 0.5 + (approx as f32).ln().max(0.0);
        }
    }
    score
}

fn best_snippet(body: &str, qterms: &[String]) -> String {
    let lc = body.to_lowercase();
    let mut best: (i64, usize) = (i64::MIN, 0);
    for q in qterms {
        if let Some(idx) = lc.find(q.as_str()) {
            let start = idx.saturating_sub(80);
            let weight = -(idx as i64);
            if weight > best.0 {
                best = (weight, start);
            }
        }
    }
    if best.0 == i64::MIN {
        return body.chars().take(200).collect();
    }
    let start = floor_char_boundary(body, best.1);
    let end = ceil_char_boundary(body, (start + 240).min(body.len()));
    let mut s = body[start..end].trim().replace('\n', " ");
    if start > 0 {
        s.insert_str(0, "… ");
    }
    if end < body.len() {
        s.push_str(" …");
    }
    s
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

async fn detect_version(state: &Arc<State>) -> String {
    state
        .project
        .lock()
        .await
        .version_major_minor()
        .map(|(maj, min)| format!("{maj}.{min}"))
        .unwrap_or_else(|| "0.7".to_string())
}
