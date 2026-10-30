//! PROTOTYPE, throwaway. Not production code: answers "what should an agent-written /changelog
//! look like?" Run `cargo run -p mergepatrol-web --example changelog_prototype` and open
//! http://localhost:4100/changelog. `?v=a|b|c` picks the layout, `&src=1` shows the Markdown each
//! agent wrote under its entry. Reads `content/changelog/*.md` and `ui/public/` from disk on every
//! request, so editing an entry is a reload.
//!
//! If a layout wins, the real thing is a `changelog.rs` beside `blog.rs`: embedded, sitemap, feed.

use axum::Router;
use axum::extract::{Path, Query};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use pulldown_cmark::{Options, Parser};
use std::collections::{BTreeMap, HashMap};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
const REPO: &str = "https://github.com/agent-labs-dev/mergepatrol";

struct Entry {
    slug: String,
    title: String,
    date: String,
    kind: String,
    pr: Option<String>,
    image: Option<String>,
    body: String,
    source: String,
}

fn entries() -> Vec<Entry> {
    let mut all = vec![];
    for f in std::fs::read_dir(format!("{ROOT}/content/changelog")).unwrap() {
        let path = f.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let Some(stem) = name.strip_suffix(".md") else { continue };
        if stem == "README" {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let rest = source.strip_prefix("---\n").expect("--- header");
        let (head, body) = rest.split_once("\n---\n").expect("closed header");
        let h: HashMap<&str, String> = head
            .lines()
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim(), v.trim().to_owned()))
            .collect();
        all.push(Entry {
            slug: stem.get(11..).unwrap_or(stem).to_owned(),
            title: h["title"].clone(),
            date: h["date"].clone(),
            kind: h.get("kind").cloned().unwrap_or("improved".into()),
            pr: h.get("pr").cloned(),
            image: h.get("image").cloned(),
            body: body.to_owned(),
            source: source.clone(),
        });
    }
    // Newest day first; within a day, new before improved before fixed, then pictures first.
    let rank = |k: &str| ["new", "improved", "fixed"].iter().position(|x| *x == k).unwrap_or(9);
    all.sort_by(|a, b| {
        b.date
            .cmp(&a.date)
            .then(rank(&a.kind).cmp(&rank(&b.kind)))
            .then(b.image.is_some().cmp(&a.image.is_some()))
            .then(a.slug.cmp(&b.slug))
    });
    all
}

fn md(s: &str) -> String {
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, Parser::new_ext(s, Options::all()));
    out
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn pretty_date(d: &str) -> String {
    const M: [&str; 12] = [
        "January", "February", "March", "April", "May", "June", "July", "August", "September",
        "October", "November", "December",
    ];
    let m: usize = d[5..7].parse().unwrap();
    let day: u32 = d[8..10].parse().unwrap();
    format!("{} {day}, {}", M[m - 1], &d[..4])
}

fn pill(kind: &str) -> String {
    format!("<span class=\"pill {kind}\">{}</span>", match kind {
        "new" => "New",
        "fixed" => "Fixed",
        _ => "Improved",
    })
}

fn pr_link(e: &Entry) -> String {
    e.pr.as_ref()
        .map(|n| format!("<a class=\"pr\" href=\"{REPO}/pull/{n}\">#{n}</a>"))
        .unwrap_or_default()
}

fn source(e: &Entry, show: bool) -> String {
    if !show {
        return String::new();
    }
    format!(
        "<details class=\"src\" open><summary>content/changelog/{}-{}.md, written by the agent in #{}</summary><pre>{}</pre></details>",
        e.date,
        e.slug,
        e.pr.as_deref().unwrap_or("?"),
        esc(&e.source)
    )
}

fn by_day(all: &[Entry]) -> Vec<(&str, Vec<&Entry>)> {
    let mut days: BTreeMap<&str, Vec<&Entry>> = BTreeMap::new();
    for e in all {
        days.entry(&e.date).or_default().push(e);
    }
    days.into_iter().rev().collect()
}

/// A: one long feed, every entry a full post with its hero picture.
fn variant_a(all: &[Entry], src: bool) -> String {
    let mut out = String::from("<div class=\"a\"><h1>Changelog</h1><p class=\"lede\">What changed in Mergepatrol, written by the agents that built it.</p>");
    for (day, es) in by_day(all) {
        out.push_str(&format!("<h2 class=\"day\">{}</h2>", pretty_date(day)));
        for e in es {
            let hero = e
                .image
                .as_ref()
                .map(|i| format!("<img class=\"hero\" src=\"{i}\" alt=\"\" loading=\"lazy\" />"))
                .unwrap_or_default();
            out.push_str(&format!(
                "<article id=\"{}\"><div class=\"row\">{}{}</div><h3><a href=\"/changelog/{}\">{}</a></h3>{hero}{}{}</article>",
                e.slug,
                pill(&e.kind),
                pr_link(e),
                e.slug,
                esc(&e.title),
                md(&e.body),
                source(e, src)
            ));
        }
    }
    out + "</div>"
}

/// B: a timeline. Sticky date on the left; features as cards, fixes folded into one list a day.
fn variant_b(all: &[Entry], src: bool) -> String {
    let mut out = String::from("<div class=\"b\"><h1>Changelog</h1>");
    for (day, es) in by_day(all) {
        out.push_str(&format!(
            "<section><time class=\"rail\">{}</time><div class=\"col\">",
            pretty_date(day)
        ));
        let (fixes, rest): (Vec<_>, Vec<_>) = es.into_iter().partition(|e| e.kind == "fixed");
        for e in rest {
            let hero = e
                .image
                .as_ref()
                .map(|i| format!("<img class=\"hero\" src=\"{i}\" alt=\"\" loading=\"lazy\" />"))
                .unwrap_or_default();
            // Only the first paragraph inline; the rest behind "More".
            let (lead, more) = e.body.trim().split_once("\n\n").unwrap_or((e.body.trim(), ""));
            let more = if more.is_empty() {
                String::new()
            } else {
                format!("<details><summary>More</summary>{}</details>", md(more))
            };
            out.push_str(&format!(
                "<article class=\"card\" id=\"{}\"><div class=\"row\">{}{}</div><h3><a href=\"/changelog/{}\">{}</a></h3>{}{hero}{more}{}</article>",
                e.slug,
                pill(&e.kind),
                pr_link(e),
                e.slug,
                esc(&e.title),
                md(lead),
                source(e, src)
            ));
        }
        if !fixes.is_empty() {
            out.push_str("<div class=\"fixes\"><h4>Fixes</h4><ul>");
            for e in fixes {
                out.push_str(&format!(
                    "<li><details><summary>{} {}</summary>{}{}</details></li>",
                    esc(&e.title),
                    pr_link(e),
                    md(&e.body),
                    source(e, src)
                ));
            }
            out.push_str("</ul></div>");
        }
        out.push_str("</div></section>");
    }
    out + "</div>"
}

/// C: one release note per day: a grid of picture highlights, then terse lists.
fn variant_c(all: &[Entry], src: bool) -> String {
    let mut out = String::from("<div class=\"c\"><h1>Changelog</h1>");
    for (day, es) in by_day(all) {
        let n = es.len();
        out.push_str(&format!(
            "<section class=\"release\"><h2>{}</h2><p class=\"meta\">{n} changes</p>",
            pretty_date(day)
        ));
        let (shots, plain): (Vec<_>, Vec<_>) =
            es.into_iter().partition(|e| e.image.is_some() && e.kind != "fixed");
        if !shots.is_empty() {
            out.push_str("<div class=\"grid\">");
            for e in shots {
                let lead = e.body.trim().split("\n\n").next().unwrap_or("");
                out.push_str(&format!(
                    "<a class=\"tile\" href=\"/changelog/{}\"><img src=\"{}\" alt=\"\" loading=\"lazy\" /><div>{}<strong>{}</strong><p>{}</p></div></a>",
                    e.slug,
                    e.image.as_deref().unwrap(),
                    pill(&e.kind),
                    esc(&e.title),
                    esc(&lead.replace("**", "").replace('`', ""))
                ));
                if src {
                    out.push_str(&source(e, true));
                }
            }
            out.push_str("</div>");
        }
        for (label, kinds) in [("New and improved", ["new", "improved"]), ("Fixed", ["fixed", "fixed"])] {
            let list: Vec<_> = plain.iter().filter(|e| kinds.contains(&e.kind.as_str())).collect();
            if list.is_empty() {
                continue;
            }
            out.push_str(&format!("<h4>{label}</h4><ul class=\"terse\">"));
            for e in list {
                out.push_str(&format!(
                    "<li><a href=\"/changelog/{}\">{}</a> {}{}</li>",
                    e.slug,
                    esc(&e.title),
                    pr_link(e),
                    source(e, src)
                ));
            }
            out.push_str("</ul>");
        }
        out.push_str("</section>");
    }
    out + "</div>"
}

/// One entry's own page, shared by every layout: what a "what's new" link or a share opens.
fn permalink(e: &Entry, src: bool) -> String {
    let hero = e
        .image
        .as_ref()
        .map(|i| format!("<img class=\"hero\" src=\"{i}\" alt=\"\" />"))
        .unwrap_or_default();
    format!(
        "<div class=\"a\"><p><a href=\"/changelog\">← Changelog</a></p><article><div class=\"row\">{}<time>{}</time>{}</div><h1>{}</h1>{hero}{}{}</article></div>",
        pill(&e.kind),
        pretty_date(&e.date),
        pr_link(e),
        esc(&e.title),
        md(&e.body),
        source(e, src)
    )
}

fn page(title: &str, main: &str, v: &str, src: bool, path: &str, all: &[Entry]) -> Html<String> {
    let shots = all.iter().filter(|e| e.image.is_some()).count();
    let link = |nv: &str, ns: bool| {
        format!("{path}?v={nv}{}", if ns { "&src=1" } else { "" })
    };
    let tab = |nv: &str, label: &str| {
        format!(
            "<a class=\"{}\" href=\"{}\">{label}</a>",
            if nv == v { "on" } else { "" },
            link(nv, src)
        )
    };
    let bar = format!(
        "<nav class=\"proto\"><b>PROTOTYPE</b>{}{}{}<a class=\"{}\" href=\"{}\">Agent source</a><span>{} entries · {shots} with a picture · ← → to switch</span></nav>",
        tab("a", "A · Feed"),
        tab("b", "B · Timeline"),
        tab("c", "C · Daily release"),
        if src { "on" } else { "" },
        link(v, !src),
        all.len()
    );
    let keys = format!(
        "<script>const vs=['a','b','c'];addEventListener('keydown',e=>{{if(e.key!=='ArrowLeft'&&e.key!=='ArrowRight')return;const i=vs.indexOf('{v}');const n=vs[(i+(e.key==='ArrowRight'?1:2))%3];const u=new URL(location);u.searchParams.set('v',n);location=u}})</script>"
    );
    Html(format!(
        r#"<!doctype html><html lang="en"><head><meta charset="UTF-8" /><meta name="viewport" content="width=device-width, initial-scale=1.0" /><title>{} | Mergepatrol</title><link rel="icon" href="/favicon.svg" /><style>{STYLE}</style></head><body><header><a href="/" class="brand"><img src="/logo.svg" alt="" width="20" height="20" />Mergepatrol</a><span><a href="/blog">Blog</a> <a href="/changelog?v={v}">Changelog</a></span></header><main>{main}</main><footer><a href="/">Mergepatrol</a>: a merge queue for GitHub, with an agent on conflicts.</footer>{bar}{keys}</body></html>"#,
        esc(title)
    ))
}

#[derive(serde::Deserialize)]
struct Q {
    v: Option<String>,
    src: Option<String>,
}

async fn index(Query(q): Query<Q>) -> Html<String> {
    let all = entries();
    let v = q.v.unwrap_or("a".into());
    let src = q.src.is_some();
    let main = match v.as_str() {
        "b" => variant_b(&all, src),
        "c" => variant_c(&all, src),
        _ => variant_a(&all, src),
    };
    page("Changelog", &main, &v, src, "/changelog", &all)
}

async fn one(Path(slug): Path<String>, Query(q): Query<Q>) -> Response {
    let all = entries();
    let Some(e) = all.iter().find(|e| e.slug == slug) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let v = q.v.unwrap_or("a".into());
    let path = format!("/changelog/{slug}");
    page(&e.title, &permalink(e, q.src.is_some()), &v, q.src.is_some(), &path, &all).into_response()
}

async fn public(uri: axum::http::Uri) -> Response {
    let rel = uri.path().trim_start_matches('/');
    if rel.contains("..") {
        return StatusCode::NOT_FOUND.into_response();
    }
    match std::fs::read(format!("{ROOT}/ui/public/{rel}")) {
        Ok(bytes) => {
            let mime = mime_guess::from_path(rel).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.to_string())], bytes).into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "prototype serves only /changelog").into_response(),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let app = Router::new()
        .route("/", get(|| async { axum::response::Redirect::to("/changelog") }))
        .route("/changelog", get(index))
        .route("/changelog/{slug}", get(one))
        .fallback(public);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:4100").await.unwrap();
    println!("PROTOTYPE changelog: http://localhost:4100/changelog");
    axum::serve(listener, app).await.unwrap();
}

const STYLE: &str = r#"
:root{color-scheme:light dark;--bg:#fafafa;--fg:#171717;--fg2:#525252;--line:#e5e5e5;--sunken:#f5f5f5;--card:#fff}
@media(prefers-color-scheme:dark){:root{--bg:#0a0a0a;--fg:#fafafa;--fg2:#a3a3a3;--line:#262626;--sunken:#171717;--card:#111}}
body{margin:0;background:var(--bg);color:var(--fg);font:16px/1.7 ui-sans-serif,system-ui,sans-serif;padding-bottom:4rem}
header,main,footer{max-width:42rem;margin:0 auto;padding:0 1.25rem}
header{display:flex;justify-content:space-between;align-items:center;height:3.5rem;border-bottom:1px solid var(--line)}
header span a{margin-left:1rem}
.brand{display:flex;gap:.5rem;align-items:center;font-weight:600}
a{color:inherit}header a,h3 a,.tile,.terse a{text-decoration:none}
main{padding-top:2rem;padding-bottom:3rem}
h1,h2,h3,h4{font-weight:600;letter-spacing:-.02em;line-height:1.25}h1{font-size:2rem;margin:0 0 .5rem}
.lede,.meta,time,.pr{color:var(--fg2);font-size:.875rem}
pre{background:var(--sunken);padding:1rem;overflow-x:auto;border-radius:.5rem}
code{font:.875em ui-monospace,monospace}img{max-width:100%;border-radius:.5rem;border:1px solid var(--line)}
table{border-collapse:collapse}td,th{border:1px solid var(--line);padding:.25rem .5rem}
footer{border-top:1px solid var(--line);padding-top:1rem;padding-bottom:2rem;color:var(--fg2);font-size:.875rem}
.row{display:flex;gap:.5rem;align-items:center}
.pill{font-size:.75rem;font-weight:600;padding:0 .5rem;border-radius:999px;border:1px solid var(--line)}
.pill.new{background:#dcfce7;color:#166534;border-color:#bbf7d0}.pill.fixed{background:#fef3c7;color:#92400e;border-color:#fde68a}.pill.improved{background:#dbeafe;color:#1e40af;border-color:#bfdbfe}
@media(prefers-color-scheme:dark){.pill.new{background:#052e16;color:#86efac;border-color:#14532d}.pill.fixed{background:#2d1b03;color:#fcd34d;border-color:#78350f}.pill.improved{background:#0b1a3a;color:#93c5fd;border-color:#1e3a8a}}
.hero{display:block;margin:.75rem 0}
.src{margin:.75rem 0;font-size:.8rem}.src summary{color:var(--fg2);cursor:pointer}.src pre{white-space:pre-wrap;border:1px dashed #a855f7}

.a .day{font-size:.8rem;text-transform:uppercase;letter-spacing:.08em;color:var(--fg2);margin:3rem 0 0;padding-bottom:.5rem;border-bottom:1px solid var(--line)}
.a article{padding:1.5rem 0;border-bottom:1px solid var(--line)}.a h3{font-size:1.375rem;margin:.5rem 0}

.b section{display:grid;grid-template-columns:9rem 1fr;gap:1.5rem;margin-top:2.5rem}
.b .rail{position:sticky;top:1rem;align-self:start;font-weight:600;color:var(--fg)}
.b .col{border-left:1px solid var(--line);padding-left:1.5rem}
.b .card{background:var(--card);border:1px solid var(--line);border-radius:.75rem;padding:1rem 1.25rem;margin-bottom:1rem}
.b .card h3{margin:.4rem 0;font-size:1.125rem}.b .card p{margin:.25rem 0}
.b details summary{cursor:pointer;color:var(--fg2);font-size:.875rem}
.b .fixes h4{margin:.5rem 0}.b .fixes ul{padding-left:1.1rem;margin:0}.b .fixes summary{color:var(--fg)}
@media(max-width:640px){.b section{grid-template-columns:1fr}.b .rail{position:static}}
.c .release{margin-top:2.5rem;padding-bottom:1.5rem;border-bottom:1px solid var(--line)}.c h2{margin:0;font-size:1.5rem}
.c .grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(18rem,1fr));gap:1rem;margin:1rem 0}
.c .tile{display:block;background:var(--card);border:1px solid var(--line);border-radius:.75rem;overflow:hidden}
.c .tile img{border:0;border-radius:0;width:100%;aspect-ratio:16/10;object-fit:cover;object-position:top left;border-bottom:1px solid var(--line)}
.c .tile div{padding:.75rem 1rem}.c .tile strong{display:block;margin:.35rem 0 .25rem;line-height:1.3}.c .tile p{margin:0;font-size:.85rem;color:var(--fg2);line-height:1.5}
.c .terse{padding-left:1.1rem}.c h4{margin:1.25rem 0 .25rem}
main:has(.c){max-width:60rem}
.proto{position:fixed;bottom:1rem;left:50%;transform:translateX(-50%);display:flex;gap:.25rem;align-items:center;background:#171717;color:#fafafa;padding:.35rem .5rem;border-radius:999px;font-size:.8rem;box-shadow:0 8px 24px #0004;white-space:nowrap}
.proto b{color:#c084fc;padding:0 .5rem}.proto a{text-decoration:none;padding:.2rem .7rem;border-radius:999px}.proto a.on{background:#fafafa;color:#171717}.proto span{color:#a3a3a3;padding:0 .5rem}
"#;
