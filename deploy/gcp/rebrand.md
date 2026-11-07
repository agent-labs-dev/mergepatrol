# MergeLoop domain cutover

The hosted app's canonical URL is `https://mergeloop.ai`. Both domains keep their DNS
records and certificates on the `mergepatrol` Gateway; the only DNS change is turning on
Cloudflare's proxy for `mergepatrol.ai`, below.

Before deploying this change, an owner of the existing GitHub App (`merge-patrol`) must add
`https://mergeloop.ai/auth/callback` to its callback URLs: the new revision sends it as the
`redirect_uri`, and GitHub refuses one it does not list. Keep the old callback until the
previous revision has drained. Rename the App to **MergeLoop** before deploying too: the
orchestrator reads the App's slug, and so its installation URL, from GitHub at startup, so
the rollout picks up a changed slug. After the deployment is live, set its setup URL to
`https://mergeloop.ai/setup/installed` and its homepage to `https://mergeloop.ai`. Keep the
existing App ID, credentials, permissions and installations; do not create a replacement
app. GitHub App settings are separate from the manifest used to create a new app; deploying
the manifest does not update them.

See GitHub's [callback URL requirements](https://docs.github.com/en/apps/creating-github-apps/registering-a-github-app/about-the-user-authorization-callback-url)
and [app registration settings](https://docs.github.com/en/apps/maintaining-github-apps/modifying-a-github-app-registration).

The deployment sets `MERGEPATROL_PUBLIC_URL` to the new origin, rolls out the app, then
applies `routes.yaml` and `legacy-domain.yaml`. The Gateway keeps serving `mergepatrol.ai`
as is: the old HTTPS `/api`, `/mcp` and `/webhooks` endpoints must keep their method, body
and Authorization header, which a redirect would drop.

Cloudflare redirects the old domain's pages, once the deployment is live. In the
`mergepatrol.ai` zone:

1. SSL/TLS: set the encryption mode to **Full (strict)**. Google's certificate is valid for
   `mergepatrol.ai`, and Flexible would send Cloudflare's requests to the HTTP listener.
2. DNS: turn the proxy on (orange cloud) for the `mergepatrol.ai` A record. Leave the
   certificate's `_acme-challenge` CNAME DNS only, or Google cannot renew the certificate.
3. Rules > Redirect Rules: add a rule with this expression:

   ```
   (http.host eq "mergepatrol.ai" and not starts_with(http.request.uri.path, "/api")
    and not starts_with(http.request.uri.path, "/mcp")
    and not starts_with(http.request.uri.path, "/webhooks"))
   ```

   Its URL redirect is Dynamic, to `concat("https://mergeloop.ai", http.request.uri.path)`,
   status 302, with **Preserve query string** on. Make it 301 once the new domain has settled.

The proxy also sends GitHub's webhooks through Cloudflare, so Bot Fight Mode must stay off in
this zone. Pointing the App's webhook URL at `https://mergeloop.ai/webhooks` removes that
dependency.

After the rule is on, verify a deep link with a query, sign-in, installation return, and a
signed webhook delivery. For example:

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

The GitHub repo is now `agent-labs-dev/mergeloop`; GitHub redirects the old name, and the
deploy's workload identity matches the repo's ID, not its name. Releases name their
container images after the repo, so the next one publishes `ghcr.io/agent-labs-dev/mergeloop`
and `-job`; the `mergepatrol` images keep their tags but get no new ones.

What users see says `mergeloop`: new repos get `.mergeloop/mergeloop.yml` and the `mergeloop`
labels, and MergeLoop's branches, commit trailers, comment markers, agent skill and API fields
use the new name. MergeLoop still reads what it wrote as Mergepatrol: a config file at
`.mergepatrol/mergepatrol.yml`, which keeps `mergepatrol` as its default label, `Fixed-by:` and
`Resolved-by: mergepatrol` trailers, and its old comment markers. Updating a repo's skill moves
it from `skills/mergepatrol`. Internal names stay: Rust packages and binaries, the Artifact
Registry images and GCP resources, `MERGEPATROL_*` settings, Git refs under the queue's cache, the
menu bar app's bundle ID and Keychain service. Existing social-card URLs moved to
`/og/mergeloop-og-*`. Past changelog screenshots remain records of the UI at the time.
