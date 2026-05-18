use std::path::Path;

use heck::{ToPascalCase, ToSnakeCase};
use minijinja::context;

use crate::tools::scaffold::ScaffoldResult;

use super::super::render::*;
use super::super::types::*;

const STALENESS_GATE_TPL: &str = r#"use dioxus::prelude::*;

/// {{ pascal }}Gate — optimistic-lock revision counter.
///
/// Use this when a component needs to gate an async reconciliation on
/// "did anyone else bump this signal while my work was in flight?". The
/// canonical shape:
///
/// ```ignore
/// let mut gate = use_{{ snake }}_gate();
/// let snap = gate.snapshot();          // capture the current revision
/// gate.bump();                          // mark the optimistic write
/// spawn(async move {
///     let _ = save().await;             // long-running async tail
///     if gate.matches(snap) {
///         // no other bump happened — safe to reconcile.
///     }
/// });
/// ```
#[derive(Clone, Copy)]
pub struct {{ pascal }}Gate {
    rev: Signal<u32>,
}

impl {{ pascal }}Gate {
    /// Increment the revision counter and return the new value. The
    /// increment is `wrapping_add` so a long-running session can't panic
    /// from overflow — the snapshot/compare semantics still work.
    pub fn bump(&mut self) -> u32 {
        let next = self.rev.peek().wrapping_add(1);
        self.rev.set(next);
        next
    }

    /// Reactive equality check against a previously-captured snapshot.
    /// Reading the underlying signal here means `use_future` /
    /// `use_effect` callers automatically re-run when the revision bumps.
    pub fn matches(&self, snap: u32) -> bool {
        *self.rev.read() == snap
    }

    /// Non-reactive read of the current revision. Use this in polling-
    /// stub `use_future` bodies where the future shouldn't subscribe to
    /// the gate (otherwise every bump would restart the future).
    pub fn snapshot(&self) -> u32 {
        *self.rev.peek()
    }
}

pub fn provide_{{ snake }}_gate() -> {{ pascal }}Gate {
    use_context_provider(|| {{ pascal }}Gate { rev: use_signal(|| 0u32) })
}

pub fn use_{{ snake }}_gate() -> {{ pascal }}Gate {
    use_context::<{{ pascal }}Gate>()
}
"#;

pub(crate) fn generate_staleness_gate(
    crate_root: &Path,
    g: &DslStalenessGate,
) -> Result<ScaffoldResult, String> {
    if g.name.trim().is_empty() {
        return Err("staleness_gate: `name` is required".to_string());
    }
    let snake = g.name.to_snake_case();
    let pascal = g.name.to_pascal_case();
    // Gate files live under src/state/ alongside view_states and
    // client_stores; the `_gate` suffix keeps the namespace clear.
    let module_stem = format!("{snake}_gate");
    let body = render(
        "staleness_gate",
        STALENESS_GATE_TPL,
        context! {
            snake => snake,
            pascal => pascal,
        },
    )?;
    write_module_file_with_cfg(crate_root, "src/state", &module_stem, body, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The minimal contract: the generator emits the file with the
    /// `{Pascal}Gate` struct, `bump` / `matches` / `snapshot` helpers, and
    /// both context entry points.
    #[test]
    fn emits_struct_and_helpers() {
        let dir = tempfile::TempDir::new().unwrap();
        let g = DslStalenessGate {
            name: "LocalLock".into(),
        };
        let r = generate_staleness_gate(dir.path(), &g).unwrap();
        let file = r
            .files_created
            .iter()
            .find(|p| p.ends_with("local_lock_gate.rs"))
            .expect("gate file present");
        let body = std::fs::read_to_string(file).unwrap();
        assert!(body.contains("pub struct LocalLockGate"));
        assert!(body.contains("pub fn bump(&mut self) -> u32"));
        assert!(body.contains("pub fn matches(&self, snap: u32) -> bool"));
        assert!(body.contains("pub fn snapshot(&self) -> u32"));
        assert!(body.contains("pub fn provide_local_lock_gate() -> LocalLockGate"));
        assert!(body.contains("pub fn use_local_lock_gate() -> LocalLockGate"));
        // Polling-stub friendly: `.peek()` MUST appear so polling futures
        // can read without subscribing.
        assert!(body.contains(".peek()"));
        // Bump uses wrapping_add so a long-running session can't panic.
        assert!(body.contains("wrapping_add"));
    }

    /// Names normalise: PascalCase input still produces snake_case file
    /// names and identifiers.
    #[test]
    fn name_case_is_normalised() {
        let dir = tempfile::TempDir::new().unwrap();
        let g = DslStalenessGate {
            name: "BoardReconciler".into(),
        };
        let r = generate_staleness_gate(dir.path(), &g).unwrap();
        assert!(
            r.files_created
                .iter()
                .any(|p| p.ends_with("board_reconciler_gate.rs")),
            "expected board_reconciler_gate.rs in files_created: {:?}",
            r.files_created,
        );
    }

    #[test]
    fn rejects_empty_name() {
        let dir = tempfile::TempDir::new().unwrap();
        let g = DslStalenessGate {
            name: "".into(),
        };
        let err = generate_staleness_gate(dir.path(), &g).unwrap_err();
        assert!(err.contains("name"), "got: {err}");
    }
}
