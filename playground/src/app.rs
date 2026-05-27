//! The playground shell: a 2×2 grid of DSL editor, live preview, generated
//! source, and validation. The preview is driven by a *local* YAML parse
//! (instant, every keystroke); the source/validation panes by a *debounced*
//! `execute_code` dry-run. The preview pane has two tabs: an **Approximate**
//! interpreter render, and a **Compiled** view that Applies the slice into the
//! scratch crate and embeds its `dx serve` output in an iframe.

use dioxus::prelude::*;

use crate::interpreter::{build_groups, ScreenNavigator};
use crate::mcp_client::{self, McpError, ScaffoldResult};
use crate::model;

/// Seed / preset: a client_crud Todo screen.
const DEMO_DOC: &str = r#"version: "1"
models:
  - name: Todo
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
      - {name: done, type: bool}
client_stores:
  - name: TodoStore
    item_type: Todo
    id_field: id
    id_type: i64
screens:
  - name: TodoScreen
    route: /todos
    template:
      kind: client_crud
      store: TodoStore
      item_type: Todo
      label_field: title
      checkbox_field: done
"#;

/// Preset: a server-backed resource slice (expands to list + new + edit).
const RESOURCE_DOC: &str = r#"version: "1"
resources:
  - name: Product
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
      - {name: price, type: f64}
      - {name: in_stock, type: bool}
"#;

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Approximate,
    Compiled,
}

#[component]
pub fn Playground() -> Element {
    let mut dsl_text = use_signal(|| DEMO_DOC.to_string());

    // Local parse → instant preview; keep last good doc so a typo doesn't blank it.
    let parsed = use_memo(move || model::parse_doc(&dsl_text()));
    let mut last_good = use_signal(|| model::parse_doc(DEMO_DOC).unwrap_or_default());
    use_effect(move || {
        if let Ok(doc) = parsed() {
            last_good.set(doc);
        }
    });

    // Debounce edits ~300ms before the dry-run that feeds source/validation.
    let mut debounced = use_signal(|| DEMO_DOC.to_string());
    use_effect(move || {
        let text = dsl_text();
        spawn(async move {
            gloo_timers::future::TimeoutFuture::new(300).await;
            if *dsl_text.peek() == text {
                debounced.set(text);
            }
        });
    });
    let plan = use_resource(move || async move { mcp_client::dry_run(&debounced()).await });

    // Unify the two disjoint preview sources for the navigator: handwritten
    // `screens:` (instant local parse) and server-synthesized `render_models`
    // (the debounced dry-run). Reading both inside one memo subscribes it to
    // both cadences.
    let groups = use_memo(move || {
        let screens = last_good().screens;
        let models = match &*plan.read() {
            Some(Ok(sr)) => sr.render_models.clone(),
            _ => Vec::new(),
        };
        build_groups(&screens, &models)
    });

    // Preview theming: the doc's `theme:` colors the preview (`--p-*` vars on
    // `.preview-root`), overridable via the picker. Falls back to the unstyled
    // card when absent.
    let reg_ctx = use_context::<Memo<Option<dioxus_mcp_registry::Registry>>>();
    let preview_theme = use_signal(|| None::<String>);
    let active_theme = use_memo(move || preview_theme().or_else(|| last_good().theme.clone()));
    let preview_decls = use_memo(move || preview_theme_decls(&reg_ctx(), &active_theme()));

    // Compiled-tab state: Apply writes into the scratch crate; the iframe shows
    // its `dx serve`. `iframe_nonce` busts the iframe cache after an Apply.
    let mut tab = use_signal(|| Tab::Approximate);
    let mut preview_url = use_signal(|| "http://localhost:8081".to_string());
    let mut apply_busy = use_signal(|| false);
    let mut apply_result = use_signal(|| None::<Result<ScaffoldResult, McpError>>);
    let mut iframe_nonce = use_signal(|| 0u32);

    rsx! {
        header { class: "pg-header",
            h1 { "dx-playground" }
            div { class: "pg-presets",
                span { "presets:" }
                button { onclick: move |_| dsl_text.set(DEMO_DOC.to_string()), "todo · client_crud" }
                button { onclick: move |_| dsl_text.set(RESOURCE_DOC.to_string()), "product · resources" }
            }
        }
        div { class: "pg-grid",
            section { class: "pg-pane",
                h2 { "DSL" }
                textarea {
                    class: "pg-editor",
                    spellcheck: false,
                    autocomplete: "off",
                    value: "{dsl_text}",
                    oninput: move |e| dsl_text.set(e.value()),
                }
            }

            section { class: "pg-pane",
                div { class: "pg-tabs",
                    button {
                        class: if tab() == Tab::Approximate { "tab active" } else { "tab" },
                        onclick: move |_| tab.set(Tab::Approximate),
                        "Approximate"
                    }
                    button {
                        class: if tab() == Tab::Compiled { "tab active" } else { "tab" },
                        onclick: move |_| tab.set(Tab::Compiled),
                        "Compiled"
                    }
                }
                match tab() {
                    Tab::Approximate => rsx! {
                        if let Err(e) = &*parsed.read() {
                            div { class: "pg-parse-banner", "YAML: {e}" }
                        }
                        if !preview_decls().is_empty() {
                            document::Style { ".preview-root {{ {preview_decls} }}" }
                        }
                        PreviewThemePicker { reg: reg_ctx, selected: preview_theme, doc_theme: last_good().theme }
                        ScreenNavigator { groups }
                    },
                    Tab::Compiled => rsx! {
                        div { class: "compiled-controls",
                            button {
                                class: "apply-btn",
                                disabled: apply_busy(),
                                onclick: move |_| {
                                    let code = dsl_text.peek().clone();
                                    apply_busy.set(true);
                                    apply_result.set(None);
                                    spawn(async move {
                                        let result = mcp_client::apply(&code).await;
                                        if result.is_ok() {
                                            iframe_nonce += 1;
                                        }
                                        apply_result.set(Some(result));
                                        apply_busy.set(false);
                                    });
                                },
                                if apply_busy() { "Applying…" } else { "Apply to scratch ▸" }
                            }
                            input {
                                class: "url-input",
                                value: "{preview_url}",
                                oninput: move |e| preview_url.set(e.value()),
                            }
                            match &*apply_result.read() {
                                Some(Ok(sr)) => rsx! {
                                    span { class: "apply-ok", "✓ wrote {sr.files_created.len()} files" }
                                },
                                Some(Err(e)) => rsx! { span { class: "apply-err", "{e}" } },
                                None => rsx! {},
                            }
                        }
                        p { class: "pg-status",
                            "Run "
                            code { "dx serve" }
                            " in dx-playground-scratch, set its URL above, then Apply."
                        }
                        iframe { class: "compiled-frame", src: "{preview_url}?n={iframe_nonce}" }
                    },
                }
            }

            section { class: "pg-pane",
                h2 { "Generated source · execute_code" }
                match &*plan.read() {
                    None => rsx! { p { class: "pg-status", "checking…" } },
                    Some(Ok(sr)) => rsx! { SourceView { result: sr.clone() } },
                    Some(Err(_)) => rsx! { p { class: "pg-status", "— see Validation" } },
                }
            }

            section { class: "pg-pane",
                h2 { "Validation" }
                match &*plan.read() {
                    None => rsx! { p { class: "pg-status", "checking…" } },
                    Some(Ok(sr)) => rsx! { ValidationView { result: sr.clone() } },
                    Some(Err(McpError::Rpc { message, .. })) => rsx! {
                        pre { class: "pg-error", "{message}" }
                    },
                    Some(Err(e)) => rsx! { pre { class: "pg-error", "{e}" } },
                }
            }
        }
    }
}

#[component]
fn SourceView(result: ScaffoldResult) -> Element {
    rsx! {
        if !result.would_create.is_empty() {
            details {
                summary { "would create {result.would_create.len()} files" }
                ul { class: "pg-tree",
                    for path in result.would_create.iter() {
                        li { "{path}" }
                    }
                }
            }
        }
        for (path , body) in result.previews.iter() {
            details { open: true,
                summary { "{path}" }
                pre { class: "pg-code", "{body}" }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Author,
    Inbox,
}

/// Root: a mode toggle between the Author playground and the Proposals inbox
/// (the human side of the M6 approval gate), plus a registry-driven theme
/// selector that recolors the whole cockpit by overriding the CSS custom
/// properties `main.css`/`preview.css` read.
#[component]
pub fn Cockpit() -> Element {
    let mut mode = use_signal(|| Mode::Inbox);
    // Re-fetch the registry whenever the window regains focus, so descriptors
    // edited outside the cockpit (the server hot-reloads them) show up without
    // a manual reload. The server load is cheap and `Registry: PartialEq` means
    // an unchanged re-fetch dedupes — no re-render or theme flicker. No idle
    // polling: it only fires on focus.
    let mut reg_tick = use_signal(|| 0u32);
    use_future(move || async move {
        let mut eval = document::eval(
            "window.addEventListener('focus', () => dioxus.send('focus'));",
        );
        while eval.recv::<String>().await.is_ok() {
            reg_tick += 1;
        }
    });
    // `None` while pending or on error — the cockpit keeps its default dark
    // theme until the registry arrives.
    let registry = use_resource(move || async move {
        let _ = reg_tick();
        mcp_client::get_registry().await.ok()
    });
    // Share the loaded registry with the preview panes so they can resolve a
    // doc's `theme:` to tokens. (Dedupes via Registry: PartialEq.)
    let reg_ctx = use_memo(move || registry.read().clone().flatten());
    use_context_provider(|| reg_ctx);
    let mut theme_id = use_signal(|| "dark".to_string());

    // Palette themes (those carrying color tokens) offered in the selector;
    // styling-family themes like tailwind/vanilla-css aren't cockpit palettes.
    let palettes = use_memo(move || match &*registry.read() {
        Some(Some(reg)) => reg
            .themes
            .values()
            .filter(|t| !t.tokens.color.is_empty())
            .map(|t| (t.id.clone(), t.label.clone()))
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    });

    // CSS for the active theme: `:root { --bg: …; }` overriding main.css's
    // defaults. Empty until the registry resolves / a palette is found.
    let theme_css = use_memo(move || match &*registry.read() {
        Some(Some(reg)) => reg.themes.get(&theme_id()).map(theme_vars_css).unwrap_or_default(),
        _ => String::new(),
    });

    rsx! {
        if !theme_css().is_empty() {
            document::Style { "{theme_css}" }
        }
        nav { class: "pg-modebar",
            button {
                class: if mode() == Mode::Author { "modetab active" } else { "modetab" },
                onclick: move |_| mode.set(Mode::Author),
                "Author"
            }
            button {
                class: if mode() == Mode::Inbox { "modetab active" } else { "modetab" },
                onclick: move |_| mode.set(Mode::Inbox),
                "Proposals"
            }
            if !palettes().is_empty() {
                select {
                    class: "theme-select",
                    value: "{theme_id}",
                    onchange: move |e| theme_id.set(e.value()),
                    for (id , label) in palettes() {
                        option { value: "{id}", "{label}" }
                    }
                }
            }
        }
        match mode() {
            Mode::Author => rsx! { Playground {} },
            Mode::Inbox => rsx! { ProposalsInbox {} },
        }
    }
}

/// Build `--p-*` custom-property declarations for the *preview* from a theme's
/// color tokens (scoped to `.preview-root`, distinct from the cockpit chrome's
/// `--*` vars). Empty when the theme id is absent/unknown, so the preview keeps
/// its default light card via the CSS fallbacks.
fn preview_theme_decls(
    reg: &Option<dioxus_mcp_registry::Registry>,
    theme_id: &Option<String>,
) -> String {
    let Some(reg) = reg else {
        return String::new();
    };
    let Some(id) = theme_id else {
        return String::new();
    };
    let Some(theme) = reg.themes.get(id) else {
        return String::new();
    };
    let c = &theme.tokens.color;
    let mut s = String::new();
    for (var, key) in [
        ("--p-bg", "bg"),
        ("--p-text", "text"),
        ("--p-border", "border"),
        ("--p-accent", "accent"),
        ("--p-surface", "panel"),
        ("--p-muted", "muted"),
    ] {
        if let Some(v) = c.get(key) {
            s.push_str(&format!("{var}: {v}; "));
        }
    }
    s
}

/// A "preview theme" selector: overrides the theme used to render the preview,
/// defaulting to the doc's own `theme:`. Shared by the Author and Inbox panes.
#[component]
fn PreviewThemePicker(
    reg: Memo<Option<dioxus_mcp_registry::Registry>>,
    selected: Signal<Option<String>>,
    doc_theme: Option<String>,
) -> Element {
    let palettes = use_memo(move || match &*reg.read() {
        Some(r) => r
            .themes
            .values()
            .filter(|t| !t.tokens.color.is_empty())
            .map(|t| (t.id.clone(), t.label.clone()))
            .collect::<Vec<_>>(),
        None => Vec::new(),
    });
    if palettes().is_empty() {
        return rsx! {};
    }
    let doc_label = doc_theme.clone().unwrap_or_else(|| "unstyled".into());
    rsx! {
        div { class: "preview-theme-bar",
            span { "preview theme:" }
            select {
                value: selected().unwrap_or_default(),
                onchange: move |e| {
                    let v = e.value();
                    selected.set(if v.is_empty() { None } else { Some(v) });
                },
                option { value: "", "from doc ({doc_label})" }
                for (id , label) in palettes() {
                    option { value: "{id}", "{label}" }
                }
            }
        }
    }
}

/// Emit `:root { --token: value; … }` from a theme's tokens. Colors map to
/// `--<name>` (matching the names main.css/preview.css read: bg/panel/border/
/// text/muted/accent/error/code); space/radius/font get prefixed vars.
fn theme_vars_css(theme: &dioxus_mcp_registry::ThemeDescriptor) -> String {
    let mut vars = String::new();
    for (k, v) in &theme.tokens.color {
        vars.push_str(&format!("--{k}: {v}; "));
    }
    for (k, v) in &theme.tokens.space {
        vars.push_str(&format!("--space-{k}: {v}; "));
    }
    for (k, v) in &theme.tokens.radius {
        vars.push_str(&format!("--radius-{k}: {v}; "));
    }
    for (k, v) in &theme.tokens.font {
        vars.push_str(&format!("--font-{k}: {v}; "));
    }
    format!(":root {{ {vars}}}")
}

/// The human side of the approval gate: poll pending proposals, review/edit the
/// DSL, Approve (round-trip the edit) or Reject.
#[component]
fn ProposalsInbox() -> Element {
    let mut tick = use_signal(|| 0u32);
    // Poll the inbox every 2s (scope-tied; cancelled when this view unmounts).
    use_future(move || async move {
        loop {
            gloo_timers::future::TimeoutFuture::new(2000).await;
            tick += 1;
        }
    });
    let proposals = use_resource(move || async move {
        let _ = tick();
        mcp_client::list_proposals().await
    });

    let mut selected = use_signal(|| None::<String>);
    let mut edit_text = use_signal(String::new);
    let mut msg = use_signal(|| None::<String>);
    // Render models from the selected proposal's stored dry-run. They reflect
    // the ORIGINAL proposal, not live edits to `edit_text` — fine for an
    // approximate review preview (a re-dry-run of the edit is a possible
    // follow-up).
    let mut sel_models = use_signal(Vec::<model::RenderModel>::new);
    let inbox_groups = use_memo(move || {
        let screens = model::parse_doc(&edit_text())
            .map(|d| d.screens)
            .unwrap_or_default();
        build_groups(&screens, &sel_models())
    });

    // Preview theming for the proposal under review: the edited doc's `theme:`,
    // overridable via the picker.
    let reg_ctx = use_context::<Memo<Option<dioxus_mcp_registry::Registry>>>();
    let preview_theme = use_signal(|| None::<String>);
    let doc_theme = use_memo(move || model::parse_doc(&edit_text()).ok().and_then(|d| d.theme));
    let active_theme = use_memo(move || preview_theme().or_else(|| doc_theme()));
    let preview_decls = use_memo(move || preview_theme_decls(&reg_ctx(), &active_theme()));

    rsx! {
        header { class: "pg-header",
            h1 { "Proposals" }
            p { "Scaffold proposals awaiting your approval. Edit the DSL before approving to round-trip your changes back to the agent." }
        }
        div { class: "pg-grid pg-grid-inbox",
            section { class: "pg-pane",
                h2 { "Inbox" }
                match &*proposals.read() {
                    None => rsx! { p { class: "pg-status", "loading…" } },
                    Some(Err(e)) => rsx! { pre { class: "pg-error", "{e}" } },
                    Some(Ok(list)) if list.is_empty() => rsx! {
                        p { class: "pg-status",
                            "No pending proposals. When an agent calls propose_scaffold against this server, it shows up here."
                        }
                    },
                    Some(Ok(list)) => rsx! {
                        for pr in list.iter() {
                            button {
                                key: "{pr.id}",
                                class: if selected() == Some(pr.id.clone()) { "inbox-item active" } else { "inbox-item" },
                                onclick: {
                                    let id = pr.id.clone();
                                    let code = pr.code.clone();
                                    let models = pr.preview.render_models.clone();
                                    move |_| {
                                        selected.set(Some(id.clone()));
                                        edit_text.set(code.clone());
                                        sel_models.set(models.clone());
                                        msg.set(None);
                                    }
                                },
                                div { class: "inbox-id", "{pr.id}" }
                                div { class: "inbox-files", "{pr.preview.would_create.len()} files would be created" }
                            }
                        }
                    },
                }
            }

            section { class: "pg-pane inbox-review",
                if let Some(pid) = selected() {
                    h2 { "Review" }
                    div { class: "compiled-controls",
                        button {
                            class: "apply-btn",
                            onclick: {
                                let pid = pid.clone();
                                move |_| {
                                    let pid = pid.clone();
                                    let code = edit_text.peek().clone();
                                    spawn(async move {
                                        let r = mcp_client::resolve_proposal(&pid, "approve", Some(&code), None).await;
                                        msg.set(Some(match r {
                                            Ok(v) => format!("approved → {}", v.get("status").and_then(|s| s.as_str()).unwrap_or("?")),
                                            Err(e) => format!("error: {e}"),
                                        }));
                                        selected.set(None);
                                        tick += 1;
                                    });
                                }
                            },
                            "Approve ▸"
                        }
                        button {
                            class: "reject-btn",
                            onclick: {
                                let pid = pid.clone();
                                move |_| {
                                    let pid = pid.clone();
                                    spawn(async move {
                                        let _ = mcp_client::resolve_proposal(&pid, "reject", None, None).await;
                                        msg.set(Some("rejected".into()));
                                        selected.set(None);
                                        tick += 1;
                                    });
                                }
                            },
                            "Reject"
                        }
                    }
                    h3 { class: "pg-subhead", "Preview of your edited DSL" }
                    if !preview_decls().is_empty() {
                        document::Style { ".preview-root {{ {preview_decls} }}" }
                    }
                    PreviewThemePicker { reg: reg_ctx, selected: preview_theme, doc_theme: doc_theme() }
                    ScreenNavigator { groups: inbox_groups }
                    h3 { class: "pg-subhead", "DSL" }
                    if let Err(e) = model::parse_doc(&edit_text()) {
                        div { class: "pg-parse-banner", "YAML: {e}" }
                    }
                    textarea {
                        class: "pg-editor pg-editor-short",
                        spellcheck: false,
                        value: "{edit_text}",
                        oninput: move |e| edit_text.set(e.value()),
                    }
                } else {
                    p { class: "pg-status", "Select a proposal on the left to review it." }
                }
                if let Some(m) = msg() {
                    p { class: "pg-status", "{m}" }
                }
            }
        }
    }
}

#[component]
fn ValidationView(result: ScaffoldResult) -> Element {
    rsx! {
        if result.collisions.is_empty() {
            p { class: "pg-ok", "✓ valid — would create {result.would_create.len()} files" }
        } else {
            div { class: "pg-warn",
                p { "Would collide with existing files:" }
                ul {
                    for c in result.collisions.iter() {
                        li { "{c}" }
                    }
                }
            }
        }
        if !result.next_steps.is_empty() {
            h3 { "Next steps" }
            ul {
                for step in result.next_steps.iter() {
                    li { "{step}" }
                }
            }
        }
    }
}
