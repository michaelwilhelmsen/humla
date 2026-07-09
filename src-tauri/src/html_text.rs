//! Minimal Tiptap-HTML → plain-text conversion, shared across the app.
//!
//! Not a full HTML parser — it handles only the small set of tags the Tiptap
//! editor emits (paragraphs, line breaks, headings, list items, blockquotes)
//! and unescapes the common entities. Originally private to the summary
//! command; promoted here so the export path (issue #18) can reuse the exact
//! same shaping the summarizer sees, rather than duplicating it.

/// Strip Tiptap-emitted HTML to plain text, preserving paragraph and list
/// structure. Collapses runs of 3+ newlines to 2 so paragraph breaks survive
/// without unbounded whitespace, and trims the result.
pub fn html_to_text(html: &str) -> String {
    let s = html
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("</p>", "\n")
        .replace("</li>", "\n")
        .replace("</h1>", "\n")
        .replace("</h2>", "\n")
        .replace("</h3>", "\n")
        .replace("</h4>", "\n")
        .replace("</blockquote>", "\n")
        .replace("<li>", "- ");
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    // Collapse runs of 3+ newlines down to 2 so paragraph breaks survive
    // but downstream consumers don't see endless whitespace.
    let mut collapsed = String::with_capacity(out.len());
    let mut nl = 0;
    for c in out.chars() {
        if c == '\n' {
            nl += 1;
            if nl <= 2 {
                collapsed.push(c);
            }
        } else {
            nl = 0;
            collapsed.push(c);
        }
    }
    collapsed.trim().to_string()
}
