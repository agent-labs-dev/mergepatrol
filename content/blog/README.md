# Blog articles

Each `.md` file here is an article at `https://mergeloop.ai/blog/<file name>`. The server
renders it with one shared template (`crates/web/src/blog.rs`), so an article is only its text:
no layout code, no route, no sitemap edit.

## Add an article

1. Add `content/blog/<slug>.md`. The slug is the URL: lowercase letters, digits and hyphens.
2. Start it with this header, then write the article in Markdown:

   ```markdown
   ---
   title: How a merge queue batches pull requests
   description: One or two sentences for search results and link previews, about 150 characters.
   date: 2026-10-30
   ---

   The first paragraph...
   ```

   | Key | Required | What it becomes |
   | --- | --- | --- |
   | `title` | yes | `<title>`, `og:title`, the page's `<h1>`. Don't repeat it as a `#` heading in the body. |
   | `description` | yes | `<meta name="description">`, `og:description`, the line under the title on `/blog` |
   | `date` | yes | The publish date, `YYYY-MM-DD`. `/blog` lists newest first. |
   | `updated` | no | `YYYY-MM-DD`, when the article last changed. The sitemap's `lastmod`. |

   No other keys are accepted. Use `##` and below for sections in the body. Put images in
   `ui/public/articles/` and link them from the site root, e.g. `![Alt](/articles/queue.png)`.
3. Run `cargo test -p mergepatrol-web blog`. It fails on a malformed header, naming the file.
4. Open a PR. The article goes live when it merges to `main` and deploys.

Each article gets its canonical link, Open Graph and Twitter tags, `BlogPosting` structured data
and a sitemap entry from its header. Files in this folder that don't end in `.md`, and this
README, are not served.
