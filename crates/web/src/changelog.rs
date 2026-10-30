//! The changelog at `/changelog`, and each entry at `/changelog/<slug>`, rendered on the server
//! from `content/changelog/<YYYY-MM-DD>-<slug>.md` like the blog (`blog.rs`).
//!
//! The index is a timeline: one section a day, newest first. New features and improvements are
//! cards with their picture; fixes fold into one list at the end of the day.

use axum::Json;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use pulldown_cmark::{Options, Parser};
use serde::Serialize;
use ts_rs::TS;

use crate::blog::{Analytics, escape, page};

#[derive(rust_embed::RustEmbed)]
#[folder = "../../content/changelog"]
#[allow_missing = true]
struct Entries;

const REPO: &str = "https://github.com/agent-labs-dev/mergepatrol";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, rename = "ChangelogKind")]
pub enum Kind {
    New,
    Improved,
    Fixed,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::New => "New",
            Kind::Improved => "Improved",
            Kind::Fixed => "Fixed",
        }
    }

    fn class(self) -> &'static str {
        match self {
            Kind::New => "new",
            Kind::Improved => "improved",
            Kind::Fixed => "fixed",
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Entry {
    pub slug: String,
    pub title: String,
    /// `YYYY-MM-DD`, from the file name.
    pub date: String,
    pub kind: Kind,
    pub pr: Option<u32>,
    /// A path under `ui/public`, such as `/changelog/you-tab/needs-you.png`.
    pub image: Option<String>,
    pub body: String,
}

/// Answers `/changelog` and `/changelog/<slug>`. `None` for any other path, including the
/// entries' pictures under `/changelog/<slug>/`, so the caller serves those from `ui/public`.
pub fn serve(path: &str, public_url: &str, analytics: Analytics) -> Option<Response> {
    let rest = path.strip_prefix("changelog")?;
    let slug = match rest.trim_end_matches('/') {
        "" => return Some(html(index_page(&entries(), public_url, analytics))),
        s => s.strip_prefix('/')?,
    };
    if slug.contains('/') {
        return None;
    }
    match entries().into_iter().find(|e| e.slug == slug) {
        Some(e) => Some(html(entry_page(&e, public_url, analytics))),
        None => Some(StatusCode::NOT_FOUND.into_response()),
    }
}

/// An entry as the dashboard's update notice lists it.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct ChangelogEntryView {
    pub slug: String,
    pub title: String,
    pub kind: Kind,
}

/// The newest entries, for the dashboard to tell what a reload brings: those it didn't have.
pub async fn latest() -> Json<Vec<ChangelogEntryView>> {
    Json(
        entries()
            .into_iter()
            .take(20)
            .map(|e| ChangelogEntryView {
                slug: e.slug,
                title: e.title,
                kind: e.kind,
            })
            .collect(),
    )
}

fn html(body: String) -> Response {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response()
}

/// Every entry: newest day first, then new before improved before fixed. A malformed file is
/// logged and left out; the `every_entry_parses` test keeps one from reaching `main`.
pub fn entries() -> Vec<Entry> {
    let mut all: Vec<Entry> = files()
        .filter_map(|stem| {
            let data = Entries::get(&format!("{stem}.md"))?.data;
            parse(&stem, &String::from_utf8_lossy(&data))
                .inspect_err(|e| {
                    tracing::error!(file = stem, error = %e, "changelog entry is malformed, not serving it")
                })
                .ok()
        })
        .collect();
    all.sort_by(|a, b| {
        b.date
            .cmp(&a.date)
            .then(a.kind.cmp(&b.kind))
            .then_with(|| a.slug.cmp(&b.slug))
    });
    all
}

/// The entries' file names, without `.md`. The folder's README is not one.
fn files() -> impl Iterator<Item = String> {
    Entries::iter().filter_map(|file| {
        let stem = file.strip_suffix(".md")?;
        (stem != "README").then(|| stem.to_owned())
    })
}

/// Reads a `<YYYY-MM-DD>-<slug>` file name, then a `---`-fenced header of `key: value` lines
/// (title, kind, optional pr and image), then the Markdown body.
pub fn parse(stem: &str, source: &str) -> Result<Entry, String> {
    let (date, slug) = match (stem.get(..10), stem.get(10..11), stem.get(11..)) {
        (Some(d), Some("-"), Some(s)) if is_iso_date(d) => (d, s),
        _ => return Err(format!("file name {stem:?} must be YYYY-MM-DD-<slug>")),
    };
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
    let (head, body) = rest
        .split_once("\n---\n")
        .ok_or("the --- header is not closed")?;
    let (mut title, mut kind, mut pr, mut image) = (None, None, None, None);
    for line in head.lines().filter(|l| !l.trim().is_empty()) {
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| format!("header line {line:?} is not `key: value`"))?;
        let value = value.trim();
        if value.is_empty() {
            return Err(format!("header key {:?} is empty", key.trim()));
        }
        match key.trim() {
            "title" => title = Some(value.to_owned()),
            "kind" => {
                kind = Some(match value {
                    "new" => Kind::New,
                    "improved" => Kind::Improved,
                    "fixed" => Kind::Fixed,
                    other => return Err(format!("kind {other:?} is not new, improved or fixed")),
                })
            }
            "pr" => {
                pr = Some(
                    value
                        .parse()
                        .map_err(|_| format!("pr {value:?} is not a number"))?,
                )
            }
            "image" => {
                if !value.starts_with("/changelog/") {
                    return Err(format!("image {value:?} must be under /changelog/"));
                }
                image = Some(value.to_owned())
            }
            other => return Err(format!("unknown header key {other:?}")),
        }
    }
    Ok(Entry {
        slug: slug.to_owned(),
        title: title.ok_or("missing title")?,
        date: date.to_owned(),
        kind: kind.ok_or("missing kind")?,
        pr,
        image,
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

/// `<url>` entries for the sitemap: the index, then each entry.
pub fn sitemap_entries(public_url: &str) -> String {
    let all = entries();
    let mut out = match all.first() {
        Some(newest) => format!(
            "  <url><loc>{public_url}/changelog</loc><lastmod>{}</lastmod></url>\n",
            newest.date
        ),
        None => format!("  <url><loc>{public_url}/changelog</loc></url>\n"),
    };
    for e in &all {
        out.push_str(&format!(
            "  <url><loc>{public_url}/changelog/{}</loc><lastmod>{}</lastmod></url>\n",
            e.slug, e.date
        ));
    }
    out
}

fn markdown(s: &str) -> String {
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, Parser::new_ext(s, Options::all()));
    out
}

/// `2026-10-30` as `October 30, 2026`.
fn long_date(date: &str) -> String {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let month: usize = date[5..7].parse().unwrap_or(1);
    let day: u32 = date[8..10].parse().unwrap_or(1);
    format!("{} {day}, {}", MONTHS[month.clamp(1, 12) - 1], &date[..4])
}

fn pill(kind: Kind) -> String {
    format!(
        "<span class=\"pill {}\">{}</span>",
        kind.class(),
        kind.label()
    )
}

fn pr_link(e: &Entry) -> String {
    e.pr.map(|n| format!("<a class=\"pr\" href=\"{REPO}/pull/{n}\">#{n}</a>"))
        .unwrap_or_default()
}

fn picture(e: &Entry) -> String {
    e.image
        .as_ref()
        .map(|src| {
            format!(
                "<img class=\"shot\" src=\"{}\" alt=\"{}\" loading=\"lazy\" />",
                escape(src),
                escape(&e.title)
            )
        })
        .unwrap_or_default()
}

fn index_page(all: &[Entry], public_url: &str, analytics: Analytics) -> String {
    let mut main = String::from(
        "<h1>Changelog</h1><p class=\"lede\">New features, improvements and fixes in Mergepatrol.</p>",
    );
    for day in all.chunk_by(|a, b| a.date == b.date) {
        let date = &day[0].date;
        main.push_str(&format!(
            "<section class=\"day\"><time class=\"rail\" datetime=\"{date}\">{}</time><div class=\"col\">",
            long_date(date)
        ));
        let (fixes, cards): (Vec<&Entry>, Vec<&Entry>) =
            day.iter().partition(|e| e.kind == Kind::Fixed);
        for e in cards {
            // The first paragraph shows; the rest folds behind "More".
            let body = e.body.trim();
            let (lead, more) = body.split_once("\n\n").unwrap_or((body, ""));
            let more = if more.is_empty() {
                String::new()
            } else {
                format!(
                    "<details><summary>More</summary>{}</details>",
                    markdown(more)
                )
            };
            main.push_str(&format!(
                "<article class=\"card\" id=\"{slug}\"><div class=\"row\">{}{}</div><h2><a href=\"/changelog/{slug}\">{}</a></h2>{}{}{more}</article>",
                pill(e.kind),
                pr_link(e),
                escape(&e.title),
                markdown(lead),
                picture(e),
                slug = e.slug,
            ));
        }
        if !fixes.is_empty() {
            main.push_str("<div class=\"fixes\"><h3>Fixes</h3><ul>");
            for e in fixes {
                main.push_str(&format!(
                    "<li id=\"{}\"><details><summary>{} {}</summary>{}</details></li>",
                    e.slug,
                    escape(&e.title),
                    pr_link(e),
                    markdown(&e.body)
                ));
            }
            main.push_str("</ul></div>");
        }
        main.push_str("</div></section>");
    }
    page(
        "Changelog",
        "New features, improvements and fixes in Mergepatrol, the merge queue for GitHub.",
        &format!("{public_url}/changelog"),
        &format!("<meta property=\"og:type\" content=\"website\" /><style>{STYLE}</style>"),
        &main,
        public_url,
        analytics,
    )
}

fn entry_page(e: &Entry, public_url: &str, analytics: Analytics) -> String {
    let main = format!(
        "<p><a href=\"/changelog\">← Changelog</a></p><article class=\"entry\"><div class=\"row\">{}<time datetime=\"{}\">{}</time>{}</div><h1>{}</h1>{}{}</article>",
        pill(e.kind),
        e.date,
        long_date(&e.date),
        pr_link(e),
        escape(&e.title),
        picture(e),
        markdown(&e.body),
    );
    let description = e
        .body
        .trim()
        .split("\n\n")
        .next()
        .unwrap_or_default()
        .replace(['*', '`'], "")
        .replace('\n', " ");
    page(
        &e.title,
        &description,
        &format!("{public_url}/changelog/{}", e.slug),
        &format!("<meta property=\"og:type\" content=\"article\" /><style>{STYLE}</style>"),
        &main,
        public_url,
        analytics,
    )
}

const STYLE: &str = "\
.lede{color:var(--fg2);margin:0}\
.row{display:flex;gap:.5rem;align-items:center;font-size:.875rem}.pr{color:var(--fg2)}\
.pill{font-size:.75rem;font-weight:600;padding:0 .5rem;border-radius:999px;border:1px solid}\
.pill.new{background:#dcfce7;color:#166534;border-color:#bbf7d0}\
.pill.improved{background:#dbeafe;color:#1e40af;border-color:#bfdbfe}\
.pill.fixed{background:#fef3c7;color:#92400e;border-color:#fde68a}\
@media(prefers-color-scheme:dark){.pill.new{background:#052e16;color:#86efac;border-color:#14532d}\
.pill.improved{background:#0b1a3a;color:#93c5fd;border-color:#1e3a8a}\
.pill.fixed{background:#2d1b03;color:#fcd34d;border-color:#78350f}}\
img.shot,.entry img,.card img{display:block;margin:.75rem 0;max-height:28rem;border:1px solid var(--line);border-radius:.5rem}\
.day{display:grid;grid-template-columns:9rem 1fr;gap:1.5rem;margin-top:2.5rem}\
.rail{position:sticky;top:1rem;align-self:start;font-weight:600;color:var(--fg);font-size:1rem}\
.col{border-left:1px solid var(--line);padding-left:1.5rem;min-width:0}\
.card{border:1px solid var(--line);border-radius:.75rem;padding:1rem 1.25rem;margin-bottom:1rem}\
.card h2{margin:.4rem 0;font-size:1.125rem}.card h2 a{text-decoration:none}.card p{margin:.25rem 0}\
details summary{cursor:pointer}.card summary{color:var(--fg2);font-size:.875rem}\
.fixes h3{margin:.5rem 0;font-size:1rem}.fixes ul{padding-left:1.1rem;margin:0}\
body:has(.day) :is(header,main,footer){max-width:52rem}\
@media(max-width:640px){.day{grid-template-columns:1fr;gap:.5rem}.rail{position:static}.col{border:0;padding:0}}";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_parses() {
        for stem in files() {
            let data = Entries::get(&format!("{stem}.md")).unwrap().data;
            if let Err(e) = parse(&stem, &String::from_utf8_lossy(&data)) {
                panic!("content/changelog/{stem}.md: {e}");
            }
        }
    }

    #[test]
    fn slugs_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for e in entries() {
            assert!(
                seen.insert(e.slug.clone()),
                "two entries are /changelog/{}",
                e.slug
            );
        }
    }

    /// Every picture an entry names, in its header or its body, is in `ui/public`.
    #[test]
    fn every_picture_exists() {
        let public = concat!(env!("CARGO_MANIFEST_DIR"), "/../../ui/public");
        for e in entries() {
            let inline = e
                .body
                .split("](/changelog/")
                .skip(1)
                .filter_map(|rest| rest.split(')').next().map(|p| format!("/changelog/{p}")));
            for src in e.image.clone().into_iter().chain(inline) {
                assert!(
                    std::path::Path::new(&format!("{public}{src}")).is_file(),
                    "{}: ui/public{src} is missing",
                    e.slug
                );
            }
        }
    }

    #[test]
    fn parses_header_and_rejects_malformed() {
        let ok = "---\ntitle: A: B\nkind: fixed\npr: 7\n---\nBody\n";
        let e = parse("2026-10-30-a-b", ok).unwrap();
        assert_eq!(
            (
                e.slug.as_str(),
                e.date.as_str(),
                e.title.as_str(),
                e.kind,
                e.pr
            ),
            ("a-b", "2026-10-30", "A: B", Kind::Fixed, Some(7))
        );
        for (stem, bad) in [
            ("a-b", ok),
            ("2026-10-30-A_b", ok),
            ("2026-10-30-", ok),
            ("2026-10-30-a", "title: T\n"),
            ("2026-10-30-a", "---\nkind: new\n---\n"),
            ("2026-10-30-a", "---\ntitle: T\n---\n"),
            ("2026-10-30-a", "---\ntitle: T\nkind: shiny\n---\n"),
            ("2026-10-30-a", "---\ntitle: T\nkind: new\npr: #7\n---\n"),
            (
                "2026-10-30-a",
                "---\ntitle: T\nkind: new\nimage: /x.png\n---\n",
            ),
            (
                "2026-10-30-a",
                "---\ntitle: T\nkind: new\ndate: 2026-10-30\n---\n",
            ),
        ] {
            assert!(
                parse(stem, bad).is_err(),
                "{stem} {bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn serves_index_and_entries_but_not_pictures() {
        let index = serve("changelog", "https://mergepatrol.ai", None).unwrap();
        assert_eq!(index.status(), StatusCode::OK);
        let first = &entries()[0];
        let one = serve(
            &format!("changelog/{}", first.slug),
            "https://mergepatrol.ai",
            None,
        );
        assert_eq!(one.unwrap().status(), StatusCode::OK);
        let missing = serve("changelog/nope", "https://mergepatrol.ai", None).unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        assert!(serve("changelog/you-tab/needs-you.png", "", None).is_none());
        assert!(serve("changelogs", "", None).is_none());
    }

    #[test]
    fn fixes_fold_into_one_list_a_day() {
        let e = |stem: &str, kind: &str| {
            parse(
                stem,
                &format!("---\ntitle: {stem}\nkind: {kind}\n---\nBody\n"),
            )
            .unwrap()
        };
        let html = index_page(
            &[
                e("2026-10-30-feature", "new"),
                e("2026-10-30-bug", "fixed"),
                e("2026-10-29-older", "improved"),
            ],
            "https://mergepatrol.ai",
            None,
        );
        let newer = html.find("October 30, 2026").unwrap();
        let older = html.find("October 29, 2026").unwrap();
        let fixes = html.find("<h3>Fixes</h3>").unwrap();
        assert!(newer < html.find("2026-10-30-feature").unwrap());
        assert!(newer < fixes && fixes < older, "{html}");
        assert_eq!(html.matches("<h3>Fixes</h3>").count(), 1);
    }
}
