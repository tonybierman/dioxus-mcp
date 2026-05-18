//! Catalog of the official Dioxus 0.7 components plus the
//! `dx components add <name>` install flow.
//!
//! `DX_COMPONENT_CATALOG_ENTRIES` is the single source of truth for the
//! widget names + descriptions + prop/event surface hints. `dx_component_names`
//! is the names-only iterator other modules consume.
//!
//! Install flow: a doc's `dx_components: [name, ...]` entries are validated
//! against the catalog, then either surfaced as dry-run hints
//! (`surface_dx_components_hints`) or shelled out via `dx components add`
//! (`install_dx_components`). After a successful install, the just-written
//! files under `src/components/{name}/` are recorded individually by
//! `record_dx_component_files`, and `suppress_dead_code_on_enums` patches the
//! component's enums with `#[allow(dead_code)]` so the upstream catalog
//! templates don't trip the lint.

use std::collections::BTreeSet;
use std::path::Path;

use heck::{ToPascalCase, ToSnakeCase};

use crate::tools::scaffold::ScaffoldResult;

use super::types::*;

/// Names + one-line descriptions + prop/event surface hints for the official
/// Dioxus 0.7 component catalog (`dx components add <name>`). Kept in sync
/// with the catalog block in `specs.rs` — `dx_components_catalog_matches_spec_block`
/// (in tests.rs) wires the two sources together so they can't silently drift.
/// The `list_components` tool returns these entries as a dedicated, smaller
/// payload so agents can pull just the catalog without the rest of the spec.
///
/// The third tuple slot is the prop/event surface hint — a one-liner that
/// captures how the widget is controlled (its main props and events), enough
/// to avoid a `describe_component` round-trip for the obvious cases. Use
/// `describe_component <name>` to get the full surface.
pub const DX_COMPONENT_CATALOG_ENTRIES: &[(&str, &str, &str)] = &[
    (
        "accordion",
        "An accordion component for displaying collapsible content sections.",
        "forwards AccordionProps; on_change + on_trigger_click events",
    ),
    (
        "alert_dialog",
        "An alert dialog component for displaying important messages and requiring user confirmation.",
        "forwards AlertDialogRootProps; on_open_change + on_click events; modal confirm dialog",
    ),
    (
        "aspect_ratio",
        "An aspect ratio component for maintaining a consistent width-to-height ratio of an element.",
        "forwards AspectRatioProps; layout-only wrapper, no events",
    ),
    (
        "avatar",
        "An avatar component for displaying user profile images or initials.",
        "forwards AvatarProps; on_load + on_error + on_state_change; extends GlobalAttributes",
    ),
    (
        "badge",
        "A small label to display status or categorization.",
        "forwards BadgeProps; presentational, no events; extends GlobalAttributes",
    ),
    (
        "button",
        "A button component for triggering actions or events when clicked.",
        "inline props (variant: ButtonVariant, size: ButtonSize); onclick + onmousedown/up + onkeydown; extends GlobalAttributes + button",
    ),
    (
        "calendar",
        "A calendar grid component for selecting dates.",
        "forwards CalendarProps; on_date_change + on_range_change + on_view_change; extends GlobalAttributes",
    ),
    (
        "card",
        "A simple card component.",
        "inline children-only wrapper; no events; extends GlobalAttributes",
    ),
    (
        "checkbox",
        "A togglable checkbox component.",
        "forwards CheckboxProps (checked: ReadSignal<Option<CheckboxState>>, default_checked, on_checked_change)",
    ),
    (
        "collapsible",
        "A collapsible component for showing and hiding content sections.",
        "forwards CollapsibleProps; on_open_change; extends GlobalAttributes",
    ),
    (
        "color_picker",
        "Allows selecting a color using a variety of input methods.",
        "forwards ColorPickerRootProps; on_color_change + on_value_change + on_open_change + oninput; extends GlobalAttributes",
    ),
    (
        "combobox",
        "An autocomplete input + popover for picking a value from a filterable list of options.",
        "wrapper defines its own ComboboxProps<T = String>; on_value_change + on_query_change + on_open_change; extends GlobalAttributes",
    ),
    (
        "context_menu",
        "A context menu component for displaying a list of actions or options after right-clicking an area.",
        "forwards ContextMenuProps; on_open_change + on_select",
    ),
    (
        "date_picker",
        "A date picker component for selecting or inputting dates.",
        "forwards DatePickerProps; on_value_change + on_range_change + on_format_* placeholders; extends GlobalAttributes",
    ),
    (
        "dialog",
        "A dialog component for displaying modal content.",
        "forwards DialogRootProps; on_open_change; pair with DialogTrigger + DialogContent children",
    ),
    (
        "drag_and_drop_list",
        "A vertically sortable list supporting drag-and-drop, touch, or keyboard input.",
        "forwards DragAndDropListProps; pointer/drag/keyboard handlers; extends GlobalAttributes",
    ),
    (
        "dropdown_menu",
        "A dropdown menu component for selecting options from a list.",
        "forwards DropdownMenuProps; on_open_change + on_select",
    ),
    (
        "form",
        "A form component for collecting user input.",
        "inline children-only wrapper; submit handler set by caller via ..attributes",
    ),
    (
        "hover_card",
        "A hover card component for displaying additional information on hover.",
        "forwards HoverCardProps; on_open_change",
    ),
    (
        "input",
        "An input field component for user text entry.",
        "inline props with 19 DOM events forwarded individually (oninput, onchange, onfocus, onkeydown, …); extends GlobalAttributes + input",
    ),
    (
        "item",
        "A component for displaying content.",
        "inline content wrapper; onclick + onkeydown; extends GlobalAttributes + div + p",
    ),
    (
        "label",
        "An accessible label component for form elements.",
        "forwards LabelProps; accessible label for form controls; no events",
    ),
    (
        "menubar",
        "A menubar component for a collection of menu items.",
        "forwards MenubarProps; on_select",
    ),
    (
        "navbar",
        "A navbar component for navigation between pages.",
        "forwards NavbarProps; onclick + onmounted + on_select",
    ),
    (
        "pagination",
        "Navigation controls for paged content.",
        "forwards PaginationLinkProps; onclick + onmousedown/up; extends GlobalAttributes + a",
    ),
    (
        "popover",
        "A popover component for collapsible content.",
        "forwards PopoverRootProps; on_open_change",
    ),
    (
        "progress",
        "An accessible progress-bar indicator.",
        "forwards ProgressProps; presentational; value prop drives the bar",
    ),
    (
        "radio_group",
        "A group of radio buttons for selecting one option from a set.",
        "forwards RadioGroupProps; on_value_change",
    ),
    (
        "scroll_area",
        "A scrollable area component.",
        "forwards ScrollAreaProps; presentational; no events",
    ),
    (
        "select",
        "A select dropdown component with typeahead support.",
        "forwards SelectGroupLabelProps; on_value_change + on_values_change + on_open_change",
    ),
    (
        "separator",
        "A visual separator between different sections of the page.",
        "forwards SeparatorProps; presentational; no events",
    ),
    (
        "sheet",
        "A sheet component as an edge panel that complements the main content.",
        "forwards DialogRootProps; on_open_change + onclick; extends GlobalAttributes",
    ),
    (
        "sidebar",
        "A sidebar component as a vertical panel fixed to the screen edge for quick access to different sections.",
        "inline props; onclick + on_open_change; extends GlobalAttributes + button",
    ),
    (
        "skeleton",
        "A placeholder component for all loading elements.",
        "presentational; no events; extends GlobalAttributes",
    ),
    (
        "slider",
        "An accessible slider component.",
        "forwards SliderProps; on_value_change",
    ),
    (
        "switch",
        "A togglable switch component.",
        "forwards SwitchProps; on_checked_change",
    ),
    (
        "tabs",
        "A tabbed interface component.",
        "forwards TabsProps; on_value_change; extends GlobalAttributes",
    ),
    (
        "textarea",
        "A textarea component for multi-line text input.",
        "inline props with 18 DOM events forwarded individually (oninput, onchange, onfocus, onkeydown, …); extends GlobalAttributes + textarea",
    ),
    (
        "toast",
        "A toast notification component.",
        "forwards ToastProps; on_close; extends GlobalAttributes",
    ),
    (
        "toggle",
        "A simple toggle button component.",
        "forwards ToggleProps; on_pressed_change + onfocus + onkeydown + onmounted",
    ),
    (
        "toggle_group",
        "A group of toggle buttons for selecting one or more options from a set.",
        "forwards ToggleGroupProps; on_pressed_change emits a HashSet<usize> of the pressed indices",
    ),
    (
        "toolbar",
        "A toolbar component for grouping related inputs.",
        "forwards ToolbarProps; on_click; extends GlobalAttributes + div",
    ),
    (
        "tooltip",
        "A tooltip component for additional information on hover or focus.",
        "forwards TooltipProps; on_open_change",
    ),
    (
        "virtual_list",
        "A virtualized list component for large datasets.",
        "forwards VirtualListProps; render-prop iterator pattern for large datasets",
    ),
];

/// Names-only projection of [`DX_COMPONENT_CATALOG_ENTRIES`]. Used by
/// validation paths that only care whether a `dx_components:` entry is
/// catalog-known.
pub(crate) fn dx_component_names() -> impl Iterator<Item = &'static str> {
    DX_COMPONENT_CATALOG_ENTRIES.iter().map(|(n, _, _)| *n)
}

/// Validate `doc.dx_components` against the catalog. Returns the deduped
/// snake-case-normalized list of valid names; typos and unknown entries are
/// appended to `result.next_steps` so the caller sees them either way.
pub(super) fn validate_dx_components(doc: &DslDoc, result: &mut ScaffoldResult) -> Vec<String> {
    if doc.dx_components.is_empty() {
        return Vec::new();
    }
    let catalog: BTreeSet<&str> = dx_component_names().collect();
    let mut valid: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for raw in &doc.dx_components {
        let name = raw.trim().to_snake_case();
        if name.is_empty() {
            continue;
        }
        if !seen.insert(name.clone()) {
            continue;
        }
        if !catalog.contains(name.as_str()) {
            result.next_steps.push(format!(
                "dx_components: {raw:?} is not in the official Dioxus 0.7 catalog — call `get_dsl_spec {{ sections: [components], include_prologue: false }}` to list the 45 valid names"
            ));
            continue;
        }
        valid.push(name);
    }
    valid
}

/// Append the post-install import hint (`crate::components::{name}::{Pascal}`)
/// so the caller knows the path to type into rsx!. Shared between the
/// dry-run hint path and the real-install path.
pub(super) fn push_import_hint(valid: &[String], result: &mut ScaffoldResult) {
    if valid.is_empty() {
        return;
    }
    let import_paths: Vec<String> = valid
        .iter()
        .map(|name| {
            let pascal = name.to_pascal_case();
            format!("crate::components::{name}::{pascal}")
        })
        .collect();
    result.next_steps.push(format!(
        "dx_components: drop into rsx! via {}",
        import_paths.join(", ")
    ));
}

/// Dry-run path: surface `dx components add <name>` commands as next_steps
/// for each catalog-valid entry, plus the one-time setup reminders. Used
/// only when `dry_run: true` — non-dry-run calls go through
/// `install_dx_components` which actually shells out.
pub(super) fn surface_dx_components_hints(
    doc: &DslDoc,
    crate_root: &Path,
    result: &mut ScaffoldResult,
) {
    let valid = validate_dx_components(doc, result);
    if valid.is_empty() {
        return;
    }
    for name in &valid {
        result.next_steps.push(format!(
            "dx_components: would run `dx components add {name}` in {} (dry_run)",
            crate_root.display()
        ));
    }
    result.next_steps.push(
        "dx_components: first-time install also needs `mod components;` in your crate root (main.rs or lib.rs) and \
         `asset!(\"/assets/dx-components-theme.css\")` mounted in your App body — `dx components add` prints these reminders after writing files".into(),
    );
    push_import_hint(&valid, result);
}

/// Real-install path: shell out to `dx components add <name>` for each
/// catalog-valid entry, with a per-command timeout. On failure (missing
/// `dx`, network error, non-zero exit) the function falls back to surfacing
/// the install command on `next_steps` so the caller still sees what to run.
pub(super) async fn install_dx_components(
    doc: &DslDoc,
    crate_root: &Path,
    result: &mut ScaffoldResult,
) {
    use tokio::process::Command;
    use tokio::time::{Duration, timeout};

    let valid = validate_dx_components(doc, result);
    if valid.is_empty() {
        return;
    }

    let mut installed: Vec<String> = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();
    for name in &valid {
        let mut cmd = Command::new("dx");
        cmd.arg("components")
            .arg("add")
            .arg(name)
            .current_dir(crate_root);
        // Quiet down ANSI in the captured output so a failure snippet pastes
        // cleanly into the response.
        cmd.env("CARGO_TERM_COLOR", "never");

        let fut = cmd.output();
        match timeout(Duration::from_secs(180), fut).await {
            Ok(Ok(out)) if out.status.success() => {
                installed.push(name.clone());
            }
            Ok(Ok(out)) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let snippet: String = stderr.lines().take(10).collect::<Vec<_>>().join("\n");
                failed.push((
                    name.clone(),
                    format!("exit {:?}: {snippet}", out.status.code()),
                ));
            }
            Ok(Err(e)) => {
                // Spawn failure — `dx` not on PATH, permission denied, etc.
                // Stop iterating: subsequent names will hit the same failure.
                failed.push((name.clone(), format!("failed to spawn `dx`: {e}")));
                for remaining in valid.iter().skip(installed.len() + failed.len()) {
                    failed.push((
                        remaining.clone(),
                        "skipped — `dx` not available on PATH".into(),
                    ));
                }
                break;
            }
            Err(_) => {
                failed.push((name.clone(), "exceeded 180s timeout".into()));
            }
        }
    }

    if !installed.is_empty() {
        // The newly-installed component dir lands under src/components/{name}/.
        // Enumerate every file `dx` wrote into the dir so the structured
        // result names them explicitly — no `ls` round-trip and no
        // verify_install bounce to know mod.rs / component.rs / docs.md
        // landed.
        for name in &installed {
            record_dx_component_files(crate_root, name, result);
        }
        result.next_steps.push(format!(
            "dx_components: installed via `dx components add` → {}",
            installed.join(", ")
        ));
    }
    if !failed.is_empty() {
        result.next_steps.push(
            "dx_components: install failed for some entries — fall back to running these by hand:"
                .into(),
        );
        for (name, reason) in &failed {
            result.next_steps.push(format!(
                "  - `dx components add {name}` in {} ({reason})",
                crate_root.display()
            ));
        }
    }
    // First-time-setup reminder always fires — `dx` prints these on every
    // install but the caller may have missed them in the captured output.
    result.next_steps.push(
        "dx_components: first-time install also needs `mod components;` in your crate root (main.rs or lib.rs) and \
         `asset!(\"/assets/dx-components-theme.css\")` mounted in your App body".into(),
    );
    push_import_hint(&valid, result);
}

/// Walk `src/components/{name}/` after a successful `dx components add`
/// invocation and record every file it wrote into `result.files_created`,
/// keeping the per-file detail callers asked for instead of just pointing at
/// the dir. The post-install dead-code touch-up lives here too: when we
/// modify `component.rs` to suppress upstream's unused-variant warnings,
/// that file moves to `files_modified` so the "what dx wrote" vs "what we
/// patched on top" split stays honest.
///
/// If the dir can't be read (permissions / a non-standard layout) the
/// helper falls back to recording the dir path itself, so the response
/// always points at the install location.
pub(super) fn record_dx_component_files(
    crate_root: &Path,
    name: &str,
    result: &mut ScaffoldResult,
) {
    let dir = crate_root.join("src/components").join(name);
    let comp = dir.join("component.rs");
    let comp_touched = suppress_dead_code_on_enums(&comp) == Some(true);

    if let Ok(read) = std::fs::read_dir(&dir) {
        let mut entries: Vec<std::path::PathBuf> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        entries.sort();
        for path in entries {
            if comp_touched && path == comp {
                result.files_modified.push(path);
            } else {
                result.files_created.push(path);
            }
        }
    } else if dir.exists() {
        result.files_created.push(dir);
    }
}

/// Prepend `#[allow(dead_code)]` to each `pub enum …` in `path` (typically
/// the just-installed `src/components/<name>/component.rs`). Returns
/// `Some(true)` when the file was edited, `Some(false)` when it already had
/// the attribute on every pub enum (or had none), and `None` when the file
/// can't be read. Idempotent: re-running on a touched file is a no-op.
///
/// We deliberately keep this string-based and conservative — only `pub enum`
/// declarations at column 0 (no indentation) are matched, so an enum nested
/// inside a fn body or impl is left alone. The upstream catalog templates put
/// their enums at the top level so this catches every real case.
pub(super) fn suppress_dead_code_on_enums(path: &Path) -> Option<bool> {
    let src = std::fs::read_to_string(path).ok()?;
    let mut out = String::with_capacity(src.len() + 64);
    let mut modified = false;
    let mut prev_blank_or_attr = true;
    let mut pending_attrs = String::new();
    for line in src.split_inclusive('\n') {
        let trimmed = line.trim_start();
        // Track preceding attribute lines so we don't insert a duplicate.
        let is_attr = trimmed.starts_with("#[");
        if is_attr {
            pending_attrs.push_str(line);
        }
        if line.starts_with("pub enum ") {
            // Skip if any preceding attribute line in this chunk already
            // contains `allow(dead_code)` — leave caller-authored intent alone.
            let already = pending_attrs.contains("dead_code");
            if !already {
                out.push_str("#[allow(dead_code)]\n");
                modified = true;
            }
            out.push_str(line);
            pending_attrs.clear();
            prev_blank_or_attr = false;
            continue;
        }
        // Reset the attr-buffer on any non-attr, non-blank line so attributes
        // are only associated with the immediately-following item.
        if !is_attr && !trimmed.is_empty() {
            pending_attrs.clear();
            prev_blank_or_attr = false;
        } else if trimmed.is_empty() {
            prev_blank_or_attr = true;
        }
        out.push_str(line);
    }
    let _ = prev_blank_or_attr;
    if modified {
        std::fs::write(path, out).ok()?;
    }
    Some(modified)
}
