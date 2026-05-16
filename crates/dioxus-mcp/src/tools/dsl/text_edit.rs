use std::path::Path;

pub(super) fn relative_to_crate(crate_root: &Path, path: &Path) -> String {
    path.strip_prefix(crate_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// 1-based line number for a byte offset in `text`. Returns 1 when `offset`
/// is past the end of the file (avoids zero/negative indices in messages).
pub(super) fn app_line_number(text: &str, offset: usize) -> usize {
    if offset >= text.len() {
        return 1;
    }
    text[..offset].bytes().filter(|b| *b == b'\n').count() + 1
}

/// Find the byte range of `fn {name}(...)`'s outer body braces. The returned
/// `start` is the byte index of the opening `{`, and `end` is the index of
/// the matching `}` (i.e. `text[start..=end]` covers the whole body). Returns
/// None when the function isn't found or the braces don't balance.
pub(super) fn find_fn_body_range(text: &str, name: &str) -> Option<std::ops::Range<usize>> {
    let needle = format!("fn {name}(");
    let fn_idx = text.find(&needle)?;
    // From there, the next `{` opens the body.
    let open_rel = text[fn_idx..].find('{')?;
    let open = fn_idx + open_rel;
    let close = match_brace(text, open)?;
    Some(open..close)
}

/// Within a function body range (start = `{`, end = `}`), find the inner
/// brace range of the first `rsx! { ... }` macro call. Returns `start..end`
/// where start is the `{` after `rsx!` and end is the matching `}`.
pub(super) fn find_rsx_inner_range(
    text: &str,
    body: std::ops::Range<usize>,
) -> Option<std::ops::Range<usize>> {
    let slice = &text[body.start..body.end];
    let rsx_rel = slice.find("rsx!")?;
    let after_macro = body.start + rsx_rel + "rsx!".len();
    // Skip whitespace, then expect `{`.
    let bytes = text.as_bytes();
    let mut i = after_macro;
    while i < text.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= text.len() || bytes[i] != b'{' {
        return None;
    }
    let open = i;
    let close = match_brace(text, open)?;
    Some(open..close)
}

/// Given the byte index of `{`, return the index of its matching `}`.
/// Counts nested braces but does NOT skip strings / chars / comments —
/// fine for our targeted use (App body / rsx! inner block) where odd brace
/// counts in string literals would be unusual.
pub(super) fn match_brace(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.get(open).copied() != Some(b'{') {
        return None;
    }
    let mut depth: i32 = 0;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Indent of the first non-empty line inside a `{ ... }` body, or None if
/// the body has no body lines yet. `range.start` is the `{`, `range.end` is
/// the `}` byte index.
pub(super) fn detect_body_indent(text: &str, range: std::ops::Range<usize>) -> Option<String> {
    let body = &text[range.start + 1..range.end];
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let lead: String = line.chars().take_while(|c| c.is_whitespace()).collect();
        if !lead.is_empty() {
            return Some(lead);
        }
    }
    None
}

/// Same as detect_body_indent but for an rsx body — we want children to line
/// up with whatever's already in the rsx block.
pub(super) fn detect_rsx_indent(text: &str, range: std::ops::Range<usize>) -> Option<String> {
    detect_body_indent(text, range)
}
