//! The docs at `/docs/<slug>`, cut from the README's sections, so the site and the README are one
//! text and cannot drift. A page is a list of the README's `##` headings; links between sections
//! point at the page that holds the section, and links to files point at them on GitHub.

use axum::http::header;
use axum::response::{Html, IntoResponse, Response};
use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd};

use crate::blog::{Analytics, escape, page};

const README: &str = include_str!("../../../README.md");
const REPO: &str = "https://github.com/agent-labs-dev/mergepatrol";

pub struct Doc {
    pub slug: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// The README's `##` headings this page shows, in order, each with its `###` subsections.
    sections: &'static [&'static str],
}

/// The docs, in reading order. A README section left out of every page stays on GitHub only.
pub const DOCS: &[Doc] = &[
    Doc {
        slug: "getting-started",
        title: "Get started with Mergepatrol",
        description: "Install the GitHub App, merge the setup PR, add a model key and label a pull request: a merge queue for GitHub in five steps.",
        sections: &["Try it"],
    },
    Doc {
        slug: "how-it-works",
        title: "How the merge queue works",
        description: "What a queued PR needs, the agent that resolves conflicts and fixes failed checks, automatic review, branches, batches, stacks and PRs from forks.",
        sections: &["How it works"],
    },
    Doc {
        slug: "configuration",
        title: "Configuration: .mergepatrol/mergepatrol.yml",
        description: "Every setting in a repo's .mergepatrol/mergepatrol.yml: the label, batch size, the agent's model, verify commands, CI fixes, extra branches and review bars.",
        sections: &["Repo settings"],
    },
    Doc {
        slug: "pricing",
        title: "Pricing",
        description: "Mergepatrol is free on public repos. Private repos cost $15 a seat a month after a 14-day trial with no card. Self-hosting is free.",
        sections: &["Pricing"],
    },
    Doc {
        slug: "mcp-and-api",
        title: "MCP server and API",
        description: "Let Claude Code, Codex or Cursor queue and review PRs through Mergepatrol's MCP server, or read a PR's queue state from the JSON API.",
        sections: &[
            "From an agent, or the API",
            "Read a PR's state from another app",
        ],
    },
    Doc {
        slug: "self-hosting",
        title: "Self-host Mergepatrol",
        description: "Run the open-source Mergepatrol yourself with Docker Compose, configure it, bring your own GitHub App, and isolate its jobs in gVisor sandboxes.",
        sections: &[
            "Run it",
            "Run it with an App you made yourself",
            "How jobs are isolated",
        ],
    },
];

/// Answers `/docs` and `/docs/<slug>` for a doc that exists. `None` for any other path, so the
/// caller serves the app with a 404, as the blog's collections do.
pub fn serve(path: &str, public_url: &str, analytics: Analytics) -> Option<Response> {
    let rest = path.strip_prefix("docs")?;
    let slug = match rest.trim_end_matches('/') {
        "" => return Some(Html(index_page(public_url, analytics)).into_response()),
        s => s.strip_prefix('/')?,
    };
    let doc = DOCS.iter().find(|d| d.slug == slug)?;
    Some(
        (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            doc_page(doc, public_url, analytics),
        )
            .into_response(),
    )
}

/// The README's `##` sections, by heading, each with its body up to the next `##`.
fn sections() -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    let mut fenced = false;
    let mut start: Option<(&str, usize)> = None;
    let mut offset = 0;
    for line in README.split_inclusive('\n') {
        if line.starts_with("```") {
            fenced = !fenced;
        }
        if !fenced && let Some(heading) = line.strip_prefix("## ") {
            if let Some((h, s)) = start {
                out.push((h, &README[s..offset]));
            }
            start = Some((heading.trim_end(), offset));
        }
        offset += line.len();
    }
    if let Some((h, s)) = start {
        out.push((h, &README[s..]));
    }
    out
}

/// GitHub's anchor for a heading: lowercase, spaces to hyphens, punctuation dropped.
fn anchor(heading: &str) -> String {
    heading
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

/// Where a README link goes on the site: a section's anchor to the page that holds it, a file to
/// GitHub. Absolute URLs are left as they are.
fn rewrite(dest: &str, all: &[(&str, &str)]) -> String {
    if let Some(target) = dest.strip_prefix('#') {
        for doc in DOCS {
            for (heading, body) in all.iter().filter(|(h, _)| doc.sections.contains(h)) {
                let inside = body
                    .lines()
                    .filter_map(|l| l.strip_prefix("### ").or(l.strip_prefix("## ")))
                    .any(|h| anchor(h.trim()) == target);
                if anchor(heading) == target || inside {
                    return format!("/docs/{}#{target}", doc.slug);
                }
            }
        }
        return format!("{REPO}#{target}");
    }
    if dest.contains(':') || dest.starts_with('/') {
        return dest.to_owned();
    }
    format!("{REPO}/blob/main/{dest}")
}

fn render(doc: &Doc) -> String {
    let all = sections();
    let markdown: String = doc
        .sections
        .iter()
        .filter_map(|want| all.iter().find(|(h, _)| h == want))
        .map(|(_, body)| *body)
        .collect();
    let mut events: Vec<Event> = Parser::new_ext(&markdown, Options::all()).collect();
    // Give each heading GitHub's anchor, so a link to a section lands on it here too.
    let mut i = 0;
    while i < events.len() {
        if let Event::Start(Tag::Heading { .. }) = events[i] {
            let text: String = events[i + 1..]
                .iter()
                .take_while(|e| !matches!(e, Event::End(TagEnd::Heading(_))))
                .filter_map(|e| match e {
                    Event::Text(t) | Event::Code(t) => Some(t.as_ref()),
                    _ => None,
                })
                .collect();
            if let Event::Start(Tag::Heading { id, .. }) = &mut events[i] {
                *id = Some(CowStr::from(anchor(&text)));
            }
        }
        if let Event::Start(Tag::Link { dest_url, .. }) = &mut events[i] {
            *dest_url = CowStr::from(rewrite(dest_url, &all));
        }
        i += 1;
    }
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, events.into_iter());
    html
}

fn doc_page(doc: &Doc, public_url: &str, analytics: Analytics) -> String {
    let url = format!("{public_url}/docs/{}", doc.slug);
    let json_ld = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "TechArticle",
        "headline": doc.title,
        "description": doc.description,
        "url": url,
        "mainEntityOfPage": url,
        "publisher": { "@type": "Organization", "name": "Mergepatrol", "url": format!("{public_url}/") },
    });
    let head = format!(
        "<meta property=\"og:type\" content=\"article\" />\n<script type=\"application/ld+json\">{}</script>",
        json_ld.to_string().replace("</", "<\\/"),
    );
    let main = format!(
        "<article><h1>{}</h1>{}{}</article>",
        escape(doc.title),
        render(doc),
        docs_nav(Some(doc.slug)),
    );
    page(
        doc.title,
        doc.description,
        &url,
        &head,
        &main,
        public_url,
        analytics,
    )
}

/// The list of every doc, at the foot of each page and on `/docs`.
fn docs_nav(current: Option<&str>) -> String {
    let mut out = String::from("<h2>Docs</h2><ul class=\"posts\">");
    for d in DOCS.iter().filter(|d| Some(d.slug) != current) {
        out.push_str(&format!(
            "<li><a href=\"/docs/{}\">{}</a><p>{}</p></li>",
            d.slug,
            escape(d.title),
            escape(d.description)
        ));
    }
    out.push_str("</ul>");
    out
}

fn index_page(public_url: &str, analytics: Analytics) -> String {
    let main = format!(
        "<h1>Docs</h1><p>How to set up, configure and run Mergepatrol, a merge queue for GitHub. The same text as the <a href=\"{REPO}#readme\">README</a>.</p>{}",
        docs_nav(None)
    );
    page(
        "Docs",
        "How to set up, configure, self-host and automate Mergepatrol, a merge queue for GitHub.",
        &format!("{public_url}/docs"),
        "<meta property=\"og:type\" content=\"website\" />",
        &main,
        public_url,
        analytics,
    )
}

/// `<url>` entries for the sitemap: the index, then each doc.
pub fn sitemap_entries(public_url: &str) -> String {
    let mut out = format!("  <url><loc>{public_url}/docs</loc></url>\n");
    for d in DOCS {
        out.push_str(&format!(
            "  <url><loc>{public_url}/docs/{}</loc></url>\n",
            d.slug
        ));
    }
    out
}

/// The docs' section of `/llms.txt`.
pub fn llms_section(public_url: &str) -> String {
    let mut out = String::from("\n## Docs\n\n");
    for d in DOCS {
        out.push_str(&format!(
            "- [{}]({public_url}/docs/{}): {}\n",
            d.title, d.slug, d.description
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A README heading renamed or removed would leave a doc without its text.
    #[test]
    fn every_doc_section_is_in_the_readme() {
        let all = sections();
        for d in DOCS {
            for s in d.sections {
                assert!(
                    all.iter().any(|(h, _)| h == s),
                    "docs/{}: README has no `## {s}`",
                    d.slug
                );
            }
        }
    }

    #[test]
    fn links_point_at_the_page_or_github() {
        let all = sections();
        assert_eq!(rewrite("#pricing", &all), "/docs/pricing#pricing");
        assert_eq!(
            rewrite("#environment-variables", &all),
            "/docs/self-hosting#environment-variables"
        );
        assert_eq!(
            rewrite("#on-gcp", &all),
            "https://github.com/agent-labs-dev/mergepatrol#on-gcp"
        );
        assert_eq!(
            rewrite("CONTRIBUTING.md", &all),
            "https://github.com/agent-labs-dev/mergepatrol/blob/main/CONTRIBUTING.md"
        );
        assert_eq!(rewrite("https://x.dev/a", &all), "https://x.dev/a");
    }

    #[test]
    fn headings_carry_githubs_anchors() {
        let html = render(&DOCS[5]);
        assert!(html.contains("<h3 id=\"environment-variables\">"), "{html}");
        assert!(html.contains("<h2 id=\"run-it-with-an-app-you-made-yourself\">"));
    }
}
