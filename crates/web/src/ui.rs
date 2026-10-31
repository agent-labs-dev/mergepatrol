use axum::extract::State;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};

use crate::AppState;

#[derive(rust_embed::RustEmbed)]
#[folder = "../../ui/dist"]
#[allow_missing = true]
struct UiAssets;

/// Where `index.html` names the server's own address: its canonical link, Open Graph URL and
/// structured data need it absolute, and only the server knows it.
const PUBLIC_URL: &str = "__PUBLIC_URL__";

/// Serves the dashboard. Unknown paths get `index.html`, so client-side routes work, but an
/// unknown file (a path with an extension) is a 404, so crawlers and link previews see it missing.
pub async fn serve(State(state): State<AppState>, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    match path {
        "robots.txt" => return robots(&state.public_url).into_response(),
        "sitemap.xml" => {
            return (
                [(header::CONTENT_TYPE, "application/xml")],
                sitemap(&state.public_url),
            )
                .into_response();
        }
        "llms.txt" => {
            return (
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                llms(&state.public_url),
            )
                .into_response();
        }
        _ => {}
    }
    for collection in crate::blog::COLLECTIONS {
        if let Some(page) = collection.serve(path, &state.public_url, state.analytics.browser()) {
            return page;
        }
    }
    if let Some(page) = crate::changelog::serve(path, &state.public_url, state.analytics.browser())
    {
        return page;
    }
    if !path.is_empty()
        && let Some(file) = UiAssets::get(path)
    {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        return ([(header::CONTENT_TYPE, mime.as_ref())], file.data).into_response();
    }
    if path
        .rsplit('/')
        .next()
        .is_some_and(|name| name.contains('.'))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    match UiAssets::get("index.html") {
        Some(f) => {
            let html = String::from_utf8_lossy(&f.data).replace(PUBLIC_URL, &state.public_url);
            ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            "The dashboard is not built. Run `pnpm build` in ui/.",
        )
            .into_response(),
    }
}

/// The landing page, the blog, the comparisons and the changelog are public. The rest is the API, sign-in and webhooks.
fn robots(public_url: &str) -> String {
    format!(
        "User-agent: *\nAllow: /\nDisallow: /api/\nDisallow: /auth/\nDisallow: /setup/\nDisallow: /webhooks/\n\nSitemap: {public_url}/sitemap.xml\n"
    )
}

fn sitemap(public_url: &str) -> String {
    let pages: String = crate::blog::COLLECTIONS
        .iter()
        .map(|c| c.sitemap_entries(public_url))
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n  <url><loc>{public_url}/</loc></url>\n{pages}{}</urlset>\n",
        crate::changelog::sitemap_entries(public_url)
    )
}

/// `/llms.txt` (llmstxt.org): what Mergepatrol is, and a link to each public page, for the
/// assistants that answer questions about merge queues. The pages come from the same files as
/// the site, so the list cannot fall behind it.
fn llms(public_url: &str) -> String {
    let pages: String = crate::blog::COLLECTIONS
        .iter()
        .map(|c| c.llms_section(public_url))
        .collect();
    format!(
        "{}{pages}\n## More\n\n- [Changelog]({public_url}/changelog): what changed, newest first\n- [Source and self-hosting](https://github.com/agent-labs-dev/mergepatrol): the README covers setup, the config file, the MCP server and the API\n",
        LLMS_INTRO.replace("{public_url}", public_url)
    )
}

const LLMS_INTRO: &str = "\
# Mergepatrol

> Mergepatrol is a merge queue for GitHub. Add a label to a pull request, and it rebases the PR onto its base branch, tests it with the PRs around it, and merges it once the required checks pass. When a rebase conflicts or a required check fails, a coding agent (Claude Code or Codex, on your own model API key) resolves the conflict or commits a fix, and the PR keeps its place.

- Hosted at {public_url}, as the public GitHub App `Mergepatrol`. Free on public repos; private repos cost $15 per seat a month after a 14-day trial with no card. Open source (Apache-2.0) and self-hostable at no cost.
- Works on any GitHub plan, including private repos on GitHub Free, Pro and Team, where GitHub's own merge queue is not available.
- Batches PRs on a draft PR so the repo's existing `pull_request` CI runs on them (no `merge_group` trigger), and bisects a failed batch down to the PR that broke it.
- Queues and merges `gh stack` stacks as one item. Queues PRs from forks without exposing the repo's secrets.
- Optional automatic review: an agent reviews each queued PR and asks code owners only about what it is unsure of.
- An MCP server at {public_url}/mcp lets coding agents queue, review and read PRs.
";
