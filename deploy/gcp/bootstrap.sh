#!/usr/bin/env bash
# Sets up Mergepatrol's one environment on GCP, then deploys it: a GKE Autopilot cluster that
# runs the orchestrator, and each rebase job in a gVisor sandbox from an Agent Sandbox warm pool.
# Safe to run again.
#
#   PROJECT=mergepatrol REGION=us-west1 PUBLIC_URL=https://mergepatrol.ai deploy/gcp/bootstrap.sh
#
# The project must exist with billing linked. After this, deploys run from cloudbuild.yaml.
set -euo pipefail

PROJECT=${PROJECT:-mergepatrol}
REGION=${REGION:-us-west1}
PUBLIC_URL=${PUBLIC_URL:-https://mergepatrol.ai}
# Pushes to this repo's main deploy, through GitHub Actions.
REPO=${REPO:-agent-labs-dev/mergepatrol}
SQL=mergepatrol
DB=mergepatrol
DB_USER=mergepatrol
# kubernetes-sigs/agent-sandbox: the SandboxTemplate, SandboxWarmPool and SandboxClaim kinds.
AGENT_SANDBOX=v1.0.6
HOST=${PUBLIC_URL#https://}

g() { gcloud --project="$PROJECT" "$@"; }
secret() { g secrets describe "$1" >/dev/null 2>&1; }
# A new service account takes a moment to show up in IAM, so a grant on one retries.
grant() { for _ in 1 2 3 4 5 6; do g "$@" >/dev/null && return; sleep 10; done; return 1; }

g services enable container.googleapis.com compute.googleapis.com certificatemanager.googleapis.com \
  sqladmin.googleapis.com artifactregistry.googleapis.com cloudbuild.googleapis.com \
  secretmanager.googleapis.com iam.googleapis.com iamcredentials.googleapis.com sts.googleapis.com

g artifacts repositories describe mergepatrol --location="$REGION" >/dev/null 2>&1 \
  || g artifacts repositories create mergepatrol --location="$REGION" --repository-format=docker
# Each deploy pushes both images and the build's layer cache. Older than a week goes, but every
# image keeps its last 10 versions, which also keeps the current cache.
g artifacts repositories set-cleanup-policies mergepatrol --location="$REGION" \
  --policy="$(dirname "$0")/cleanup-policy.json" --no-dry-run

# The orchestrator holds the GitHub App key and starts jobs. The cluster's nodes run as an
# account that can only pull images and write logs, not as the default compute account, which
# deploys. A job runs untrusted repo code and an agent with a shell: it has no account, and its
# network policy keeps it from the metadata server.
for sa in mergepatrol mergepatrol-node mergepatrol-deploy; do
  g iam service-accounts describe "$sa@$PROJECT.iam.gserviceaccount.com" >/dev/null 2>&1 \
    || g iam service-accounts create "$sa"
done
ORCH="serviceAccount:mergepatrol@$PROJECT.iam.gserviceaccount.com"
for role in roles/cloudsql.client roles/secretmanager.secretAccessor; do
  grant projects add-iam-policy-binding "$PROJECT" --member="$ORCH" --role="$role" --condition=None
done
NODE="serviceAccount:mergepatrol-node@$PROJECT.iam.gserviceaccount.com"
for role in roles/container.defaultNodeServiceAccount roles/artifactregistry.reader; do
  grant projects add-iam-policy-binding "$PROJECT" --member="$NODE" --role="$role" --condition=None
done

# Cloud Build deploys both to the cluster.
NUMBER=$(g projects describe "$PROJECT" --format='value(projectNumber)')
BUILD="serviceAccount:$NUMBER-compute@developer.gserviceaccount.com"
for role in roles/container.developer roles/artifactregistry.writer roles/logging.logWriter \
  roles/storage.objectViewer; do
  grant projects add-iam-policy-binding "$PROJECT" --member="$BUILD" --role="$role" --condition=None
done

# GitHub Actions on main signs in as mergepatrol-deploy with its OIDC token, so no key exists.
# The condition pins the repo by id, which a rename or a new repo with the same name can't take.
REPO_ID=$(gh api "repos/$REPO" --jq .id)
DEPLOY=mergepatrol-deploy@$PROJECT.iam.gserviceaccount.com
g iam workload-identity-pools describe github --location=global >/dev/null 2>&1 \
  || g iam workload-identity-pools create github --location=global
g iam workload-identity-pools providers describe github --location=global \
  --workload-identity-pool=github >/dev/null 2>&1 \
  || g iam workload-identity-pools providers create-oidc github --location=global \
    --workload-identity-pool=github --issuer-uri=https://token.actions.githubusercontent.com \
    --attribute-mapping=google.subject=assertion.sub,attribute.repository_id=assertion.repository_id \
    --attribute-condition="assertion.repository_id=='$REPO_ID' && assertion.ref=='refs/heads/main'"
grant iam service-accounts add-iam-policy-binding "$DEPLOY" --role=roles/iam.workloadIdentityUser \
  --member="principalSet://iam.googleapis.com/projects/$NUMBER/locations/global/workloadIdentityPools/github/attribute.repository_id/$REPO_ID"
# Submits the build and streams its log; the build itself deploys, as the account above.
for role in roles/cloudbuild.builds.editor roles/serviceusage.serviceUsageConsumer roles/storage.admin \
  roles/viewer; do
  grant projects add-iam-policy-binding "$PROJECT" --member="serviceAccount:$DEPLOY" --role="$role" --condition=None
done
grant iam service-accounts add-iam-policy-binding "${BUILD#serviceAccount:}" \
  --member="serviceAccount:$DEPLOY" --role=roles/iam.serviceAccountUser

if ! g sql instances describe "$SQL" >/dev/null 2>&1; then
  g sql instances create "$SQL" --database-version=POSTGRES_17 --edition=enterprise \
    --tier=db-f1-micro --region="$REGION" --storage-auto-increase --backup
fi
g sql databases describe "$DB" --instance="$SQL" >/dev/null 2>&1 || g sql databases create "$DB" --instance="$SQL"
if ! secret mergepatrol-database-url; then
  PASSWORD=$(openssl rand -hex 24)
  g sql users create "$DB_USER" --instance="$SQL" --password="$PASSWORD"
  # sqlx reads a `host` that starts with `/` as the directory of a Unix socket.
  printf 'postgres://%s:%s@localhost/%s?host=/cloudsql/%s:%s:%s' \
    "$DB_USER" "$PASSWORD" "$DB" "$PROJECT" "$REGION" "$SQL" \
    | g secrets create mergepatrol-database-url --data-file=-
fi
# Encrypts the GitHub App and the model API keys in the database. Without it, set up again.
secret mergepatrol-secret-key \
  || openssl rand -base64 32 | tr -d '\n' | g secrets create mergepatrol-secret-key --data-file=-

# The cluster. Autopilot runs gVisor without node pools to manage, and the Secret Manager add-on
# hands the orchestrator its secrets as files, read with its own account.
if ! g container clusters describe mergepatrol --location="$REGION" >/dev/null 2>&1; then
  g container clusters create-auto mergepatrol --location="$REGION" --release-channel=regular \
    --service-account="${NODE#serviceAccount:}"
fi
g container clusters update mergepatrol --location="$REGION" --enable-secret-manager --quiet
g container clusters get-credentials mergepatrol --location="$REGION"
# The orchestrator's Kubernetes account signs in to GCP as mergepatrol@.
grant iam service-accounts add-iam-policy-binding "${ORCH#serviceAccount:}" --role=roles/iam.workloadIdentityUser \
  --member="serviceAccount:$PROJECT.svc.id.goog[mergepatrol/mergepatrol]"
kubectl apply --server-side \
  -f "https://github.com/kubernetes-sigs/agent-sandbox/releases/download/$AGENT_SANDBOX/sandbox-with-extensions.yaml"
kubectl -n agent-sandbox-system rollout status deployment/agent-sandbox-controller --timeout=5m

# The load balancer's address and certificate. Certificate Manager proves the domain with a DNS
# record, so the certificate is ready before the domain points here.
g compute addresses describe mergepatrol --global >/dev/null 2>&1 \
  || g compute addresses create mergepatrol --global
g certificate-manager dns-authorizations describe mergepatrol >/dev/null 2>&1 \
  || g certificate-manager dns-authorizations create mergepatrol --domain="$HOST"
g certificate-manager certificates describe mergepatrol >/dev/null 2>&1 \
  || g certificate-manager certificates create mergepatrol --domains="$HOST" --dns-authorizations=mergepatrol
g certificate-manager maps describe mergepatrol >/dev/null 2>&1 \
  || g certificate-manager maps create mergepatrol
g certificate-manager maps entries describe mergepatrol --map=mergepatrol >/dev/null 2>&1 \
  || g certificate-manager maps entries create mergepatrol --map=mergepatrol --hostname="$HOST" \
    --certificates=mergepatrol
sed -e "s#__PROJECT__#$PROJECT#g" -e "s#__REGION__#$REGION#g" -e "s#__HOST__#$HOST#g" \
  "$(dirname "$0")/k8s/cluster.yaml" | kubectl apply -f -

# The hosted GitHub App: made once, in the browser.
secret mergepatrol-github-app-id || ORG="${REPO%%/*}" PROJECT="$PROJECT" PUBLIC_URL="$PUBLIC_URL" \
  deploy/gcp/hosted-app.sh

g builds submit --region="$REGION" --config=deploy/gcp/cloudbuild.yaml \
  --substitutions="_REGION=$REGION,_PUBLIC_URL=$PUBLIC_URL" .

ADDRESS=$(g compute addresses describe mergepatrol --global --format='value(address)')
CHALLENGE_NAME=$(g certificate-manager dns-authorizations describe mergepatrol --format='value(dnsResourceRecord.name)')
CHALLENGE_DATA=$(g certificate-manager dns-authorizations describe mergepatrol --format='value(dnsResourceRecord.data)')
cat <<EOF

Deployed. Left to do by hand, in the domain's DNS, as DNS only records: a Cloudflare proxy
blocks Google's certificate.
  1. A CNAME that proves the domain for the certificate: $CHALLENGE_NAME -> $CHALLENGE_DATA
     Then wait until this says ACTIVE:
       gcloud certificate-manager certificates describe mergepatrol --project=$PROJECT
  2. An A record: $HOST -> $ADDRESS
  3. Open $PUBLIC_URL, install the App, and sign in.
After this, every push to main of $REPO deploys (.github/workflows/deploy.yml).
EOF
