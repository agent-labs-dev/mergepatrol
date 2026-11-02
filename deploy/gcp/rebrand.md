# MergeLoop domain cutover

The hosted app's canonical URL is `https://mergeloop.ai`. Both domains must retain their
existing DNS and certificates on the `mergepatrol` Gateway. No DNS change is needed for the
cutover: both domains already reach it.

Before deploying this change, an owner of the existing GitHub App (`merge-patrol`) must add
`https://mergeloop.ai/auth/callback` to its callback URLs, retaining the old callback while
the previous revision drains. Set its setup URL to `https://mergeloop.ai/setup/installed`,
its homepage to `https://mergeloop.ai`, and its name to **MergeLoop**. Keep the existing
App ID, credentials, permissions and installations; do not create a replacement app. If the
name change changes its slug, verify its new public page. The orchestrator reads the current
slug and installation URL from GitHub at startup. GitHub App settings are separate from
the manifest used to create a new app; deploying the manifest does not update them.

See GitHub's [callback URL requirements](https://docs.github.com/en/apps/creating-github-apps/registering-a-github-app/about-the-user-authorization-callback-url)
and [app registration settings](https://docs.github.com/en/apps/maintaining-github-apps/modifying-a-github-app-registration).

The deployment sets `MERGEPATROL_PUBLIC_URL` to the new origin, rolls out the app, then
applies `routes.yaml` and `legacy-domain.yaml`. HTTPS pages on `mergepatrol.ai` redirect
with 302 to the same path and query at `mergeloop.ai`. Its HTTP pages go straight to the new
HTTPS origin too. The old HTTPS `/api`, `/mcp` and `/webhooks` endpoints still reach the
backend, preserving method, body and Authorization headers for existing clients and senders.

After deployment, verify a deep link with a query, sign-in, installation return, and a signed
webhook delivery. For example:

```sh
curl -I 'https://mergepatrol.ai/docs/configuration?source=old-link'
curl -I 'http://mergepatrol.ai/docs/configuration?source=old-link'
curl -I https://mergeloop.ai/
curl -i https://mergepatrol.ai/api/session
```

The first two return 302 with Location
`https://mergeloop.ai/docs/configuration?source=old-link`; the latter two return 200.
The new origin's `/auth/login` must send `redirect_uri=https://mergeloop.ai/auth/callback`
to GitHub, and sign-in must complete in a browser. People sign in again on the new origin:
the browser cannot carry a host-only session cookie between these domains.

The public GitHub App's name and the Stripe product's name (including receipts and customer
portal branding) also need updating in their owner settings. This repo sends no outbound
email itself. Keep the existing Stripe product/price IDs and subscription records.

Compatibility names intentionally retained in code: the GitHub repo, Rust packages
and binaries, container images and GCP resources, `MERGEPATROL_*` settings, `mergepatrol`
queue labels, `.mergepatrol/mergepatrol.yml`, API fields, Git refs, comment markers, the
`mergepatrol` skill name and installed paths, the menu bar app's bundle ID and Keychain
service, and the rolling `Mergepatrol.zip` download URL. Its bundle is now `MergeLoop.app`.
Existing social-card URLs serve the new artwork. Past changelog screenshots remain records
of the UI at the time. None of these identifiers should be renamed as a string replacement.
