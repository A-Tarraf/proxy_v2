# Monitoring the DMR Docker Cluster with Metric Proxy

## Quick start

```bash
# 1. Start cluster
cd /d/benchmark/docker-cluster-dmrv2
docker ps -a --filter "name=dmr" -q | xargs -r docker rm -f  # remove any leftover dmr containers
bash start.sh -n 9  # start 9 nodes: dmr01 (head) + dmr02-09 (compute)

# 2. (Optional) Start FTIO on the HOST — see FTIO section below.
#    Skip this step if FTIO analysis is not needed.
#    Normally the proxy starts FTIO itself (it finds admire_proxy_zmq in
#    PATH and spawns it) — no manual step. It is only needed in THIS
#    Docker setup: the DMR containers have no internet access, so FTIO is
#    not installed inside them and runs on the host instead, and a proxy
#    inside a container cannot spawn a host process. The admire_proxy_zmq
#    in MPATH is just a wrapper that prints the host server's address, so
#    the server (and the wrapper — its port changes on every start) must
#    exist BEFORE the proxies come up. With FTIO installed inside the
#    containers (Case 1 in the FTIO section), this step disappears.

# 3. Start proxies
PROXY=/opt/hpc/build/metric-proxy/bin/proxy_v2
MPATH=/opt/hpc/build/metric-proxy/bin  # prepended to PATH so proxy can find admire_proxy_zmq

# kill any stale proxy instances across all nodes
docker ps --filter "name=dmr" --format "{{.Names}}" | xargs -I{} docker exec {} pkill proxy_v2 2>/dev/null; true

# Do NOT pin the proxies to cores on this Docker cluster (see Notes below;
# on real Slurm clusters the proxy DOES get one dedicated core via the
# allocation — here pinning collides with the ranks and slows jobs 3-10x).

# root proxy on dmr01 — aggregates all metrics and serves the web UI on port 1337
docker exec -d -u mpiuser dmr01 env PATH=$MPATH:/usr/local/bin:/usr/bin $PROXY --port 1337 -S 100

# leaf proxy on every compute node — forwards metrics upstream to dmr01
docker ps --filter "name=dmr" --format "{{.Names}}" | grep -v -E "dmr01$|dmr01-ftio" | xargs -I{} \
    docker exec -d -u mpiuser {} env PATH=$MPATH:/usr/local/bin:/usr/bin \
    $PROXY --port 1338 --root-proxy dmr01:1337 -S 100

# 4. Open Chrome → http://localhost:1337

# 5. Submit job
bash /d/benchmark/docker-cluster-dmrv2/mpiuser-drop-in.sh
# inside the container:
cd /opt/hpc/build/dmr/examples/hacc-io
sbatch test.sh && squeue

# 6. Cleanup
docker ps --filter "name=dmr" --format "{{.Names}}" | xargs -I{} docker exec {} pkill proxy_v2 2>/dev/null; true
docker ps -a --filter "name=dmr" -q | xargs -r docker rm -f
# If FTIO was active: remove the wrapper so a stale port doesn't block the next proxy startup
rm -f /e/dmr/docker-cluster-dmrv2/build/metric-proxy/bin/admire_proxy_zmq
```

> **Do not restart the proxy while a job is running.** The proxy holds trace data in memory and writes it to disk incrementally. Restarting mid-job discards whatever has not yet been flushed.

---

## Prerequisites

### Install the proxy

The proxy installs to `/opt/hpc/build/metric-proxy/` on the shared Docker volume. Since all 9 containers mount the same volume, installing once makes the proxy available everywhere — no syncing between nodes is needed.

```bash
docker exec -u root dmr01 bash -c "
  export RUSTUP_HOME=/opt/hpc/build/rustup CARGO_HOME=/opt/hpc/build/cargo
  source /opt/hpc/build/cargo/env
  cargo install cbindgen

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

These changes are local-only (not in the upstream docker-cluster repo); the
full modified script is preserved verbatim in the
[appendix](#appendix--startsh-backup-copy) as a backup.

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

**One core per proxy — cluster yes, this Docker setup NO.** On a real Slurm
cluster each per-node proxy runs on one core *reserved by the allocation*
(README / `docs/sbatch_example.sh`: `--ntasks-per-node=1 --cpus-per-task=1
--overlap`, with `--mem-per-cpu` sized so one core stays free) — the ranks
get the *other* cores, so the proxy's core is uncontended. Do **not** imitate
this with `taskset` here: all 9 containers share the host's cores and Slurm
binds ranks to low core IDs inside each container, so a pinned proxy sits on
exactly the cores the ranks use. Measured with HACC-IO (4 nodes, strace on):
unpinned ≈ 1:49–2:18; pinned (any variant) 6–19+ min, degrading with each
run. Leave the proxies unpinned and let the scheduler spread them.

**After restarting proxies**, verify the tree with `/topo` — NOT with the
root's `/join/list`:
```bash
curl -s localhost:1337/topo | python3 -c "
import json,sys
from collections import Counter
edges=[e for e in json.load(sys.stdin) if e[0]!=e[1]]
dup=[c for c,n in Counter(c for _,c in edges).items() if n>1]
print(len(edges),'nodes attached | double parents:', dup or 'NONE')"
# expect: 8 nodes attached | double parents: NONE
```
Registration is a *tree*: the root delegates joins to child proxies once its
branch limit is reached, so most leaves legitimately do NOT appear in the
root's direct `/join/list`. Blindly re-joining every node at the root used to
give nodes **two parents**, and every counter that flowed through both paths
was summed twice at the root (a 4-rank job showed 8-16 ranks and ~2x bytes).
`/join` and `/pivot` are idempotent against the tree now, so a stray manual
join is harmless — but the right check is `/topo`, and only genuinely missing
nodes should be joined manually.

**Sampling rate:** `-S 100` = 100 ms period (10 Hz). Use `-S 10` for 100 Hz but expect some artifacts.

**`-S` overhead — read this before lowering it.** The scraping loop's wakeup
granularity is derived from `-S` (period/10, clamped to 10–1000 ms), so a small
`-S` means frequent wakeups on *every* proxy in the tree. On a shared machine
(like this Docker cluster, where all 9 proxies compete with the job for the
same cores) those wakeups contend the scheduler, and jobs traced with the
**strace exporter** are hit hardest — every traced syscall needs the tracer
rescheduled promptly. Measured with HACC-IO (4 nodes, ~270 k syscalls/rank,
details in `speed_report.md`):

| Setup | Job time |
|---|---|
| no instrumentation | ~22 s |
| `proxy_run -e mpi` (any `-S`) | ~19 s |
| `proxy_run` with strace, 10 ms wakeups (old fixed loop / `-S 100`) | 112–195 s |
| `proxy_run` with strace, `-S 1000` (100 ms wakeups) | ~60 s |

Rules of thumb: keep `-S 1000` unless you need finer time resolution; if job
runtime matters, prefer `proxy_run -e mpi` (free) over the strace exporter; on
dedicated cluster nodes the effect is much smaller than on this laptop setup.

**Slurm:** `slurmctld` and `slurmd` need ~15 s to start after `start.sh`. If you submit too early, `squeue` shows the job stuck in `CF` (configuring) or nodes show `unk*`. Wait, then verify:
```bash
sleep 15
docker exec dmr01 /opt/hpc/install/slurm-dmr/bin/sinfo
# Expected: local* up infinite  8  idle dmr[02-09]
```

If nodes still show `unk*` after a container restart (without re-running `start.sh`), re-bootstrap Slurm manually:
```bash
docker exec -u root dmr01 /opt/hpc/build/bin/rebootstrap-slurm.sh
sleep 15
docker exec dmr01 /opt/hpc/install/slurm-dmr/bin/sinfo
```

---

## Instrumenting jobs with proxy_run

Adding `proxy_run --` before the application binary is the only change needed. It auto-detects all installed exporters and injects them:

| Exporter | What it captures | Metric prefix |
|---|---|---|
| `mpi` | MPI call counts, times, and data sizes | `mpi___` |
| `strace` | Syscall counts and times | `strace___` |
| `finstrument` | Function entry/exit via `LD_PRELOAD` | `finstrument___` |

### `proxy_run` must wrap the application, not the MPI launcher

`proxy_run` attaches strace and sets `LD_PRELOAD` on the process it directly runs. To get per-node strace and MPI instrumentation, it must be spawned on each compute node by mpirun — not placed outside it.

| ❌ Wrong — only instruments the launcher on dmr01 | ✅ Correct — instruments each rank on each node |
|---|---|
| `proxy_run -- mpirun -N 4 ./app` | `mpirun -N 4 proxy_run -- ./app` |
| `proxy_run -- dmr_wrapper mpirun --host ... ./app` | `dmr_wrapper mpirun --host ... proxy_run -- ./app` |
| `proxy_run -- srun ./app` | `srun proxy_run -- ./app` |

**Job ID in the DMR Docker cluster:** Pass `-j $SLURM_JOB_ID` explicitly to `proxy_run`. In this setup, `dmr_wrapper` forces SSH-based process launch (overriding Slurm PLM), and `fwd-environment` does **not** forward `SLURM_JOBID` to ranks on remote nodes. Without `-j`, each remote rank falls back to `METRIC_PROXY_LAUNCHER_PPID` (its own parent PID) and registers as a separate one-process job. Since bash expands `$SLURM_JOB_ID` to a literal number before mpirun runs, passing it explicitly ensures all ranks on all nodes get the same job ID as a CLI argument.

> Full details on measurement semantics (interception at call **return**, times **summed over ranks** → per-rank averages, counter-vs-gauge aggregation, FTIO wire protocol) are in [`metrics_semantics.md`](metrics_semantics.md).

**Bandwidth = dewrapped bandwidth (virtual metric):** For each `___size___<fn>` / `___time___<fn>` counter pair the trace UI offers a virtual `___bandwidth_dewrap___<fn>` metric — the **aggregate wall-clock bytes/s**. (The former stored `___bandwidth___` gauge and its `--no-bandwidth` flag were removed: its Δsize/Δtime was the *per-rank speed inside the I/O calls* with each burst attributed to the sample where the call completed — superseded by the dewrap.)

How dewrapping works: the exporters account a call's bytes and duration only when the call **returns**, so a burst normally lands entirely on its completion sample. Dewrap **creates the points in the past**: each burst is spread backwards from its completion time over its estimated wall-clock span (Δtime ÷ `proxy_mpi_ranks`, i.e. assuming the ranks did the I/O concurrently), so the burst *starts* where it actually started. Each plotted point carries the rate of the interval **starting** at its timestamp, held until the next sample (the series ends with a closing 0). The integral over time equals the transferred bytes exactly. Everything is derived on the fly at plot time from the stored cumulative counters — nothing extra is stored in the trace, and old points are never rewritten (the "past" points exist only in the derived view).

For FTIO the same reconstruction runs on the FTIO side: add `--dewrap` to the *custom arguments* in the FTIO tab. FTIO then analyzes the reconstructed signal **in addition to** the regular metrics, storing the model under the `___bandwidth_dewrap___<fn>` name — i.e. exactly the metric you plot in the UI, so the FTIO overlay matches. (The proxy deliberately does not pre-compute these series for FTIO: FTIO derivates every non-`deriv` metric, which would mangle an already-derived rate.)

**Ranks of a (malleable) job over time — `job_mpi_ranks`:** every job trace contains `job_mpi_ranks`, the number of MPI ranks *of that job* currently connected: exact integer per node, **summed across nodes on aggregating proxies** (it is a set-type counter, so the root shows the job's total). A malleable job that grows or shrinks at runtime shows the change directly in this series, and the dewrap uses it as its concurrency estimate. By contrast `proxy_mpi_ranks`/`proxy_connected_procs` are *node-wide* gauges (all jobs together), and gauges are averaged (`total/hits`) when merged — that is why they can show decimals on the root; the UI badge uses `GET /ranks`, which sums live per-node values and is always an integer.

**Connected processes:** `proxy_connected_procs` is tracked as a live time-series Counter on each proxy node and summed across nodes at the root.

---

## FTIO

FTIO analyses the metric time series for periodicity and dominant frequencies. The proxy sends all collected metrics to FTIO after each scrape and displays the results in the FTIO tab of the trace view.

### How it works

When the proxy starts it searches for `admire_proxy_zmq` in PATH, spawns it, reads the ZMQ address from its first line of stdout, and connects automatically. `MPATH` is prepended to PATH in the start commands so any `admire_proxy_zmq` placed there is picked up without further configuration.

### Case 1 — FTIO installed inside the container

If the container image already has FTIO installed (e.g. rebuilt with it bundled), `admire_proxy_zmq` is already in PATH inside the container. No wrapper needed — just start the proxies normally and FTIO is used automatically.

To check:
```bash
docker exec dmr01 which admire_proxy_zmq   # should print a path
```

**Installing FTIO into the container (one-time, persists on the shared volume):**
Rocky 9 ships only Python 3.9, but FTIO needs ≥ 3.10. Python 3.12 is available
via dnf and the containers have PyPI access, so FTIO can live in a venv on the
shared build volume — it survives container restarts and removes the
host-wrapper / ephemeral-port dance of Case 2 entirely:

```bash
docker exec -u root dmr01 dnf install -y python3.12
docker exec -u root dmr01 python3.12 -m venv /opt/hpc/build/ftio-venv
docker exec -u root dmr01 /opt/hpc/build/ftio-venv/bin/pip install ftio-hpc
# put it in MPATH so the proxy finds it (remove any old wrapper first):
docker exec -u root dmr01 ln -sf /opt/hpc/build/ftio-venv/bin/admire_proxy_zmq \
    /opt/hpc/build/metric-proxy/bin/admire_proxy_zmq
```

`python3.12` must be installed in **every** container that runs a proxy (the
venv itself is shared). Needs ~1 GB free on the shared volume.

### Case 2 — FTIO on the host (current DMR setup)

The DMR containers have no internet access, so FTIO runs on the host. A wrapper script in `MPATH` redirects the proxy to the host FTIO server via the Docker gateway (`172.17.0.1`).

**What runs where:** only FTIO's analysis code (the Python
`admire_proxy_zmq` server and its parallel workers) executes on the host.
Everything else stays inside the containers: the proxies, the job, and the
exporters. The proxy in the container sends the metrics over ZMQ through the
Docker gateway to the host, and the host sends the models back — FTIO never
touches the containers. The wrapper script is *not* FTIO: it is a stand-in
executed by the proxy inside the container whose only job is to print the
host server's address.

**Each session** (or after any proxy restart), recreate the wrapper:

```bash
MPATH=/e/dmr/docker-cluster-dmrv2/build/metric-proxy/bin

# 1. Start FTIO on the host (skip if already running)
/d/github/FTIO/.venv/bin/admire_proxy_zmq > /tmp/ftio_zmq_host.log 2>&1 &
FTIO_PORT=$(until grep -oP ':\K\d+' /tmp/ftio_zmq_host.log 2>/dev/null; do sleep 0.3; done | head -1)
echo "FTIO listening on port $FTIO_PORT"

# 2. Write the wrapper (always recreate — port changes each run)
cat > $MPATH/admire_proxy_zmq << EOF
#!/bin/bash
echo 'tcp://172.17.0.1:${FTIO_PORT}'
exec tail -f /tmp/ftio_zmq_host.log 2>/dev/null || sleep infinity
EOF
chmod +x $MPATH/admire_proxy_zmq

# 3. Restart the proxies so they pick up the new wrapper
```

**Why restarts matter:** the proxy discovers FTIO **once, at startup** — it
spawns whatever `admire_proxy_zmq` it finds in PATH and reads the address
from its first line of output; it never re-reads it later. "Restarting the
proxies" means killing and relaunching the `proxy_v2` processes in the
containers (the `pkill proxy_v2` + `docker exec` commands from quick-start
step 3). So the order is: host FTIO server first, then the wrapper (the port
changes on every server start), then the proxies. If the FTIO server is ever
restarted, the running proxies still hold the old port — recreate the wrapper
and restart the proxies too.


> **Cleanup:** remove the wrapper when done (`rm $MPATH/admire_proxy_zmq`). A stale wrapper pointing to a dead port is no longer fatal: FTIO analysis now runs in its own thread with bounded ZMQ timeouts, so a dead FTIO server costs a warning in the log instead of freezing metric collection. (Previously it stalled the whole scraping loop — jobs appeared late and traces got a single data point.)

> **FTIO can spawn many CPU-heavy parallel worker processes** when analyzing a large job. Do not restart the proxy while FTIO is processing — killing FTIO mid-analysis and restarting the proxy will discard in-flight trace data.

### Verifying FTIO works

```bash
curl -s http://localhost:1337/ftio/port
# Expected: {"operation":"<port>","success":true}

tail -f /tmp/ftio_zmq_host.log
# During a job: "Received request (N bytes)" / "Processing N metrics" / "Calculation time: N s"
```

---

## HACC-IO

The submit script is at `/d/github/HACC-IO/test.sh` (mounted into containers at `/opt/hpc/build/dmr/examples/hacc-io/test.sh`).

```bash
#!/bin/bash
#SBATCH --time=00:30:00
#SBATCH --exclusive
#SBATCH -o slurm_proxy.out
#SBATCH -N4

echo $DMR_PATH
echo $SLURM_ROOT

export PATH=$SLURM_ROOT/bin:$PATH
export LD_LIBRARY_PATH=$DMR_PATH/build/lib:$LD_LIBRARY_PATH

export DMR_PROCS_PER_NODE=1

NODELIST_WITH_COUNTS=$(scontrol show hostnames "$SLURM_JOB_NODELIST" | \
    awk -v n="$DMR_PROCS_PER_NODE" '{print $1 ":" n}' | paste -sd,)

PROXY_RUN=/opt/hpc/build/metric-proxy/bin/proxy_run

set -x

$DMR_PATH/scripts/dmr_wrapper mpirun --host $NODELIST_WITH_COUNTS \
    $PROXY_RUN -j $SLURM_JOB_ID -- ./HACC_ASYNC_IO 1000000 test_run/mpi

rm -f test_run/mpi* && echo "Deleted MPI files of HACC-IO"
cat *_MPI.jsonl > all.jsonl || echo true
```

The only additions relative to a plain DMR job (`submit_custom_slurm.sh`) are `$PROXY_RUN -j $SLURM_JOB_ID --` before the binary. `sbatch` propagates the submitter's environment, so `mpirun` and OpenMPI libs are found automatically from the shell PATH where `sbatch` was called.

> **Note:** `-j $SLURM_JOB_ID` is required in this Docker setup. See the "Job ID" note in the `proxy_run` section above for the reason.

**Expected metrics while the job runs:**
- `mpi___hits___mpi_file_write_at`, `mpi___time___mpi_file_write_at`, `mpi___size___mpi_file_write_at`
- `mpi___bandwidth___mpi_file_write_at` — live bytes/s, summed across all 4 ranks
- `strace___hits___write`, `strace___time___write` — per-rank I/O syscalls from each compute node
- `proxy_mpi_ranks` — MPI ranks currently running on the node (time series; aggregated across nodes on the root)
- `proxy_connected_procs` — connected exporter *processes* on the node
- FTIO tab shows periodicity analysis for all metrics

### Ranks vs procs

Two related but different numbers:

- **`proxy_mpi_ranks`** counts Unix-socket connections that identified as the
  MPI exporter (first metric descriptor starts with `mpi___`) — exactly **one
  per MPI rank**. This is the number to watch for malleability
  (expand/shrink): the trace metric gives ranks-per-node over time, and
  `GET /ranks` on the root gives the live cluster-wide total.
- **`proxy_connected_procs`** counts **all** connected exporter processes.
  Each rank typically contributes ~2 (MPI wrapper + strace exporter), and the
  node running `mpirun`/`dmr_wrapper` contributes a few more for its
  instrumented helper processes. Useful as a liveness signal, not a rank count.

The badge in the trace UI shows both: `ranks: N | procs: M` (polled from
`/ranks` and `/procs` on the proxy serving the page — on the root these
aggregate over all registered compute proxies).

## Appendix — start.sh (backup copy)

Verbatim copy of `/d/benchmark/docker-cluster-dmrv2/start.sh` as used by the
quick start. The upstream repo
(`gitlab.inria.fr/dynres/dyn-procs/docker-cluster`) tracks this file, but the
monitoring-relevant parts are **local, uncommitted modifications** — restoring
from upstream would lose them. Local-only relative to upstream:

- `IMAGE_NAME=docker-cluster-dmr:dmrv2` (image tag)
- `-p 1337:1337` on dmr01 (root proxy web UI reachable from the host browser)
- bind mounts for `HACC-IO`, `TMIO` and `FTIO` from `/d/github`
- `--add-host host.docker.internal:host-gateway` (needed by the host-FTIO
  wrapper setup, Case 2 above)

Run it with a pty (`script -qfc "bash start.sh -n 9" /dev/null`) if the
terminal is non-interactive — the final `docker exec -it` steps need one.

```bash
#!/bin/bash

#
# Default values
#
IMAGE_NAME=docker-cluster-dmr:dmrv2
OVERLAY_NETWORK=pmix-net
NNODES=2
INSTALL_DIR=
BUILD_DIR=

MPI_HOSTFILE=$PWD/tmp/hostfile.txt
HOSTFILE=$PWD/tmp/hosts
AUTHORIZEDKEYS=$PWD/tmp/authorized_keys
SHUTDOWN_FILE=$PWD/tmp/shutdown-`hostname -s`.sh


# Argument parsing
while [[ $# -gt 0 ]] ; do
    case $1 in
        "-h" | "--help")
            printf "Usage: %s [option]
    -p | --prefix PREFIX       Prefix string for hostnames (Default: %s)
    -n | --num NUM             Number of nodes to start on this host (Default: %s)
    -i | --image NAME          Name of the container image (Required)
    -h | --help                Print this help message\n" \
        `basename $0` $NNODES
            exit 0
            ;;
        "-n" | "--num")
            shift
            NNODES=$1
            ;;
        "-i" | "--image" | "-img")
            shift
            IMAGE_NAME=docker-cluster-dmr
            ;;
        *)
            printf "Unkonwn option: %s\n" $1
            exit 1
            ;;
    esac
    shift
done

if [ "x$IMAGE_NAME" == "x" ] ; then
    echo "Error: --image must be specified"
    exit 1
fi

# Spin up all of the containers
ALL_CONTAINERS=()
startup_container()
{
    C_ID=$(($1 + 0))
    C_HOSTNAME=`printf "%s%02d" "dmr" $C_ID`

    echo "Starting: $C_HOSTNAME"

	# Publish port 1337 on dmr01 so the root proxy is reachable from the host browser
	PORT_PUBLISH=""
	if [ "$C_ID" -eq 1 ]; then
		PORT_PUBLISH="-p 1337:1337"
	fi

	CMD="docker run --rm \
		--cap-add=SYS_NICE --cap-add=SYS_PTRACE --security-opt seccomp=unconfined \
		-v install-dmr:/opt/hpc/install \
		-v build-dmr:/opt/hpc/build \
		-v /d/github/HACC-IO:/opt/hpc/build/dmr/examples/hacc-io \
		-v /d/github/TMIO:/opt/hpc/build/dmr/examples/TMIO \
		-v /d/github/FTIO:/opt/hpc/build/FTIO \
		--add-host host.docker.internal:host-gateway \
		$PORT_PUBLISH \
		-h $C_HOSTNAME --name $C_HOSTNAME \
		--detach $IMAGE_NAME"

    C_FULL_ID=`$CMD`
    RTN=$?
    if [ 0 != $RTN ] ; then
        echo "Error: Failed to create $C_HOSTNAME"
        echo $C_FULL_ID
        exit 1
    fi

    C_SHORT_ID=`echo $C_FULL_ID | cut -c -12`
    docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' $C_SHORT_ID >> $MPI_HOSTFILE
    ALL_CONTAINERS+=($C_SHORT_ID)
}

docker volume create --driver local --name build-dmr --opt type=none --opt device=$PWD/build --opt o=uid=root,gid=root --opt o=bind > /dev/null 2>&1
docker volume create --driver local --name install-dmr --opt type=none --opt device=$PWD/install --opt o=uid=root,gid=root --opt o=bind > /dev/null 2>&1

mkdir -p tmp
mkdir -p install/etc/
rm -rf $MPI_HOSTFILE
rm -rf $HOSTFILE
rm -rf $AUTHORIZEDKEYS
rm -rf ./install/etc/hosts
rm -rf ./install/etc/authorized_keys
touch $MPI_HOSTFILE
touch $HOSTFILE
touch $AUTHORIZEDKEYS

cat ~/.ssh/*.pub > $AUTHORIZEDKEYS
chmod 644 $AUTHORIZEDKEYS

# Create each virtual node
for i in $(seq 1 $NNODES); do
    startup_container $i
done

# Create a shutdown file to help when we cleanup
rm -f $SHUTDOWN_FILE

touch $SHUTDOWN_FILE
chmod +x $SHUTDOWN_FILE
for cid in "${ALL_CONTAINERS[@]}" ; do
    echo "docker stop $cid" >> $SHUTDOWN_FILE
done

cp -a $HOSTFILE ./install/etc/hosts
cp -a $AUTHORIZEDKEYS ./install/etc/authorized_keys
cp -a tmp/unique_hosts ./build
cp -a tmp/hostuser ./build

# generate configurations
./build/bin/generate-configurations.sh
docker exec -it -u root dmr01 /opt/hpc/build/bin/set-authorized-keys.sh

HostNumber=1
FormattedNumber=""
while read h; do
    FormattedNumber=`printf %02d $HostNumber`
    docker exec -u root dmr${FormattedNumber} /opt/hpc/build/bin/set-hosts.sh
    docker exec -u root dmr${FormattedNumber} groupadd -g `id -g` mpigroup > /dev/null 2>&1
    docker exec -u root dmr${FormattedNumber} usermod mpiuser -u `id -u`   > /dev/null 2>&1
    docker exec -u root dmr${FormattedNumber} usermod mpiuser -g `id -g`   > /dev/null 2>&1
	docker exec -u root dmr${FormattedNumber} /opt/hpc/build/bin/start-munge.sh > /dev/null 2>&1
    ((HostNumber++))
done < ./tmp/all_nodes

cp -a tmp/all_nodes unique_hosts
#docker exec -u 0 -it dmr01 /opt/hpc/build/bin/start-mariadb.sh
docker exec -u 0 -it dmr01 /opt/hpc/build/bin/rebootstrap-slurm.sh
```
