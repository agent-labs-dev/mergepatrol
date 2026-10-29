#!/usr/bin/env bash
# Sets up Mergepatrol's one environment on GCP, then deploys it: the orchestrator as a Cloud
# Run service, and each rebase job as an execution of a Cloud Run Job. Safe to run again.
#
#   PROJECT=mergepatrol REGION=us-west1 PUBLIC_URL=https://merge.bugpatrol.ai deploy/gcp/bootstrap.sh
#
# The project must exist with billing linked. After this, deploys run from cloudbuild.yaml.
set -euo pipefail

PROJECT=${PROJECT:-mergepatrol}
# A region with Cloud Run domain mappings, near nebula's us-west2.
REGION=${REGION:-us-west1}
PUBLIC_URL=${PUBLIC_URL:-https://merge.bugpatrol.ai}
SQL=mergepatrol
DB=mergepatrol
DB_USER=mergepatrol

g() { gcloud --project="$PROJECT" "$@"; }
secret() { g secrets describe "$1" >/dev/null 2>&1; }
# A new service account takes a moment to show up in IAM, so a grant on one retries.
grant() { for _ in 1 2 3 4 5 6; do g "$@" >/dev/null && return; sleep 10; done; return 1; }

g services enable run.googleapis.com sqladmin.googleapis.com artifactregistry.googleapis.com \
  cloudbuild.googleapis.com secretmanager.googleapis.com iam.googleapis.com

g artifacts repositories describe mergepatrol --location="$REGION" >/dev/null 2>&1 \
  || g artifacts repositories create mergepatrol --location="$REGION" --repository-format=docker

# The orchestrator holds the GitHub App key and starts jobs. A job runs untrusted repo code and
# an agent with a shell, so its account has no roles at all: the token it can get from the
# metadata server opens nothing.
for sa in mergepatrol mergepatrol-job; do
  g iam service-accounts describe "$sa@$PROJECT.iam.gserviceaccount.com" >/dev/null 2>&1 \
    || g iam service-accounts create "$sa"
done
ORCH="serviceAccount:mergepatrol@$PROJECT.iam.gserviceaccount.com"
for role in roles/cloudsql.client roles/secretmanager.secretAccessor roles/run.developer; do
  grant projects add-iam-policy-binding "$PROJECT" --member="$ORCH" --role="$role" --condition=None
done
grant iam service-accounts add-iam-policy-binding "mergepatrol-job@$PROJECT.iam.gserviceaccount.com" \
  --member="$ORCH" --role=roles/iam.serviceAccountUser

# Cloud Build deploys both, as their service accounts.
NUMBER=$(g projects describe "$PROJECT" --format='value(projectNumber)')
BUILD="serviceAccount:$NUMBER-compute@developer.gserviceaccount.com"
for role in roles/run.admin roles/artifactregistry.writer roles/logging.logWriter roles/iam.serviceAccountUser \
  roles/storage.objectViewer; do
  grant projects add-iam-policy-binding "$PROJECT" --member="$BUILD" --role="$role" --condition=None
done

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
# Encrypts the GitHub App and the Anthropic keys in the database. Without it, set up again.
secret mergepatrol-secret-key \
  || openssl rand -base64 32 | tr -d '\n' | g secrets create mergepatrol-secret-key --data-file=-

g builds submit --region="$REGION" --config=deploy/gcp/cloudbuild.yaml \
  --substitutions="_REGION=$REGION,_PUBLIC_URL=$PUBLIC_URL" .

cat <<EOF

Deployed. Left to do by hand:
  1. Map the domain: gcloud beta run domain-mappings create --project=$PROJECT --region=$REGION \\
       --service=mergepatrol --domain=${PUBLIC_URL#https://}
     The domain must be verified for your Google account first (Search Console). Then point a
     CNAME at ghs.googlehosted.com, DNS only: a Cloudflare proxy blocks Google's certificate.
  2. Read the setup token: gcloud logging read --project=$PROJECT \\
       'resource.labels.service_name="mergepatrol" AND jsonPayload.setup_token:*' --limit=1
     and open $PUBLIC_URL to create the GitHub App.
  3. Deploy on every push to main: connect agent-labs-dev/mergepatrol to Cloud Build, and add a
     trigger that runs deploy/gcp/cloudbuild.yaml with _REGION=$REGION and _PUBLIC_URL=$PUBLIC_URL.
EOF
