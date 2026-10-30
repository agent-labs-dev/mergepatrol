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
        _ => {}
    }
    if let Some(page) = crate::blog::serve(path, &state.public_url, state.analytics.browser()) {
        return page;
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

/// The landing page, the blog and the changelog are public. The rest is the API, sign-in and webhooks.
fn robots(public_url: &str) -> String {
    format!(
        "User-agent: *\nAllow: /\nDisallow: /api/\nDisallow: /auth/\nDisallow: /setup/\nDisallow: /webhooks/\n\nSitemap: {public_url}/sitemap.xml\n"
    )
}

fn sitemap(public_url: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n  <url><loc>{public_url}/</loc></url>\n{}{}</urlset>\n",
        crate::blog::sitemap_entries(public_url),
        crate::changelog::sitemap_entries(public_url)
    )
}
