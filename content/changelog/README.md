# Changelog entries

Each `.md` file here is one entry on `https://mergeloop.ai/changelog`, with its own page at
`/changelog/<slug>`. The server renders them with one template (`crates/web/src/changelog.rs`),
so an entry is only its text and its picture.

## When a PR adds one

A PR whose change a MergeLoop user can see or feel adds one entry, in the same PR: a new page,
control or setting, a change in how the queue behaves, a fix to something users hit, a price
change. Deploy, CI, refactors, internal logs, tests and docs-only PRs don't. The test: would a
user who reads the entry care? If not, skip it.

## Add an entry

1. Add `content/changelog/<YYYY-MM-DD>-<slug>.md`. The date is the day the PR merges, as far as
   you can tell; the changelog groups entries by it. The slug is the URL: lowercase letters,
   digits and hyphens, unique across the folder.
2. Start it with this header, then write the entry in Markdown:

   ```markdown
   ---
   title: Close a PR from the queue page
   kind: improved
   pr: 125
   image: /changelog/close-pr/after.png
   ---

   PRs in the **Open · not queued** list have a close button. It asks first, then closes the
   PR as you, so GitHub decides who may.
   ```

   | Key | Required | What it is |
   | --- | --- | --- |
   | `title` | yes | What the user gets, as a short sentence. Not the PR's Conventional Commit title. |
   | `kind` | yes | `new`, `improved` or `fixed`. Fixes fold into one list a day, without pictures. |
   | `pr` | no | The PR number, linked from the entry. |
   | `image` | no | The one picture that shows the change, shown under the first paragraph. |

   No other keys are accepted.
3. Write to the user, in the present tense: what they can do now, and where. One to three short
   paragraphs; one or two sentences for a fix. The first paragraph is all the index shows; the
   rest folds behind "More". No file names, internals, or how it was built.
4. Pictures: a PR that changes what users see already has screenshots for its Evidence. Copy the
   one or two that show the change best to `ui/public/changelog/<slug>/` and keep them; link
   them from the site root, e.g. `![The Code tab](/changelog/open-pr-detail/code.png)`. Use the
   "after" state, at the size and theme the PR used. No customer repos, names or avatars.
5. Run `cargo test -p mergepatrol-web changelog`. It fails on a malformed header, a repeated
   slug, or a picture that isn't in `ui/public`, naming the file.

The entry goes live when the PR merges to `main` and deploys.
