# Builds both images in one BuildKit, so they share the dashboard and Rust stages.
# cloudbuild.yaml runs it from the repository root. Each Cloud Build VM starts empty, so the
# layer cache lives in the registry, one ref per image.

variable "REPO" {}
variable "TAG" {}
variable "CLAUDE_CODE_VERSION" { default = "latest" }
variable "CODEX_VERSION" { default = "latest" }

group "default" {
  targets = ["orchestrator", "job"]
}

target "_common" {
  context    = "."
  dockerfile = "Dockerfile"
  # A plain image, not an index with an attestation: one registry version per deploy, which the
  # cleanup policy counts.
  attest     = ["type=provenance,disabled=true"]
  cache-from = [
    "type=registry,ref=${REPO}/cache:orchestrator",
    "type=registry,ref=${REPO}/cache:job",
  ]
}

target "orchestrator" {
  inherits = ["_common"]
  target   = "orchestrator"
  tags     = ["${REPO}/orchestrator:${TAG}"]
  cache-to = ["type=registry,ref=${REPO}/cache:orchestrator,mode=max,image-manifest=true,oci-mediatypes=true"]
}

target "job" {
  inherits = ["_common"]
  target   = "job"
  tags     = ["${REPO}/job:${TAG}"]
  cache-to = ["type=registry,ref=${REPO}/cache:job,mode=max,image-manifest=true,oci-mediatypes=true"]
  args = {
    CLAUDE_CODE_VERSION = CLAUDE_CODE_VERSION
    CODEX_VERSION       = CODEX_VERSION
  }
}
