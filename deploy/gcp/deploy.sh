#!/usr/bin/env bash
# Rolls out one build to the GKE cluster `mergepatrol`: the job sandboxes first, since a new
# orchestrator may send a job what only a new job reads, then the orchestrator. Run by
# cloudbuild.yaml, as its service account, with the images it built.
#
#   PROJECT=mergepatrol REGION=us-west1 PUBLIC_URL=https://mergepatrol.ai \
#     JOB_IMAGE=... ORCHESTRATOR_IMAGE=... POSTHOG_KEY=... STRIPE_PRICE=... deploy/gcp/deploy.sh
set -euo pipefail
: "${PROJECT:?}" "${REGION:?}" "${PUBLIC_URL:?}" "${JOB_IMAGE:?}" "${ORCHESTRATOR_IMAGE:?}"
: "${POSTHOG_KEY:?}" "${STRIPE_PRICE:?}"

gcloud container clusters get-credentials mergepatrol --project="$PROJECT" --location="$REGION"

# A manifest under k8s/, with the placeholders filled in.
manifest() {
  sed -e "s#__PROJECT__#$PROJECT#g" -e "s#__REGION__#$REGION#g" -e "s#__PUBLIC_URL__#$PUBLIC_URL#g" \
    -e "s#__JOB_IMAGE__#$JOB_IMAGE#g" -e "s#__ORCHESTRATOR_IMAGE__#$ORCHESTRATOR_IMAGE#g" \
    -e "s#__POSTHOG_KEY__#$POSTHOG_KEY#g" -e "s#__STRIPE_PRICE__#$STRIPE_PRICE#g" \
    "$(dirname "$0")/k8s/$1.yaml"
}

manifest jobs | kubectl apply -f -
# The warm pool replaces its sandbox with one on the new image, which also pulls the image onto
# a node before the first job needs it.
kubectl -n mergepatrol-jobs wait sandboxwarmpool/mergepatrol-job \
  --for=jsonpath='{.status.readyReplicas}'=1 --timeout=10m

manifest orchestrator | kubectl apply -f -
kubectl -n mergepatrol rollout status deployment/mergepatrol --timeout=15m
