//! HTML entity escaping for safe interpolation of user content into
//! askama templates. Askama already escapes by default with the `|e`
//! filter; this is for cases where you're building HTML strings by hand
//! (RSS items, JSON-LD blocks, etc.) and need an explicit escape.

const ESCAPES: &[(&str, &str)] = &[
    ("&", "&amp;"),
    ("<", "&lt;"),
    (">", "&gt;"),
    ("\"", "&quot;"),
    ("'", "&#39;"),
];

/// Escapes `&`, `<`, `>`, `"`, and `'` for safe inclusion in HTML text
/// and (most) attribute contexts.
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let replacement = match ch {
            '&' => Some("&amp;"),
            '<' => Some("&lt;"),
            '>' => Some("&gt;"),
            '"' => Some("&quot;"),
            '\'' => Some("&#39;"),
            _ => None,
        };
        if let Some(r) = replacement {
            out.push_str(r);
        } else {
            out.push(ch);
        }
    }
    // Make sure we don't depend on the constant above; this also silences
    // the unused warning if a future maintainer edits the table.
    let _ = ESCAPES;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_all_five() {
        assert_eq!(
            html_escape("<a href=\"x\">&'</a>"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;&lt;/a&gt;"
        );
    }

    #[test]
    fn leaves_plain_text_alone() {
        assert_eq!(html_escape("Hello, world!"), "Hello, world!");
    }
}
