#!/usr/bin/env bash
# Creates the hosted GitHub App, public so anyone can install it, and stores its credentials in
# Secret Manager, where cloudbuild.yaml hands them to the orchestrator. Run once, by an owner of
# the org: it opens GitHub's App form in the browser and catches the result on localhost, so the
# key never passes through a terminal or a file.
#
#   ORG=agent-labs-dev PROJECT=mergepatrol PUBLIC_URL=https://mergepatrol.ai deploy/gcp/hosted-app.sh
set -euo pipefail

ORG=${ORG:-agent-labs-dev}
PROJECT=${PROJECT:-mergepatrol}
PUBLIC_URL=${PUBLIC_URL:-https://mergepatrol.ai}

MANIFEST=$(cargo run -q -p mergepatrol -- hosted-manifest "$PUBLIC_URL")

python3 -I -u - "$ORG" "$PROJECT" "$MANIFEST" <<'PY'
import html, json, secrets, subprocess, sys, urllib.parse, urllib.request, webbrowser
from http.server import BaseHTTPRequestHandler, HTTPServer

org, project, manifest = sys.argv[1], sys.argv[2], json.loads(sys.argv[3])
state = secrets.token_urlsafe(24)
done = False

def store(name, value):
    exists = subprocess.run(["gcloud", "--project", project, "secrets", "describe", name],
                            capture_output=True).returncode == 0
    cmd = ["versions", "add", name] if exists else ["create", name]
    subprocess.run(["gcloud", "--project", project, "secrets", *cmd, "--data-file=-"],
                   input=value.encode(), check=True, capture_output=True)

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, body):
        self.send_response(200)
        self.send_header("content-type", "text/html")
        self.end_headers()
        self.wfile.write(body.encode())

    def do_GET(self):
        global done
        url = urllib.parse.urlparse(self.path)
        q = urllib.parse.parse_qs(url.query)
        if url.path == "/":
            action = f"https://github.com/organizations/{org}/settings/apps/new?state={state}"
            self.reply(f'<form id=f method=post action="{html.escape(action)}">'
                       f'<input type=hidden name=manifest value="{html.escape(manifest)}"></form>'
                       '<script>f.submit()</script>')
        elif url.path == "/callback" and q.get("state") == [state] and "code" in q:
            req = urllib.request.Request(
                f"https://api.github.com/app-manifests/{q['code'][0]}/conversions",
                method="POST", headers={"accept": "application/vnd.github+json"})
            app = json.load(urllib.request.urlopen(req))
            for name, key in [("app-id", "id"), ("app-private-key", "pem"), ("client-id", "client_id"),
                              ("client-secret", "client_secret"), ("webhook-secret", "webhook_secret")]:
                store(f"mergepatrol-github-{name}", str(app[key]))
            print(f"created {app['html_url']} (id {app['id']}); its credentials are in Secret Manager")
            self.reply(f"Created {html.escape(app['slug'])}. You can close this tab.")
            done = True
        else:
            self.send_error(404)

server = HTTPServer(("127.0.0.1", 0), Handler)
port = server.server_port
# GitHub sends the browser back here with a one-time code, instead of to the orchestrator's
# self-hosted setup, which the hosted orchestrator turns off.
manifest["redirect_url"] = f"http://localhost:{port}/callback"
manifest = json.dumps(manifest)
print(f"opening http://localhost:{port}/ to create the App under {org}")
webbrowser.open(f"http://localhost:{port}/")
while not done:
    server.handle_request()
PY
