//! Optional cargo invocations triggered by `execute_code` after a successful
//! scaffold: `cargo check` (compile-time gate) and `rustfmt` (style the files
//! this call wrote). Both are opt-in and best-effort — a failure surfaces as a
//! `next_steps` entry and never voids the scaffold result.

use std::path::{Path, PathBuf};

use crate::tools::scaffold::ScaffoldResult;

/// Run `cargo check --message-format=short` in `crate_root` with a generous
/// timeout. Returns `Some(message)` when the check fails (or doesn't complete),
/// `None` when it succeeds. The returned message is a single `next_steps`
/// entry — we truncate stderr so a slow build doesn't bloat the response.
pub(super) async fn run_cargo_check(crate_root: &Path) -> Option<String> {
    use tokio::process::Command;
    use tokio::time::{Duration, timeout};

    let mut cmd = Command::new("cargo");
    cmd.arg("check")
        .arg("--message-format=short")
        .current_dir(crate_root);
    // Quiet down build progress so the captured output is just diagnostics.
    cmd.env("CARGO_TERM_COLOR", "never");

    let fut = cmd.output();
    let out = match timeout(Duration::from_secs(180), fut).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return Some(format!(
                "cargo_check: failed to spawn `cargo check`: {e} — run it yourself in {}",
                crate_root.display()
            ));
        }
        Err(_) => {
            return Some(format!(
                "cargo_check: `cargo check` exceeded the 180s budget — run it yourself in {}",
                crate_root.display()
            ));
        }
    };
    if out.status.success() {
        return None;
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Pull the first ~20 lines of diagnostics — enough for the first few
    // errors without burying the rest of the response.
    let snippet: String = stderr.lines().take(20).collect::<Vec<_>>().join("\n");
    Some(format!(
        "cargo_check: `cargo check` failed (exit {:?}). First diagnostics:\n{snippet}",
        out.status.code()
    ))
}

/// Run `rustfmt` over the exact set of files this scaffold call wrote or
/// modified. We bypass `cargo fmt` so the formatting is scoped — `cargo fmt`
/// would format the entire crate, which is surprising on top of a focused
/// scaffold. Returns `Some(message)` when formatting fails or rustfmt is
/// unavailable, `None` on success. The returned message is a single
/// `next_steps` entry; the scaffolded files are kept either way.
pub(super) async fn run_cargo_fmt(crate_root: &Path, result: &ScaffoldResult) -> Option<String> {
    use tokio::process::Command;
    use tokio::time::{Duration, timeout};

    // Collect a deduped, .rs-only list of touched paths. rustfmt rejects
    // non-Rust files (e.g. Cargo.toml, mod.rs we wrote) wholesale, so we
    // filter rather than let it bail out.
    let mut paths: Vec<PathBuf> = Vec::new();
    for p in result
        .files_created
        .iter()
        .chain(result.files_modified.iter())
    {
        if p.extension().and_then(|x| x.to_str()) != Some("rs") {
            continue;
        }
        if !paths.contains(p) {
            paths.push(p.clone());
        }
    }
    if paths.is_empty() {
        return None;
    }

    let mut cmd = Command::new("rustfmt");
    cmd.arg("--edition=2024");
    for p in &paths {
        cmd.arg(p);
    }
    cmd.current_dir(crate_root);

    let fut = cmd.output();
    let out = match timeout(Duration::from_secs(60), fut).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return Some(format!(
                "format_after: failed to spawn `rustfmt`: {e} — run `cargo fmt` yourself in {}",
                crate_root.display()
            ));
        }
        Err(_) => {
            return Some(format!(
                "format_after: `rustfmt` exceeded the 60s budget — run `cargo fmt` yourself in {}",
                crate_root.display()
            ));
        }
    };
    if out.status.success() {
        return None;
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let snippet: String = stderr.lines().take(10).collect::<Vec<_>>().join("\n");
    Some(format!(
        "format_after: `rustfmt` failed (exit {:?}). First diagnostics:\n{snippet}",
        out.status.code()
    ))
}
