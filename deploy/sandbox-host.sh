#!/usr/bin/env bash
# Prepares a Debian/Ubuntu VM (a GCE VM in production) to run Mergepatrol's jobs:
#   - gVisor (runsc) as a Docker runtime: each job gets its own user-space kernel.
#   - A Docker network for jobs, whose traffic may reach the internet but not the cloud
#     metadata server, private ranges, or this host.
# Run as root. Safe to run again (for example from the VM's startup script).
# Then set MERGEPATROL_JOB_RUNTIME=runsc, MERGEPATROL_JOB_NETWORK=mergepatrol-jobs, and
# MERGEPATROL_JOB_DNS=1.1.1.1,8.8.8.8 (the firewall blocks Docker's embedded DNS).
set -euo pipefail

NETWORK=mergepatrol-jobs
SUBNET=10.213.0.0/24

if ! command -v runsc >/dev/null; then
  curl -fsSL https://gvisor.dev/archive.key | gpg --dearmor -o /usr/share/keyrings/gvisor-archive-keyring.gpg
  echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/gvisor-archive-keyring.gpg] https://storage.googleapis.com/gvisor/releases release main" \
    > /etc/apt/sources.list.d/gvisor.list
  apt-get update && apt-get install -y runsc
fi
if ! docker info --format '{{json .Runtimes}}' | grep -q runsc; then
  runsc install
  systemctl restart docker
fi

if ! docker network inspect "$NETWORK" >/dev/null 2>&1; then
  docker network create --driver bridge --subnet "$SUBNET" \
    -o com.docker.network.bridge.name=mp-jobs "$NETWORK"
fi

# Jobs to the outside: no metadata server, no private or shared ranges.
for dst in 169.254.0.0/16 10.0.0.0/8 172.16.0.0/12 192.168.0.0/16 100.64.0.0/10; do
  iptables -C DOCKER-USER -s "$SUBNET" -d "$dst" -j DROP 2>/dev/null \
    || iptables -I DOCKER-USER -s "$SUBNET" -d "$dst" -j DROP
done
# Jobs to this host (the Docker API, the dashboard, anything listening): no.
iptables -C INPUT -i mp-jobs -j DROP 2>/dev/null || iptables -I INPUT -i mp-jobs -j DROP

echo "ready: MERGEPATROL_JOB_RUNTIME=runsc MERGEPATROL_JOB_NETWORK=$NETWORK MERGEPATROL_JOB_DNS=1.1.1.1,8.8.8.8"
