use std::sync::Arc;

use schemars::JsonSchema;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};

use crate::state::{CachedDoc, State};

const SITEMAP_URL: &str = "https://dioxuslabs.com/sitemap.xml";

// ---------- search_docs ----------

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
    pub url: String,
    pub title: Option<String>,
    pub score: f32,
    pub snippet: String,
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
    let urls = fetch_sitemap(state).await?;
    let prefix = format!("https://dioxuslabs.com/learn/{version}/");
    let candidates: Vec<String> = urls
        .into_iter()
        .filter(|u| u.starts_with(&prefix))
        .collect();

    let qterms = tokenize(&p.query);
    if qterms.is_empty() {
        return Err("query is empty".into());
    }

    // Cheap pass: rank by overlap between URL slug and query.
    let mut url_ranked: Vec<(f32, String)> = candidates
        .into_iter()
        .map(|u| {
            let slug_terms = tokenize(&u.replace('/', " ").replace('-', " ").replace('_', " "));
            let score = score_terms(&qterms, &slug_terms) * 0.5;
            (score, u)
        })
        .collect();
    url_ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    // Take top 12 by URL match, fetch each, rescore on body.
    let to_fetch: Vec<String> = url_ranked
        .into_iter()
        .take(12)
        .map(|(_, u)| u)
        .collect();

    let mut hits: Vec<DocHit> = Vec::new();
    for url in to_fetch {
        let doc = match fetch_doc(state, &url).await {
            Ok(d) => d,
            Err(_) => continue,
        };
        let body_terms = tokenize(&doc.body);
        let title_terms = doc.title.as_deref().map(tokenize).unwrap_or_default();
        let body_score = score_terms(&qterms, &body_terms);
        let title_score = score_terms(&qterms, &title_terms) * 3.0;
        let score = body_score + title_score;
        if score <= 0.0 {
            continue;
        }
        let snippet = best_snippet(&doc.body, &qterms);
        hits.push(DocHit {
            url: doc.url.clone(),
            title: doc.title.clone(),
            score,
            snippet,
        });
    }

    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    hits.truncate(limit);

    Ok(SearchDocsResult {
        query: p.query,
        version,
        hits,
    })
}

fn tokenize(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(|t| t.to_string())
        .collect()
}

fn score_terms(query: &[String], doc: &[String]) -> f32 {
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
    let start = best.1;
    let end = (start + 240).min(body.len());
    let mut s = body[start..end].trim().replace('\n', " ");
    if start > 0 {
        s.insert_str(0, "… ");
    }
    if end < body.len() {
        s.push_str(" …");
    }
    s
}

async fn fetch_sitemap(state: &Arc<State>) -> Result<Vec<String>, String> {
    if let Some(cached) = state.doc_cache.get(SITEMAP_URL).await {
        return Ok(cached
            .body
            .lines()
            .filter(|l| !l.is_empty())
            .map(|s| s.to_string())
            .collect());
    }
    let resp = state
        .http
        .get(SITEMAP_URL)
        .send()
        .await
        .map_err(|e| format!("sitemap fetch: {e}"))?;
    let text = resp
        .text()
        .await
        .map_err(|e| format!("sitemap body: {e}"))?;
    let urls: Vec<String> = text
        .split("<loc>")
        .skip(1)
        .filter_map(|chunk| chunk.split("</loc>").next().map(|s| s.trim().to_string()))
        .filter(|u| !u.is_empty())
        .collect();
    let cached = Arc::new(CachedDoc {
        url: SITEMAP_URL.into(),
        title: None,
        body: urls.join("\n"),
    });
    state.doc_cache.insert(SITEMAP_URL.into(), cached).await;
    Ok(urls)
}

async fn fetch_doc(state: &Arc<State>, url: &str) -> Result<Arc<CachedDoc>, String> {
    if let Some(cached) = state.doc_cache.get(url).await {
        return Ok(cached);
    }
    let resp = state
        .http
        .get(url)
        .send()
        .await
        .map_err(|e| format!("fetch {url}: {e}"))?;
    let html = resp
        .text()
        .await
        .map_err(|e| format!("body {url}: {e}"))?;

    // Parse + extract synchronously; drop `Html` before any await.
    let (title, body) = parse_html(&html);

    let cached = Arc::new(CachedDoc {
        url: url.to_string(),
        title,
        body,
    });
    state.doc_cache.insert(url.to_string(), cached.clone()).await;
    Ok(cached)
}

fn parse_html(html: &str) -> (Option<String>, String) {
    let doc = Html::parse_document(html);
    let title = Selector::parse("title")
        .ok()
        .and_then(|sel| doc.select(&sel).next())
        .map(|n| n.text().collect::<String>().trim().to_string());
    let body = ["article", "main", "body"]
        .iter()
        .find_map(|sel| {
            Selector::parse(sel).ok().and_then(|s| {
                doc.select(&s)
                    .next()
                    .map(|n| n.text().collect::<Vec<_>>().join(" "))
            })
        })
        .unwrap_or_default();
    (title, body)
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

// ---------- find_example ----------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct FindExampleParams {
    pub concept: String,
    /// Branch or tag, e.g. "main" or "v0.7.0". Defaults to "main".
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ExampleHit {
    pub name: String,
    pub path: String,
    pub url: String,
    pub raw_url: String,
    pub score: f32,
}

#[derive(Debug, Serialize)]
pub struct FindExampleResult {
    pub concept: String,
    pub git_ref: String,
    pub hits: Vec<ExampleHit>,
}

pub async fn find_example(
    state: &Arc<State>,
    p: FindExampleParams,
) -> Result<FindExampleResult, String> {
    let git_ref = p.git_ref.clone().unwrap_or_else(|| "main".into());
    let limit = p.limit.unwrap_or(3);
    let api_url = format!(
        "https://api.github.com/repos/DioxusLabs/dioxus/contents/examples?ref={}",
        git_ref
    );
    let cache_key = format!("examples:{git_ref}");
    let listing = if let Some(cached) = state.doc_cache.get(&cache_key).await {
        cached.body.clone()
    } else {
        let resp = state
            .http
            .get(&api_url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| format!("github fetch: {e}"))?;
        let body = resp
            .text()
            .await
            .map_err(|e| format!("github body: {e}"))?;
        state
            .doc_cache
            .insert(
                cache_key.clone(),
                Arc::new(CachedDoc {
                    url: api_url.clone(),
                    title: None,
                    body: body.clone(),
                }),
            )
            .await;
        body
    };

    let entries: serde_json::Value = serde_json::from_str(&listing)
        .map_err(|e| format!("github json: {e}"))?;
    let arr = entries.as_array().ok_or("expected array")?;

    let qterms = tokenize(&p.concept);
    let mut hits: Vec<ExampleHit> = Vec::new();
    for item in arr {
        let kind = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if kind != "file" && kind != "dir" {
            continue;
        }
        let name = item
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let path = item
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let html_url = item
            .get("html_url")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let download_url = item
            .get("download_url")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let name_terms = tokenize(&name.replace('-', " ").replace('_', " "));
        let score = score_terms(&qterms, &name_terms);
        if score > 0.0 {
            hits.push(ExampleHit {
                name,
                path,
                url: html_url,
                raw_url: download_url,
                score,
            });
        }
    }
    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    hits.truncate(limit);

    Ok(FindExampleResult {
        concept: p.concept,
        git_ref,
        hits,
    })
}
