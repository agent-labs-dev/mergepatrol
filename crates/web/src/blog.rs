//! Articles at `/blog/<slug>`, rendered on the server from `content/blog/<slug>.md`.
//!
//! The dashboard is a single-page app, so every other URL returns the same `index.html`. A crawler
//! that runs no JavaScript sees each article only if the server sends it as its own HTML page with
//! its own title, description and canonical link. That is what this module does.

use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use pulldown_cmark::{Options, Parser};

#[derive(rust_embed::RustEmbed)]
#[folder = "../../content/blog"]
#[allow_missing = true]
struct Articles;

#[derive(Debug, PartialEq)]
pub struct Article {
    pub slug: String,
    pub title: String,
    pub description: String,
    /// `YYYY-MM-DD`, the publish date.
    pub date: String,
    /// `YYYY-MM-DD`, set when the article changed after it was published.
    pub updated: Option<String>,
    pub body: String,
}

impl Article {
    fn last_modified(&self) -> &str {
        self.updated.as_deref().unwrap_or(&self.date)
    }
}

/// The PostHog project key and ingestion host, when product analytics are on.
pub type Analytics<'a> = Option<(&'a str, &'a str)>;

/// Answers `/blog` and `/blog/<slug>`. `None` for any other path, so the caller serves the app.
pub fn serve(path: &str, public_url: &str, analytics: Analytics) -> Option<Response> {
    let rest = path.strip_prefix("blog")?;
    let slug = match rest.trim_end_matches('/') {
        "" => return Some(Html(index_page(&articles(), public_url, analytics)).into_response()),
        s => s.strip_prefix('/')?,
    };
    if slug.contains('/') {
        return Some(StatusCode::NOT_FOUND.into_response());
    }
    let Some(article) = load(slug) else {
        return Some(StatusCode::NOT_FOUND.into_response());
    };
    Some(
        (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            article_page(&article, public_url, analytics),
        )
            .into_response(),
    )
}

/// Every published article, newest first. A malformed file is logged and left out; the
/// `every_article_parses` test keeps one from reaching `main`.
pub fn articles() -> Vec<Article> {
    let mut all: Vec<Article> = slugs().filter_map(|slug| load(&slug)).collect();
    all.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| a.slug.cmp(&b.slug)));
    all
}

/// The file names of the articles, without `.md`. The folder's README is not one.
fn slugs() -> impl Iterator<Item = String> {
    Articles::iter().filter_map(|file| {
        let slug = file.strip_suffix(".md")?;
        (slug != "README").then(|| slug.to_owned())
    })
}

fn load(slug: &str) -> Option<Article> {
    if slug == "README" {
        return None;
    }
    let file = Articles::get(&format!("{slug}.md"))?;
    let source = String::from_utf8_lossy(&file.data);
    match parse(slug, &source) {
        Ok(article) => Some(article),
        Err(e) => {
            tracing::error!(slug, error = %e, "blog article is malformed, not serving it");
            None
        }
    }
}

/// Reads a `---`-fenced header of `key: value` lines (title, description, date, optional
/// updated), then the Markdown body.
pub fn parse(slug: &str, source: &str) -> Result<Article, String> {
    if slug.is_empty()
        || !slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(format!(
            "slug {slug:?} must be lowercase letters, digits and hyphens"
        ));
    }
    let rest = source
        .strip_prefix("---\n")
        .ok_or("must start with a --- header")?;
    let (header, body) = rest
        .split_once("\n---\n")
        .ok_or("the --- header is not closed")?;
    let (mut title, mut description, mut date, mut updated) = (None, None, None, None);
    for line in header.lines().filter(|l| !l.trim().is_empty()) {
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| format!("header line {line:?} is not `key: value`"))?;
        let value = value.trim().to_owned();
        let slot = match key.trim() {
            "title" => &mut title,
            "description" => &mut description,
            "date" => &mut date,
            "updated" => &mut updated,
            other => return Err(format!("unknown header key {other:?}")),
        };
        if value.is_empty() {
            return Err(format!("header key {:?} is empty", key.trim()));
        }
        *slot = Some(value);
    }
    let date = date.ok_or("missing date")?;
    for d in std::iter::once(&date).chain(updated.as_ref()) {
        if !is_iso_date(d) {
            return Err(format!("date {d:?} is not YYYY-MM-DD"));
        }
    }
    Ok(Article {
        slug: slug.to_owned(),
        title: title.ok_or("missing title")?,
        description: description.ok_or("missing description")?,
        date,
        updated,
        body: body.to_owned(),
    })
}

fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// `<url>` entries for the sitemap: the index, then each article.
pub fn sitemap_entries(public_url: &str) -> String {
    let all = articles();
    // The index changes whenever an article does, so its lastmod is the newest article's.
    let mut out = match all.iter().map(Article::last_modified).max() {
        Some(newest) => {
            format!("  <url><loc>{public_url}/blog</loc><lastmod>{newest}</lastmod></url>\n")
        }
        None => format!("  <url><loc>{public_url}/blog</loc></url>\n"),
    };
    for a in &all {
        out.push_str(&format!(
            "  <url><loc>{public_url}/blog/{}</loc><lastmod>{}</lastmod></url>\n",
            a.slug,
            a.last_modified()
        ));
    }
    out
}

fn article_page(a: &Article, public_url: &str, analytics: Analytics) -> String {
    let mut body = String::new();
    pulldown_cmark::html::push_html(&mut body, Parser::new_ext(&a.body, Options::all()));
    let url = format!("{public_url}/blog/{}", a.slug);
    // The articles are written by the team, not signed by a person, so the site is the author too.
    let publisher = serde_json::json!({
        "@type": "Organization",
        "name": "Mergepatrol",
        "url": format!("{public_url}/"),
        "logo": { "@type": "ImageObject", "url": format!("{public_url}/apple-touch-icon.png") },
    });
    let json_ld = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "BlogPosting",
        "headline": a.title,
        "description": a.description,
        "datePublished": a.date,
        "dateModified": a.last_modified(),
        "url": url,
        "image": format!("{public_url}/og/mergepatrol-og-1200x630.png"),
        "mainEntityOfPage": url,
        "author": publisher.clone(),
        "publisher": publisher,
    });
    let head = format!(
        "<meta property=\"og:type\" content=\"article\" />\n\
         <meta property=\"article:published_time\" content=\"{}\" />\n\
         <script type=\"application/ld+json\">{}</script>",
        a.date,
        json_ld.to_string().replace("</", "<\\/"),
    );
    let main = format!(
        "<article><h1>{}</h1><p class=\"meta\"><time datetime=\"{}\">{}</time></p>{body}</article>",
        escape(&a.title),
        a.date,
        a.date,
    );
    page(
        &a.title,
        &a.description,
        &url,
        &head,
        &main,
        public_url,
        analytics,
    )
}

fn index_page(all: &[Article], public_url: &str, analytics: Analytics) -> String {
    let mut list = String::from("<h1>Blog</h1><ul class=\"posts\">");
    for a in all {
        list.push_str(&format!(
            "<li><a href=\"/blog/{}\">{}</a><p>{}</p><time datetime=\"{}\">{}</time></li>",
            a.slug,
            escape(&a.title),
            escape(&a.description),
            a.date,
            a.date
        ));
    }
    list.push_str("</ul>");
    page(
        "Blog",
        "Merge queues, rebases and CI, from the people who build Mergepatrol.",
        &format!("{public_url}/blog"),
        "<meta property=\"og:type\" content=\"website\" />",
        &list,
        public_url,
        analytics,
    )
}

/// The public pages' shell: the blog's and the changelog's. `head` goes at the end of `<head>`.
pub(crate) fn page(
    title: &str,
    description: &str,
    url: &str,
    head: &str,
    main: &str,
    public_url: &str,
    analytics: Analytics,
) -> String {
    let (title, description) = (
        escape(&format!("{title} | Mergepatrol")),
        escape(description),
    );
    let image = format!("{public_url}/og/mergepatrol-og-1200x630.png");
    let tracking = analytics.map(tracking).unwrap_or_default();
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="UTF-8" />
<meta name="viewport" content="width=device-width, initial-scale=1.0" />
<title>{title}</title>
<meta name="description" content="{description}" />
<link rel="canonical" href="{url}" />
<meta property="og:site_name" content="Mergepatrol" />
<meta property="og:url" content="{url}" />
<meta property="og:title" content="{title}" />
<meta property="og:description" content="{description}" />
<meta property="og:image" content="{image}" />
<meta property="og:image:width" content="1200" />
<meta property="og:image:height" content="630" />
<meta name="twitter:card" content="summary_large_image" />
<meta name="twitter:title" content="{title}" />
<meta name="twitter:description" content="{description}" />
<meta name="twitter:image" content="{image}" />
{head}
<link rel="icon" type="image/svg+xml" href="/favicon.svg" />
<link rel="icon" href="/favicon.ico" sizes="32x32" />
<link rel="apple-touch-icon" href="/apple-touch-icon.png" />
<style>{STYLE}{FOOTER_STYLE}</style>
{tracking}
</head>
<body>
<header><a href="/" class="brand"><img src="/logo.svg" alt="" width="20" height="20" />Mergepatrol</a><nav><a href="/blog">Blog</a><a href="/changelog">Changelog</a></nav></header>
<main>{main}</main>
{FOOTER}
</body>
</html>
"#
    )
}

/// The landing page's footer, shared with the dashboard's build so the two cannot drift.
const FOOTER: &str = include_str!("../../../ui/src/footer.html");
const FOOTER_STYLE: &str = include_str!("../../../ui/src/footer.css");

const STYLE: &str = "\
:root{color-scheme:light dark;--bg:#fafafa;--fg:#171717;--fg2:#525252;--line:#e5e5e5;--sunken:#f5f5f5}\
@media(prefers-color-scheme:dark){:root{--bg:#0a0a0a;--fg:#fafafa;--fg2:#a3a3a3;--line:#262626;--sunken:#171717}}\
body{margin:0;background:var(--bg);color:var(--fg);font:16px/1.7 ui-sans-serif,system-ui,sans-serif}\
header,main{max-width:42rem;margin:0 auto;padding:0 1.25rem}\
header{display:flex;justify-content:space-between;align-items:center;height:3.5rem;border-bottom:1px solid var(--line)}\
.brand{display:flex;gap:.5rem;align-items:center;font-weight:600}nav{display:flex;gap:1rem}\
a{color:inherit}header a{text-decoration:none}\
main{padding-top:2rem;padding-bottom:3rem}h1,h2,h3{font-weight:600;letter-spacing:-.02em;line-height:1.25}h1{font-size:2rem;margin:0 0 .5rem}\
.meta,time,.posts p{color:var(--fg2);font-size:.875rem}\
.posts{list-style:none;padding:0}.posts li{padding:1rem 0;border-bottom:1px solid var(--line)}\
.posts a{font-weight:600;font-size:1.125rem}.posts p{margin:.25rem 0}\
pre{background:var(--sunken);padding:1rem;overflow-x:auto;border-radius:.5rem}\
code{font:.875em ui-monospace,monospace}img{max-width:100%}\
table{border-collapse:collapse}td,th{border:1px solid var(--line);padding:.25rem .5rem}";

/// PostHog's browser library, configured as `ui/src/analytics.ts` configures the dashboard's. The
/// articles are plain HTML with no app bundle, so without this a reader who arrives from a search
/// or a share is invisible until they reach the landing page. Both write the same first-party
/// cookie, so a reader keeps one anonymous id from article to landing page to sign-in, where
/// `identify` joins it to their GitHub login. Persons are made only at sign-in
/// (`identified_only`), so an anonymous reader stays a bare id with no profile.
fn tracking((key, host): (&str, &str)) -> String {
    // PostHog Cloud serves the library from a separate assets host; a self-hosted PostHog from
    // its own.
    let assets = match host.strip_suffix(".i.posthog.com") {
        Some(region) => format!("{region}-assets.i.posthog.com"),
        None => host.to_owned(),
    };
    let config = serde_json::json!({
        "api_host": host,
        "defaults": "2026-08-30",
        "person_profiles": "identified_only",
    });
    format!(
        "<script async src=\"{}/static/array.js\" onload=\"posthog.init({}, {})\"></script>",
        escape(&assets),
        escape(&serde_json::json!(key).to_string()),
        escape(&config.to_string()),
    )
}

pub(crate) fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_article_parses() {
        for slug in slugs() {
            let data = Articles::get(&format!("{slug}.md")).unwrap().data;
            if let Err(e) = parse(&slug, &String::from_utf8_lossy(&data)) {
                panic!("content/blog/{slug}.md: {e}");
            }
        }
    }

    #[test]
    fn parses_header_and_rejects_malformed() {
        let ok = "---\ntitle: A: B\ndescription: D\ndate: 2026-10-30\n---\n# Hi\n";
        let a = parse("a-b", ok).unwrap();
        assert_eq!(
            (a.title.as_str(), a.date.as_str(), a.body.as_str()),
            ("A: B", "2026-10-30", "# Hi\n")
        );
        for (slug, bad) in [
            ("a", "title: T\n"),
            ("a", "---\ntitle: T\ndescription: D\n---\n"),
            (
                "a",
                "---\ntitle: T\ndescription: D\ndate: 30/10/2026\n---\n",
            ),
            (
                "a",
                "---\ntitle: T\ndescription: D\ndate: 2026-10-30\nauthor: X\n---\n",
            ),
            ("A_b", ok),
        ] {
            assert!(
                parse(slug, bad).is_err(),
                "{slug} {bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn page_escapes_header_fields() {
        let a = parse(
            "x",
            "---\ntitle: <b>\"T\"\ndescription: D\ndate: 2026-10-30\n---\nbody\n",
        )
        .unwrap();
        let html = article_page(&a, "https://mergepatrol.ai", None);
        assert!(html.contains("<title>&lt;b&gt;&quot;T&quot; | Mergepatrol</title>"));
        assert!(html.contains("<link rel=\"canonical\" href=\"https://mergepatrol.ai/blog/x\" />"));
    }

    #[test]
    fn structured_data_names_author_and_publisher() {
        let a = parse(
            "x",
            "---\ntitle: T\ndescription: D\ndate: 2026-10-30\n---\nbody\n",
        )
        .unwrap();
        let html = article_page(&a, "https://mergepatrol.ai", None);
        let json = html
            .split("<script type=\"application/ld+json\">")
            .nth(1)
            .and_then(|rest| rest.split("</script>").next())
            .unwrap();
        let ld: serde_json::Value = serde_json::from_str(json).unwrap();
        for key in ["author", "publisher"] {
            assert_eq!(ld[key]["name"], "Mergepatrol", "{key} in {json}");
        }
        assert_eq!(ld["datePublished"], "2026-10-30");
    }

    #[test]
    fn loads_posthog_only_when_analytics_are_on() {
        let a = parse(
            "x",
            "---\ntitle: T\ndescription: D\ndate: 2026-10-30\n---\nbody\n",
        )
        .unwrap();
        assert!(!article_page(&a, "https://mergepatrol.ai", None).contains("posthog"));
        for (host, script) in [
            (
                "https://us.i.posthog.com",
                "https://us-assets.i.posthog.com/static/array.js",
            ),
            (
                "https://ph.example.com",
                "https://ph.example.com/static/array.js",
            ),
        ] {
            let html = article_page(&a, "https://mergepatrol.ai", Some(("phc_x", host)));
            assert!(html.contains(&format!("src=\"{script}\"")), "{html}");
            assert!(html.contains("posthog.init(&quot;phc_x&quot;"), "{html}");
        }
    }
}
