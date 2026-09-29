//! Markdown to HTML that is safe to print with `{!! !!}`, for text written by
//! anyone. Enabled by the `markdown` feature (pulldown-cmark).

use pulldown_cmark::{CowStr, Event, Parser, Tag, html};

use super::form::url_scheme;

/// `markdown` as HTML. HTML written in the Markdown shows as text, and a link
/// or image whose scheme is not `http`, `https` or `mailto` (such as
/// `javascript:` or `data:`) points to `#`. Relative links are kept.
///
/// ```
/// use rustclamp::web::markdown;
///
/// let html = markdown::to_html("**Hi** <script>x</script> [a](javascript:alert(1))");
/// assert_eq!(
///     html,
///     "<p><strong>Hi</strong> &lt;script&gt;x&lt;/script&gt; <a href=\"#\">a</a></p>\n"
/// );
/// ```
pub fn to_html(markdown: &str) -> String {
    let events = Parser::new(markdown).map(|event| match event {
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: safe(dest_url),
            title,
            id,
        }),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: safe(dest_url),
            title,
            id,
        }),
        other => other,
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

/// `url` when it is relative or uses a web scheme, else `#`.
fn safe(url: CowStr<'_>) -> CowStr<'_> {
    match url_scheme(&url).as_deref() {
        None | Some("http" | "https" | "mailto") => url,
        Some(_) => CowStr::Borrowed("#"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_cannot_carry_script() {
        let html = to_html(
            "<script>alert(1)</script>\n\nHi <img src=x onerror=alert(1)> \
             [a](javascript:alert(1)) [b](JAVASCRIPT:x) [c](%20javascript:x) \
             ![d](data:image/svg+xml,x) [ok](https://rustclamp.com) [rel](/blog) [m](mailto:a@b.si)",
        );
        assert!(
            !html.contains("<script") && !html.contains("<img src=x"),
            "{html}"
        );
        assert!(
            html.contains("&lt;script&gt;"),
            "raw HTML shows as text: {html}"
        );
        assert!(
            !html.to_lowercase().contains("javascript:") && !html.contains("data:"),
            "{html}"
        );
        for kept in [
            r#"href="https://rustclamp.com""#,
            r#"href="/blog""#,
            r#"href="mailto:a@b.si""#,
        ] {
            assert!(html.contains(kept), "{kept} in {html}");
        }
    }
}
