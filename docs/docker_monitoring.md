# Monitoring the DMR Docker Cluster with Metric Proxy

## Quick start

```bash
# 1. Start cluster
cd /d/benchmark/docker-cluster-dmrv2
docker ps -a --filter "name=dmr" -q | xargs -r docker rm -f
bash start.sh -n 9

# 2. Start FTIO (on host — see FTIO section below)
/d/github/FTIO/.venv/bin/admire_proxy_zmq > /tmp/ftio_zmq_host.log 2>&1 &
FTIO_PORT=$(until grep -m1 'tcp://' /tmp/ftio_zmq_host.log; do sleep 0.5; done | grep -oP ':\K\d+')
# Write a persistent wrapper into the shared volume so dmr01 can find it
cat > /e/dmr/docker-cluster-dmrv2/build/metric-proxy/bin/admire_proxy_zmq << EOF
#!/bin/bash
echo 'tcp://172.17.0.1:${FTIO_PORT}'
exec tail -f /tmp/ftio_zmq_host.log 2>/dev/null || sleep infinity
EOF
chmod +x /e/dmr/docker-cluster-dmrv2/build/metric-proxy/bin/admire_proxy_zmq

# 3. Start proxies
PROXY=/opt/hpc/build/metric-proxy/bin/proxy_v2
MPATH=/opt/hpc/build/metric-proxy/bin
docker ps --filter "name=dmr" --format "{{.Names}}" | xargs -I{} docker exec {} pkill proxy_v2 2>/dev/null; true
docker exec -d -u mpiuser dmr01 env PATH=$MPATH:/usr/local/bin:/usr/bin $PROXY --port 1337 -S 100
docker ps --filter "name=dmr" --format "{{.Names}}" | grep -v -E "dmr01$|dmr01-ftio" | xargs -I{} \
    docker exec -d -u mpiuser {} env PATH=$MPATH:/usr/local/bin:/usr/bin \
    $PROXY --port 1338 --root-proxy dmr01:1337 -S 100

# 4. Open Chrome → http://localhost:1337

# 5. Submit job
bash /d/benchmark/docker-cluster-dmrv2/mpiuser-drop-in.sh
# inside:
cd /opt/hpc/build/dmr/examples/hacc-io
sbatch test.sh && squeue

# 6. Cleanup
docker ps --filter "name=dmr" --format "{{.Names}}" | xargs -I{} docker exec {} pkill proxy_v2 2>/dev/null; true
docker ps -a --filter "name=dmr" -q | xargs -r docker rm -f
```

---

## Prerequisites

### Install the proxy

The proxy installs to `/opt/hpc/build/metric-proxy/` on the shared Docker volume. Since all 9 containers mount the same volume, installing once makes the proxy available everywhere — no syncing between nodes is needed.

```bash
docker exec -u root dmr01 bash -c "
  # Install Rust + cbindgen to the shared volume (survives container restarts)
  export RUSTUP_HOME=/opt/hpc/build/rustup CARGO_HOME=/opt/hpc/build/cargo
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
  source /opt/hpc/build/cargo/env
  cargo install cbindgen
  pip3 install numpy

  # Build and install everything (proxy + MPI exporter + strace)
  export MPICC=/opt/hpc/install/ompi/bin/mpicc
  cd /opt/hpc/build/proxy_v2
  ./install.sh /opt/hpc/build/metric-proxy
"
```

After install, verify all exporters are detected:
```
/opt/hpc/build/metric-proxy/bin/proxy_run --listexporters
# Expected: mpi, strace, finstrument
```

### Changes to `start.sh`

**Publish port 1337** on dmr01 so Chrome on the host can reach the root proxy.
In `startup_container()`, replace the `CMD="docker run ..."` block with:

```bash
PORT_PUBLISH=""
if [ "$C_ID" -eq 1 ]; then PORT_PUBLISH="-p 1337:1337"; fi

CMD="docker run --rm \
    --cap-add=SYS_NICE --cap-add=SYS_PTRACE --security-opt seccomp=unconfined \
    -v install-dmr:/opt/hpc/install \
    -v build-dmr:/opt/hpc/build \
    $PORT_PUBLISH \
    -h $C_HOSTNAME --name $C_HOSTNAME \
    --detach $IMAGE_NAME"
```

**Mount HACC-IO** (optional) by adding these lines to the `CMD` block:
```bash
    -v /d/github/HACC-IO:/opt/hpc/build/dmr/examples/hacc-io \
    --add-host host.docker.internal:host-gateway \
```

---

## Notes

**Shared volume:** `/opt/hpc/build/` is bind-mounted into every container from `/e/dmr/docker-cluster-dmrv2/build/` on the host. Files written there by any container or from the host are immediately visible to all nodes.

**All proxies run as mpiuser**, including dmr01. This ensures the Unix socket UID matches the job user so MPI processes can connect.

**Sampling rate:** `-S 100` = 100 ms period (10 Hz). Use `-S 10` for 100 Hz but expect some artifacts.

**Slurm:** `slurmctld` may take a moment after `start.sh`. If nodes show `unk*` after a container restart (without re-running `start.sh`), run:
```bash
docker exec -u root dmr01 /opt/hpc/build/bin/rebootstrap-slurm.sh
```

**Verify Slurm is healthy:**
```bash
docker exec dmr01 /opt/hpc/install/slurm-dmr/bin/sinfo
# Expected: local* up infinite  8  idle dmr[02-09]
```

---

## MPI and syscall metrics

`test.sh` submits a HACC-IO job wrapped with `proxy_run`. The launcher auto-detects all available exporters from the install prefix and applies them automatically:

| Exporter | What it captures | Metric prefix |
|---|---|---|
| `mpi` | MPI call counts, times, and data sizes | `mpi___` |
| `strace` | Syscall counts and times on the submit node | `strace___` |
| `finstrument` | Function entry/exit via `LD_PRELOAD` | `finstrument___` |

No flags are needed — `proxy_run --` activates all detected exporters.

**Bandwidth metric:** For every `___size___<fn>` / `___time___<fn>` counter pair the proxy automatically synthesizes a `___bandwidth___<fn>` gauge (bytes/s = Δsize / Δtime_in_call). This is computed live at each scrape and forwarded to FTIO. Disable with `--no-bandwidth` on the proxy start command.

---

## FTIO

FTIO analyses the metric time series for periodicity and dominant frequencies. The proxy sends all collected metrics to FTIO after each scrape and displays the results in the FTIO tab of the trace view.

### Why FTIO runs on the host

The FTIO server (`admire_proxy_zmq`) requires Python 3.10+. The DMR containers run Rocky Linux 9.7 with Python 3.9, so FTIO cannot run inside any container. Instead, run it on the host (which has Python 3.13) and point the proxy at it via the Docker host gateway (`172.17.0.1`).

### Setup

```bash
# 1. Start FTIO on the host
/d/github/FTIO/.venv/bin/admire_proxy_zmq > /tmp/ftio_zmq_host.log 2>&1 &

# 2. Get the ZMQ port it bound to
FTIO_PORT=$(until grep -m1 'tcp://' /tmp/ftio_zmq_host.log; do sleep 0.5; done | grep -oP ':\K\d+')

# 3. Write a wrapper into the shared volume bin dir
#    The proxy calls `which admire_proxy_zmq`, so placing it in the metric-proxy bin
#    dir (which is in PATH when the proxy starts) is enough — no container changes needed.
cat > /e/dmr/docker-cluster-dmrv2/build/metric-proxy/bin/admire_proxy_zmq << EOF
#!/bin/bash
echo 'tcp://172.17.0.1:${FTIO_PORT}'
exec tail -f /tmp/ftio_zmq_host.log 2>/dev/null || sleep infinity
EOF
chmod +x /e/dmr/docker-cluster-dmrv2/build/metric-proxy/bin/admire_proxy_zmq
```

The wrapper:
- Prints the ZMQ address on stdout (the proxy reads this as the connection endpoint)
- Then keeps running so the proxy doesn't consider it dead

**After this, start (or restart) the proxies.** The root proxy on dmr01 will find `admire_proxy_zmq`, spawn it, read the ZMQ address, and connect. All subsequent job metrics are forwarded to FTIO automatically.

### Persistence

The wrapper lives on the shared volume and survives container restarts. The FTIO server is a host process; if you restart the host or kill the server, re-run steps 1–3 (the port changes each time) and restart the proxies.

### Verifying FTIO works

```bash
# Check the proxy connected successfully
curl -s http://localhost:1337/ftio/port
# Expected: {"operation":"<port>","success":true}

# Watch incoming data in the FTIO log
tail -f /tmp/ftio_zmq_host.log
# During a running job you will see lines like:
# Received request (677951 bytes)
# Processing 360 metrics
# Calculation time: 0.43 s
```

Results appear in Chrome under the **FTIO** tab of the trace view — select a job to see dominant frequencies and phase predictions for each metric.

---

## HACC-IO

The submit script `test.sh` runs HACC-IO with full proxy interception:

```bash
PROXY_RUN=/opt/hpc/build/metric-proxy/bin/proxy_run
$PROXY_RUN -- $DMR_PATH/scripts/dmr_wrapper mpirun --host $NODELIST_WITH_COUNTS \
    ./HACC_ASYNC_IO 1000000 test_run/mpi
```

**Expected metrics in the trace view (job is running):**
- `mpi___hits___mpi_file_write_at`, `mpi___time___mpi_file_write_at`, `mpi___size___mpi_file_write_at`
- `mpi___bandwidth___mpi_file_write_at` — live bytes/s per rank (aggregate × 8 ≈ HACC reported BW)
- `strace___hits___write`, `strace___time___write`
- FTIO tab shows periodicity analysis for all 360+ metrics
