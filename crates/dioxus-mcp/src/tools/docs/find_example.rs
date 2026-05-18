use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::state::{CachedDoc, State};
use crate::tools::docs::search_docs::{score_terms, tokenize};

mod local;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct FindExampleParams {
    /// Free-text concept to match against example folder/file names
    /// (e.g. "router", "fullstack", "use_signal"). Tokens are matched against
    /// hyphen/underscore-split names; multi-token queries OR across tokens.
    /// Omit (or pass empty) to return an alphabetically-sorted listing of every
    /// example in the repo — useful for a first call when you don't yet know
    /// which folder name to ask for.
    #[serde(default)]
    pub concept: Option<String>,
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
    /// `"upstream"` — a folder/file in the DioxusLabs/dioxus repo, browsable
    /// via `url` / `raw_url`. `"local"` — a pattern example shipped inside
    /// dioxus-mcp itself (because the upstream repo doesn't have a folder for
    /// it). Local hits set `body:` to the inline source so callers don't need
    /// a follow-up fetch; upstream hits leave `body:` empty.
    pub kind: &'static str,
    /// Inline source for `kind: "local"` hits. Always absent for upstream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Short blurb describing what the example demonstrates. Set for local
    /// hits (sourced from the registry below); empty for upstream hits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blurb: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FindExampleResult {
    pub concept: Option<String>,
    pub git_ref: String,
    pub hits: Vec<ExampleHit>,
}

pub async fn find_example(
    state: &Arc<State>,
    p: FindExampleParams,
) -> Result<FindExampleResult, String> {
    let git_ref = p.git_ref.clone().unwrap_or_else(|| "main".into());
    let concept = p
        .concept
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    // When no concept is given, return more results so the listing is useful.
    let limit = p.limit.unwrap_or(if concept.is_some() { 3 } else { 100 });
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
        let body = resp.text().await.map_err(|e| format!("github body: {e}"))?;
        state
            .doc_cache
            .insert(
                cache_key.clone(),
                Arc::new(CachedDoc { body: body.clone() }),
            )
            .await;
        body
    };

    let entries: serde_json::Value =
        serde_json::from_str(&listing).map_err(|e| format!("github json: {e}"))?;
    let arr = entries.as_array().ok_or("expected array")?;

    let qterms = concept.map(tokenize).unwrap_or_default();
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
        if qterms.is_empty() {
            hits.push(ExampleHit {
                name,
                path,
                url: html_url,
                raw_url: download_url,
                score: 0.0,
                kind: "upstream",
                body: None,
                blurb: None,
            });
        } else {
            let name_terms = tokenize(&name.replace(['-', '_'], " "));
            let score = score_terms(&qterms, &name_terms);
            if score > 0.0 {
                hits.push(ExampleHit {
                    name,
                    path,
                    url: html_url,
                    raw_url: download_url,
                    score,
                    kind: "upstream",
                    body: None,
                    blurb: None,
                });
            }
        }
    }

    // Merge in any local pattern examples — these cover wirings that the
    // upstream Dioxus repo doesn't ship a folder for (e.g. an SSE snapshot
    // stream + client reconcile, or the cookie-authed server fn prologue).
    // Local hits keep their own scoring against the same query terms so they
    // sort alongside upstream hits instead of always landing at the bottom.
    for entry in local::registry() {
        if qterms.is_empty() {
            hits.push(local_hit(entry, 0.0));
        } else {
            let name_terms = tokenize(&entry.name.replace(['-', '_'], " "));
            let blurb_terms = tokenize(entry.blurb);
            let combined: Vec<String> = name_terms.into_iter().chain(blurb_terms).collect();
            let score = score_terms(&qterms, &combined);
            if score > 0.0 {
                hits.push(local_hit(entry, score));
            }
        }
    }

    if qterms.is_empty() {
        hits.sort_by(|a, b| a.name.cmp(&b.name));
    } else {
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    hits.truncate(limit);

    Ok(FindExampleResult {
        concept: concept.map(str::to_owned),
        git_ref,
        hits,
    })
}

fn local_hit(entry: &local::LocalExample, score: f32) -> ExampleHit {
    ExampleHit {
        name: entry.name.to_string(),
        path: format!("dioxus-mcp/local-examples/{}.rs", entry.name),
        url: entry.url.to_string(),
        raw_url: String::new(),
        score,
        kind: "local",
        body: Some(entry.body.to_string()),
        blurb: Some(entry.blurb.to_string()),
    }
}
