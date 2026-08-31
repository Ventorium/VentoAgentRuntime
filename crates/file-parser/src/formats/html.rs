//! HTML → Markdown (`.html`, `.htm`, `.xhtml`).
//!
//! A faithful port of the original JS regex implementation
//! (fullstack `file2md/src/parsers/html.ts`): strip non-content blocks
//! (script/style/nav/footer/header/noscript, comments), extract the title
//! from `<title>`/`<h1>`, convert block and inline elements, then remove
//! remaining tags and decode entities. HTML emits Markdown directly, like
//! PDF; it has no document-model form.

use regex::Regex;
use std::sync::LazyLock;

use crate::error::ConvertError;

/// Convert an HTML document to Markdown. `fallback_title` (typically the
/// file stem) is used when neither `<title>` nor `<h1>` names the page.
pub fn to_markdown(bytes: &[u8], fallback_title: &str) -> Result<String, ConvertError> {
    let html = String::from_utf8_lossy(bytes);
    Ok(html_to_markdown(&html, fallback_title) + "\n")
}

fn html_to_markdown(html: &str, fallback_title: &str) -> String {
    // Remove non-content blocks (with their content) and comments.
    let content = strip_non_content(html);

    // Title: <title>, else the first <h1>, else the fallback.
    let title = TITLE
        .captures(&content)
        .or_else(|| H1.captures(&content))
        .map(|c| c[1].to_string())
        .unwrap_or_else(|| fallback_title.to_string());
    let title = strip_tags(&title).trim().to_string();

    // Body only, when the document marks one.
    let body = BODY
        .captures(&content)
        .map(|c| c[1].to_string())
        .unwrap_or_else(|| content.to_string());

    // Block-level elements. Content keeps its inline tags for the next step.
    let mut processed = replace_headings(&body);
    processed = P
        .replace_all(&processed, |c: &regex::Captures| format!("\n{}\n", &c[1]))
        .into_owned();
    processed = BR.replace_all(&processed, "\n").into_owned();
    processed = HR.replace_all(&processed, "\n---\n").into_owned();
    processed = LI
        .replace_all(&processed, |c: &regex::Captures| format!("- {}\n", &c[1]))
        .into_owned();
    processed = TR
        .replace_all(&processed, |c: &regex::Captures| {
            let row = &c[1];
            let cells: Vec<String> = TD
                .captures_iter(row)
                .map(|cell| {
                    let text = TAGS.replace_all(&cell[1], "").into_owned();
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        " ".to_string()
                    } else {
                        trimmed.to_string()
                    }
                })
                .collect();
            format!("| {} |\n", cells.join(" | "))
        })
        .into_owned();

    // Inline elements. `<b` and `<i` openings are whitespace-anchored so
    // they cannot swallow longer element names (`<blockquote>`, `<input>`);
    // the original enforced this with a backreference this regex engine
    // lacks.
    for regex in [&*STRONG, &*B] {
        processed = regex
            .replace_all(&processed, |c: &regex::Captures| {
                let inner = c.len().saturating_sub(1);
                format!("**{}**", &c[inner])
            })
            .into_owned();
    }
    for regex in [&*EM, &*I] {
        processed = regex
            .replace_all(&processed, |c: &regex::Captures| {
                let inner = c.len().saturating_sub(1);
                format!("*{}*", &c[inner])
            })
            .into_owned();
    }
    processed = A
        .replace_all(&processed, |c: &regex::Captures| {
            format!("[{}]({})", &c[2], &c[1])
        })
        .into_owned();
    processed = IMG_ALT
        .replace_all(&processed, |c: &regex::Captures| {
            format!("![{}]({})", &c[2], &c[1])
        })
        .into_owned();
    processed = IMG
        .replace_all(&processed, |c: &regex::Captures| format!("![]({})", &c[1]))
        .into_owned();

    // Remove all remaining tags and decode entities.
    let cleaned = strip_tags(&processed);

    // Tidy: trim line ends, collapse 3+ newlines, trim the document.
    let final_text = cleaned
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    let final_text = BLANKS.replace_all(&final_text, "\n\n");
    let final_text = final_text.trim();

    format!("# {title}\n\n{final_text}")
}

/// Drop the six non-content element kinds (with their content) and HTML
/// comments — one pattern per kind, matching the original's per-tag pass.
fn strip_non_content(html: &str) -> String {
    let mut out = html.to_string();
    for regex in [
        &*SCRIPT, &*STYLE, &*NAV, &*FOOTER, &*HEADER, &*NOSCRIPT, &*COMMENT,
    ] {
        out = regex.replace_all(&out, "").into_owned();
    }
    out
}

/// Replace `<h1>`..`<h6>` with `#`..`######` in the original's step order.
fn replace_headings(body: &str) -> String {
    let mut out = body.to_string();
    for (regex, marker) in [
        (&*H1, "#"),
        (&*H2, "##"),
        (&*H3, "###"),
        (&*H4, "####"),
        (&*H5, "#####"),
        (&*H6, "######"),
    ] {
        out = regex
            .replace_all(&out, |c: &regex::Captures| {
                let text = strip_tags(&c[1]).trim().to_string();
                format!("\n{marker} {text}\n")
            })
            .into_owned();
    }
    out
}

/// Remove all tags, then decode the common named entities and drop the
/// unknown ones — the same chain, in the same order, as the original.
fn strip_tags(text: &str) -> String {
    let no_tags = TAGS.replace_all(text, "").into_owned();
    let decoded = no_tags
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    NAMED_ENTITY.replace_all(&decoded, "").into_owned()
}

fn block(tag: &str) -> Regex {
    Regex::new(&format!(r"(?is)<{tag}\b[^>]*>.*?</{tag}\s*>")).expect("block pattern")
}

static SCRIPT: LazyLock<Regex> = LazyLock::new(|| block("script"));
static STYLE: LazyLock<Regex> = LazyLock::new(|| block("style"));
static NAV: LazyLock<Regex> = LazyLock::new(|| block("nav"));
static FOOTER: LazyLock<Regex> = LazyLock::new(|| block("footer"));
static HEADER: LazyLock<Regex> = LazyLock::new(|| block("header"));
static NOSCRIPT: LazyLock<Regex> = LazyLock::new(|| block("noscript"));
static COMMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<!--.*?-->").expect("comment pattern"));
static TITLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<title[^>]*>([^<]*)</title\s*>").expect("title pattern"));
static H1: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<h1[^>]*>(.*?)</h1\s*>").expect("h1 pattern"));
static H2: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<h2[^>]*>(.*?)</h2\s*>").expect("h2 pattern"));
static H3: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<h3[^>]*>(.*?)</h3\s*>").expect("h3 pattern"));
static H4: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<h4[^>]*>(.*?)</h4\s*>").expect("h4 pattern"));
static H5: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<h5[^>]*>(.*?)</h5\s*>").expect("h5 pattern"));
static H6: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<h6[^>]*>(.*?)</h6\s*>").expect("h6 pattern"));
static BODY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<body[^>]*>(.*?)</body\s*>").expect("body pattern"));
static P: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<p[^>]*>(.*?)</p\s*>").expect("p pattern"));
static BR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<br\s*/?>").expect("br pattern"));
static HR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<hr\s*/?>").expect("hr pattern"));
static LI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<li[^>]*>(.*?)</li\s*>").expect("li pattern"));
static TR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<tr[^>]*>(.*?)</tr\s*>").expect("tr pattern"));
static TD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<t[dh][^>]*>(.*?)</t[dh]\s*>").expect("td pattern"));
static STRONG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<strong\b[^>]*>(.*?)</strong\s*>").expect("strong"));
static B: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<b(?:\s[^>]*)?>(.*?)</b\s*>").expect("b pattern"));
static EM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<em\b[^>]*>(.*?)</em\s*>").expect("em pattern"));
static I: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<i(?:\s[^>]*)?>(.*?)</i\s*>").expect("i pattern"));
static A: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<a\b[^>]*href="([^"]*)"[^>]*>(.*?)</a\s*>"#).expect("a pattern")
});
static IMG_ALT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<img\b[^>]*src="([^"]*)"[^>]*alt="([^"]*)"[^>]*/?>"#).expect("img+alt")
});
static IMG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)<img\b[^>]*src="([^"]*)"[^>]*/?>"#).expect("img"));
static TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").expect("tags pattern"));
static NAMED_ENTITY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"&[a-zA-Z]+;").expect("entity pattern"));
static BLANKS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\n{3,}").expect("blank-lines pattern"));

#[cfg(test)]
mod tests {
    use super::*;

    fn md(html: &str) -> String {
        to_markdown(html.as_bytes(), "fallback").unwrap()
    }

    #[test]
    fn converts_basic_html_to_markdown() {
        let html = r#"<!DOCTYPE html>
<html>
<head><title>Test Page</title></head>
<body>
  <h1>Main Title</h1>
  <p>Hello <strong>world</strong> and <em>everyone</em>.</p>
  <h2>Section</h2>
  <p>Some <a href="https://example.com">link</a> here.</p>
  <ul>
    <li>Item 1</li>
    <li>Item 2</li>
  </ul>
</body>
</html>"#;
        let out = md(html);
        assert!(out.contains("# Main Title"), "{out}");
        assert!(out.contains("**world**"), "{out}");
        assert!(out.contains("*everyone*"), "{out}");
        assert!(out.contains("## Section"), "{out}");
        assert!(out.contains("[link](https://example.com)"), "{out}");
        assert!(out.contains("- Item 1"), "{out}");
        assert!(out.contains("- Item 2"), "{out}");
        assert!(!out.contains("<h1>"), "{out}");
        assert!(!out.contains("<strong>"), "{out}");
    }

    #[test]
    fn strips_script_and_style_tags() {
        let html = r#"<html>
<head><style>body { color: red; }</style></head>
<body>
  <script>alert('xss')</script>
  <p>Content here</p>
</body></html>"#;
        let out = md(html);
        assert!(!out.contains("alert"), "{out}");
        assert!(!out.contains("color: red"), "{out}");
        assert!(out.contains("Content here"), "{out}");
    }

    #[test]
    fn handles_tables() {
        let html = r#"<html><body>
      <table>
        <tr><th>Name</th><th>Age</th></tr>
        <tr><td>Alice</td><td>30</td></tr>
        <tr><td>Bob</td><td>25</td></tr>
      </table>
    </body></html>"#;
        let out = md(html);
        assert!(out.contains("| Name | Age |"), "{out}");
        assert!(out.contains("| Alice | 30 |"), "{out}");
    }

    #[test]
    fn handles_images_with_alt() {
        let out = md(r#"<html><body><img src="photo.jpg" alt="A photo"/></body></html>"#);
        assert!(out.contains("![A photo](photo.jpg)"), "{out}");
    }

    #[test]
    fn handles_images_without_alt() {
        let out = md(r#"<html><body><img src="pic.png"/></body></html>"#);
        assert!(out.contains("![](pic.png)"), "{out}");
    }

    #[test]
    fn handles_nested_inline_formatting() {
        let out =
            md(r#"<html><body><p><strong>bold <em>and italic</em></strong></p></body></html>"#);
        assert!(out.contains("**bold"), "{out}");
        assert!(out.contains("*and italic*"), "{out}");
    }

    #[test]
    fn handles_multiple_heading_levels() {
        let out = md(
            "<html><body>\n<h1>H1</h1><h2>H2</h2><h3>H3</h3><h4>H4</h4><h5>H5</h5><h6>H6</h6>\n</body></html>",
        );
        for level in [
            "# H1",
            "## H2",
            "### H3",
            "#### H4",
            "##### H5",
            "###### H6",
        ] {
            assert!(out.contains(level), "missing {level} in {out}");
        }
    }

    #[test]
    fn strips_nav_header_footer_noscript() {
        let html = r#"<html>
<header>Site Header</header>
<nav>Navigation Menu</nav>
<body><p>Main Content</p></body>
<footer>Site Footer</footer>
<noscript>JS Required</noscript>
</html>"#;
        let out = md(html);
        assert!(out.contains("Main Content"), "{out}");
        assert!(!out.contains("Site Header"), "{out}");
        assert!(!out.contains("Navigation Menu"), "{out}");
        assert!(!out.contains("Site Footer"), "{out}");
        assert!(!out.contains("JS Required"), "{out}");
    }

    #[test]
    fn strips_html_comments() {
        let html = "<html><body>\n<p>Visible</p>\n<!-- This is a comment -->\n<p>Also visible</p>\n</body></html>";
        let out = md(html);
        assert!(out.contains("Visible"), "{out}");
        assert!(!out.contains("This is a comment"), "{out}");
    }

    #[test]
    fn handles_horizontal_rules() {
        let out = md(r#"<html><body><p>A</p><hr/><p>B</p></body></html>"#);
        assert!(out.contains("---"), "{out}");
    }

    #[test]
    fn handles_lists() {
        let out =
            md("<html><body>\n<ul><li>Unordered 1</li><li>Unordered 2</li></ul>\n</body></html>");
        assert!(out.contains("- Unordered 1"), "{out}");
        assert!(out.contains("- Unordered 2"), "{out}");
    }

    #[test]
    fn uses_title_tag_as_heading_when_no_h1() {
        let out =
            md(r#"<html><head><title>Page Title</title></head><body><p>Content</p></body></html>"#);
        assert!(out.contains("# Page Title"), "{out}");
    }

    #[test]
    fn falls_back_to_the_provided_title() {
        let out = to_markdown(b"<html><body><p>text</p></body></html>", "named-file").unwrap();
        assert!(out.contains("# named-file"), "{out}");
    }

    #[test]
    fn decodes_entities() {
        let out = md(
            r#"<html><body><p>a &amp; b &lt;c&gt; &quot;d&quot; &#39;e&#39; &nbsp;f</p></body></html>"#,
        );
        // `&nbsp;` decodes to a plain space, coalescing with the one before
        // it — the tidy pass only trims line ends, never inline runs.
        assert!(out.contains("a & b <c> \"d\" 'e'  f"), "{out}");
    }

    #[test]
    fn drops_unknown_named_entities() {
        let out = md(r#"<html><body><p>&copy; 2024</p></body></html>"#);
        assert!(out.contains("2024"), "{out}");
        assert!(!out.contains("&copy;"), "{out}");
    }

    #[test]
    fn extension_routes_html_family() {
        use crate::Format;
        for ext in ["html", "htm", "xhtml", "HTML"] {
            assert_eq!(Format::from_extension(ext), Some(Format::Html));
        }
        assert_eq!(Format::from_extension("txt"), None);
    }
}
