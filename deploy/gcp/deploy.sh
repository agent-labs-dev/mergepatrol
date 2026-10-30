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
# The warm pool replaces its sandbox with one on the new image: once it is ready, the new job
# image starts, and sits on a node before the first job needs it.
ready_on_new_image() {
  kubectl -n mergepatrol-jobs get pods \
    -o jsonpath='{range .items[?(@.status.containerStatuses[0].ready==true)]}{.spec.containers[0].image}{"\n"}{end}' \
    | grep -qxF "$JOB_IMAGE"
}
for i in $(seq 120); do
  ready_on_new_image && break
  [ "$i" = 120 ] && { echo "no sandbox on $JOB_IMAGE got ready in 10 minutes" >&2; exit 1; }
  sleep 5
done

manifest orchestrator | kubectl apply -f -
kubectl -n mergepatrol rollout status deployment/mergepatrol --timeout=15m
