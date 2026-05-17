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
