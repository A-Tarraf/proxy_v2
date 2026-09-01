#!/bin/bash
# Malleability experiment for metric proxy v2.
#
# Tests three scenarios against a 4-node Docker Swarm cluster (dmr01-04):
#   GRACEFUL     — SIGTERM to a child proxy; /leave endpoint triggers immediate repair
#   SHRINK       — SIGKILL a child proxy; TBON self-repair closes the gap
#   EXPAND       — new proxy discovers root via shared root.url and auto-joins the TBON
#
# Usage:
#   bash experiment/run_malleability_test.sh
#
# The script checks prerequisites automatically:
#   - Docker containers dmr01..dmr04 must be running
#   - If the proxy binary is missing it is built inside dmr01 (cargo build --release)
#   - Shared volumes: /opt/hpc/build  (source + binaries)
#                     /opt/hpc/install (runtime data, used for shared root.url)

set -euo pipefail

PROXY=/opt/hpc/build/proxy_v2/target/release/proxy_v2
PROXY_RUN=/opt/hpc/build/proxy_v2/target/release/proxy_run
ROOT_NODE=dmr01
ROOT_PORT=1337
ROOT_URL="${ROOT_NODE}:${ROOT_PORT}"

# Log dir must be a path that EXISTS INSIDE the containers (shared volume).
# Experiment output is then pulled back to the host after the run.
LOGDIR=/opt/hpc/build/proxy_v2/experiment/logs

# Root proxy writes root.url here; child proxies with --auto-root read it from here.
# Must be on a shared filesystem volume visible to all nodes.
SHARED_PROFILEDIR=/opt/hpc/install/proxy_profiles
ROOT_PROFILEDIR="${SHARED_PROFILEDIR}/root"

log() { echo "[$(date +%H:%M:%S)] $*"; }

# -------------------------------------------------------
# PREREQUISITES CHECK
# -------------------------------------------------------
log "=== Checking prerequisites ==="

# 1. Verify all Docker containers are running
for node in dmr01 dmr02 dmr03 dmr04; do
  if ! docker inspect --format '{{.State.Running}}' "$node" 2>/dev/null | grep -q "^true$"; then
    echo "ERROR: container '$node' is not running."
    echo "  Start the cluster with your docker-compose / docker stack command first."
    exit 1
  fi
done
log "Docker containers dmr01-dmr04: OK"

# 2. Build proxy if binary is missing
if ! docker exec dmr01 bash -c "test -x ${PROXY}" 2>/dev/null; then
  log "Proxy binary not found — building inside dmr01 (this may take a few minutes) ..."
  docker exec dmr01 bash -c "cd /opt/hpc/build/proxy_v2 && cargo build --release 2>&1"
  if ! docker exec dmr01 bash -c "test -x ${PROXY}" 2>/dev/null; then
    echo "ERROR: Build failed — ${PROXY} still missing after cargo build."
    exit 1
  fi
  log "Build complete."
else
  log "Proxy binary: OK"
fi

# 3. Create log dir inside the cluster
docker exec dmr01 bash -c "mkdir -p ${LOGDIR}"

# -------------------------------------------------------
# CLEANUP helper (also called on exit)
# -------------------------------------------------------
cleanup() {
  log "=== Cleanup: stopping all proxy and workload processes ==="
  for node in dmr01 dmr02 dmr03 dmr04; do
    docker exec "$node" bash -c "pkill proxy_v2 || true; pkill proxy_run || true" 2>/dev/null || true
  done
  # Remove shared profile dirs so next run starts clean
  docker exec dmr01 bash -c "rm -rf ${SHARED_PROFILEDIR}" 2>/dev/null || true
  # Copy logs back to host for inspection
  docker cp dmr01:"${LOGDIR}/." "$(dirname "${BASH_SOURCE[0]}")/logs/" 2>/dev/null || true
}
trap cleanup EXIT

# -------------------------------------------------------
# STEP 1: Start root proxy on dmr01
# -------------------------------------------------------
log "=== STEP 1: Starting root proxy on ${ROOT_NODE} ==="
docker exec -d dmr01 bash -c "
  mkdir -p ${ROOT_PROFILEDIR}
  ${PROXY} --port ${ROOT_PORT} --target-prefix ${ROOT_PROFILEDIR} \
    > ${LOGDIR}/proxy_root.log 2>&1
"
sleep 4  # give root time to bind and write root.url

if docker exec dmr01 bash -c "curl -sf http://${ROOT_URL}/is_admire_proxy.html > /dev/null 2>&1"; then
  log "Root proxy UP at ${ROOT_URL}"
else
  log "ERROR: Root proxy did not start"; exit 1
fi

ROOTURL_CONTENT=$(docker exec dmr01 bash -c "cat ${ROOT_PROFILEDIR}/root.url" 2>/dev/null || echo "<missing>")
log "root.url content: ${ROOTURL_CONTENT}"

# -------------------------------------------------------
# STEP 2: Start child proxies on dmr02, dmr03, dmr04
# -------------------------------------------------------
log "=== STEP 2: Starting child proxies ==="
for node in dmr02 dmr03 dmr04; do
  docker exec -d "$node" bash -c "
    mkdir -p ${SHARED_PROFILEDIR}/${node}
    ${PROXY} --port ${ROOT_PORT} \
      --target-prefix ${SHARED_PROFILEDIR}/${node} \
      --root-proxy ${ROOT_URL} \
      > ${LOGDIR}/proxy_${node}.log 2>&1
  "
done
sleep 6  # wait for all children to join

log "=== TBON topology after startup ==="
docker exec dmr01 bash -c "curl -sf http://${ROOT_URL}/topo 2>/dev/null | python3 -m json.tool" || true

# -------------------------------------------------------
# STEP 3: Run a background I/O workload on all nodes
# -------------------------------------------------------
log "=== STEP 3: Starting background I/O workload ==="
for node in dmr01 dmr02 dmr03 dmr04; do
  docker exec -d "$node" bash -c "
    ${PROXY_RUN} -j malleable_test -- bash -c 'while true; do dd if=/dev/zero of=/tmp/testio bs=1M count=10 2>/dev/null; sleep 1; done'
  "
done
sleep 5
log "Jobs visible at root:"
docker exec dmr01 bash -c "curl -sf 'http://${ROOT_URL}/job/list' 2>/dev/null | python3 -m json.tool" | grep -i jobid || true

# -------------------------------------------------------
# STEP 4: GRACEFUL LEAVE — SIGTERM dmr03 proxy
# Expect: proxy sends /leave before dying → immediate repair, well under 1 s scrape timeout
# -------------------------------------------------------
log ""
log "=== STEP 4: GRACEFUL LEAVE TEST — SIGTERM to dmr03 proxy ==="
log "Topology BEFORE:"
docker exec dmr01 bash -c "curl -sf http://${ROOT_URL}/topo 2>/dev/null | python3 -m json.tool" || true

T_START=$(date +%s%3N)
docker exec dmr03 bash -c "pkill -SIGTERM proxy_v2 || true" 2>/dev/null
sleep 2
T_END=$(date +%s%3N)
ELAPSED=$((T_END - T_START))

TOPO=$(docker exec dmr01 bash -c "curl -sf http://${ROOT_URL}/topo 2>/dev/null")
if echo "$TOPO" | grep -q "dmr03"; then
  log "  [FAIL] dmr03 still present after ${ELAPSED}ms — graceful leave did not fire"
else
  log "  [PASS] GRACEFUL LEAVE: dmr03 removed in ${ELAPSED}ms (scrape timeout would be ~1000ms)"
fi
log "Topology AFTER graceful leave:"
docker exec dmr01 bash -c "curl -sf http://${ROOT_URL}/topo 2>/dev/null | python3 -m json.tool" || true
echo ""

# -------------------------------------------------------
# STEP 5: SHRINK TEST — abrupt kill dmr04 proxy (no SIGTERM)
# Expect: root detects missing scrape within ~sampling-period ms, removes dmr04
# -------------------------------------------------------
log "=== STEP 5: SHRINK TEST — SIGKILL dmr04 proxy (no graceful leave) ==="
log "Topology BEFORE:"
docker exec dmr01 bash -c "curl -sf http://${ROOT_URL}/topo 2>/dev/null | python3 -m json.tool" || true

T_KILL=$(date +%s%3N)
docker exec dmr04 bash -c "pkill -SIGKILL proxy_v2 || true" 2>/dev/null
log "dmr04 killed (SIGKILL) — polling for TBON self-repair ..."

REPAIR_DONE=0
for i in $(seq 1 20); do
  sleep 1
  TOPO=$(docker exec dmr01 bash -c "curl -sf http://${ROOT_URL}/topo 2>/dev/null")
  if echo "$TOPO" | grep -q "dmr04"; then
    log "  [${i}s] dmr04 still in topology"
  else
    T_DONE=$(date +%s%3N)
    REPAIR_MS=$((T_DONE - T_KILL))
    log "  [PASS] SHRINK REPAIR COMPLETE: dmr04 removed in ${REPAIR_MS}ms"
    REPAIR_DONE=1
    break
  fi
done
[ "$REPAIR_DONE" -eq 0 ] && log "  [FAIL] dmr04 still present after 20s — repair did not complete"

log "Topology AFTER repair:"
docker exec dmr01 bash -c "curl -sf http://${ROOT_URL}/topo 2>/dev/null | python3 -m json.tool" || true
log "Root still collecting metrics:"
docker exec dmr01 bash -c "curl -sf http://${ROOT_URL}/metrics 2>/dev/null | head -5" || true
echo ""

# -------------------------------------------------------
# STEP 6: EXPAND TEST — restart dmr04 with --auto-root
# root.url is on the shared volume; dmr04 reads it and self-registers
# -------------------------------------------------------
log "=== STEP 6: EXPAND TEST — dmr04 rejoins via --auto-root ==="
log "Topology BEFORE expand:"
docker exec dmr01 bash -c "curl -sf http://${ROOT_URL}/topo 2>/dev/null | python3 -m json.tool" || true

T_EXPAND=$(date +%s%3N)
docker exec -d dmr04 bash -c "
  mkdir -p ${SHARED_PROFILEDIR}/dmr04_v2
  ${PROXY} --port ${ROOT_PORT} \
    --target-prefix ${SHARED_PROFILEDIR}/dmr04_v2 \
    --auto-root \
    --root-url-dir ${ROOT_PROFILEDIR} \
    > ${LOGDIR}/proxy_dmr04_expand.log 2>&1
"

log "Waiting for dmr04 to auto-join TBON ..."
JOIN_DONE=0
for i in $(seq 1 15); do
  sleep 1
  TOPO=$(docker exec dmr01 bash -c "curl -sf http://${ROOT_URL}/topo 2>/dev/null")
  if echo "$TOPO" | grep -q "dmr04"; then
    T_JOINED=$(date +%s%3N)
    JOIN_MS=$((T_JOINED - T_EXPAND))
    log "  [PASS] AUTO-JOIN COMPLETE: dmr04 back in TBON in ${JOIN_MS}ms"
    JOIN_DONE=1
    break
  else
    log "  [${i}s] dmr04 not yet in topology"
  fi
done
[ "$JOIN_DONE" -eq 0 ] && log "  [FAIL] dmr04 did not rejoin after 15s"

log "Final topology:"
docker exec dmr01 bash -c "curl -sf http://${ROOT_URL}/topo 2>/dev/null | python3 -m json.tool" || true

log "=== Experiment complete. Logs in ${LOGDIR} ==="
