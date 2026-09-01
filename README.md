# Metric Proxy

Metric Proxy is a Prometheus Aggregating Push Gateway designed for use in the ADMIRE project. It acts as an instrumentation proxy, collecting and aggregating metrics from various sources to provide a centralized view for monitoring and analysis.

## Quick Start

```sh
git clone https://github.com/besnardjb/proxy_v2.git
cd proxy_v2
# Install the proxy (requires mpicc in the path)
./install.sh $HOME/metric-proxy
# Add the prefix to your path
export PATH=$HOME/metric-proxy/bin:$PATH
# Run the server (and keep it running in a terminal)
proxy_v2
# Run the client in another shell
proxy_run -j testjob -- ls -R /
```

Then open http://localhost:1337

---

## Install

### Prerequisites

- Rust (install via `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
- `mpicc` in PATH (any MPI matching your application)
- Python 3 in PATH
- `libssl-dev` / `openssl-devel` (if OpenSSL compile errors, set `export OPENSSL_DIR="/usr/local/ssl"`)

### Compile and Install

```bash
git clone https://github.com/besnardjb/proxy_v2.git
cd proxy_v2
./install.sh $HOME/metric-proxy
export PATH=$HOME/metric-proxy/bin:$PATH
```

---

## Running the Proxy

### Flags Reference

| Flag | Long form | Default | Description |
|------|-----------|---------|-------------|
| `-p` | `--port` | `1337` | HTTP server port |
| `-r` | `--root-proxy` | — | Relay all metrics to this upstream proxy (`ADDR` or `ADDR@PERIOD_MS`) |
| `-s` | `--sub-proxies` | — | Additional proxies to scrape (comma-separated) |
| `-S` | `--sampling-period` | `1000` | Sampling period in milliseconds |
| `-m` | `--max-trace-size` | `32` | Max trace file size on disk in MB |
| `-t` | `--target-prefix` | `~/.proxyprofiles/` | Root directory for proxy data |
| `-i` | `--inhibit-profile-aggregation` | false | Disable local profile saving (use on leaf nodes when relaying to root) |
| `-b` | `--branches` | `2` | Branches for hierarchical aggregation (0 = binomial, >0 = k-ary tree) |
| `-u` | `--unix` | — | Path to UNIX socket for the gateway |
| — | `--no-bandwidth` | false | Disable live `___bandwidth___` metric synthesis |

### Single-Node Usage

Start the proxy server, then wrap your application with `proxy_run`:

```sh
# Terminal 1 — server
proxy_v2

# Terminal 2 — run application
mpirun -np 8 proxy_run -- ./myprogram
# or with srun
srun -n 8 proxy_run -- ./myprogram
```

Force a job ID with `-j`:

```sh
proxy_run -j myjob -- ./myprogram
```

### `proxy_run` Flags Reference

| Flag | Long form | Default | Description |
|------|-----------|---------|-------------|
| `-e` | `--exporter` | all detected | Exporters to activate (comma-separated), e.g. `-e mpi`, `-e mpi,strace` |
| `-j` | `--jobid` | auto | Job ID (MPI/SLURM usually provide one) |
| `-S` | `--period` | `1000` | Push period in milliseconds (how often the client sends counters to the proxy) |
| `-u` | `--unixsocket` | auto | Path to the proxy's UNIX socket |
| `-T` | `--strace-trace` | denylist (see below) | Which syscalls the **strace exporter** traces |
| `-l` | `--listexporters` | — | List detected exporters and exit |

### Exporters, and the cost of syscall tracing

With no `-e` flag, **all detected exporters run**, including `strace`. That
matters, because the strace exporter uses `ptrace`: **every traced syscall stops
the application twice**, at roughly 50 µs a stop. The cost is set by *how many
syscalls are traced* — **not** by the sampling period. Lowering `-S` does not make
tracing cheaper.

| Exporter | Mechanism | Cost |
|---|---|---|
| `mpi` | In-process PMPI wrappers | **Free** — no `ptrace`, no context switch |
| `strace` | `ptrace` syscall interception | ~50 µs × number of traced syscalls |
| `finstrument` | Compile-time function instrumentation | Only if built with `-finstrument-functions` |

```sh
proxy_run -e mpi -- ./myprogram      # MPI metrics only — essentially free
proxy_run -- ./myprogram             # everything, including syscall tracing
```

**If you only need MPI-level metrics, use `-e mpi`.** On a syscall-heavy workload
(100k `write` + 500k `getpid`): `-e mpi` 0.29 s vs untraced 0.30 s — free.

#### Which syscalls get traced (`-T` / `--strace-trace`)

When the strace exporter is active, `proxy_run` by default **skips syscalls that
are hot but carry no useful metric** (MPI progress engines spin on these). The
kernel filters them out via seccomp-BPF, so they never stop the application:

```
default: !getpid,gettid,futex,sched_yield,clock_gettime,clock_nanosleep,
          nanosleep,rt_sigprocmask,rt_sigaction
```

Same workload as above:

| Invocation | Wall time | Metrics |
|---|---|---|
| `proxy_run -T all -- app` (trace everything) | 23.0 s | 43 |
| `proxy_run -- app` *(default denylist)* | **4.6 s** | 41 |
| `proxy_run -e mpi -- app` (no `ptrace`) | 0.29 s | — |

**This is a denylist, not an I/O allowlist.** File, network, memory, process and
signal syscalls are all still traced — only the nine above are skipped.

**What you lose, and how to get it back.** The nine denylisted syscalls produce no
`strace___hits___*` or `strace___time___*` counters (up to 18 metrics; they are not
I/O calls, so no `___size___` counter exists for them anyway). **All byte counters
are unaffected and byte-for-byte identical.** To restore them:

```sh
# trace everything (previous behaviour: complete, but far slower)
proxy_run -T all -- ./myprogram

# keep futex (MPI sync/wait time) but drop the rest of the hot set
proxy_run -T '!getpid,gettid,sched_yield,clock_gettime,nanosleep' -- ./myprogram

# only file-descriptor and network syscalls
proxy_run -T '%desc,%network' -- ./myprogram
```

`-T` takes any `strace -e trace=` expression. It can also be set with the
`METRIC_PROXY_STRACE_TRACE` environment variable, which is handy when you do not
control the `proxy_run` command line inside a batch script.

Full details: [docs/strace_exporter.md](docs/strace_exporter.md).

To measure what instrumentation costs *your* application on a real cluster, see
[docs/cluster_benchmark.md](docs/cluster_benchmark.md) and the ready-made sweep
job [`docs/sbatch_overhead_sweep.sh`](docs/sbatch_overhead_sweep.sh), which
compares `plain` / `-e mpi` / `-T all` / default / keep-`futex` on the same nodes.

---

## HPC / SLURM Multi-Node Deployment

In a multi-node SLURM job the recommended topology is:

```
Login node:  proxy_v2  (root proxy — persistent, stores profiles and traces)
                ↑
Compute nodes: proxy_v2 -i -r http://LOGIN_NODE:1337  (leaf proxies — relay to root)
                ↑
Application: srun proxy_run -- <app>
```

### Why this layout?

- The **root proxy** on the login node aggregates data from all nodes and exposes the web UI at `http://LOGIN_NODE:1337`.
- Each **leaf proxy** runs on a compute node with `-i` (no local profile saving) and `-r` (relay to root). This keeps the compute nodes lightweight.
- `-S 100` samples every 100 ms for finer-grained metrics during short jobs.
- `-m 128` raises the per-node trace limit to 128 MB for large workloads.

### Example SLURM Job Script

The following script (see also [`docs/sbatch_example.sh`](docs/sbatch_example.sh)) launches DLIO with proxy instrumentation across multiple nodes:

```bash
#!/bin/bash
#SBATCH -J my_benchmark
#SBATCH -n 64               # total MPI ranks
#SBATCH -c 3                # CPUs per task
#SBATCH --mem-per-cpu=3760  # leaves one core free for the per-node proxy
#SBATCH -t 00:40:00

module purge
source ~/loads
source /path/to/FTIO/.venv/bin/activate

export PATH=$TOOLS_BIN:$PATH
export LD_LIBRARY_PATH=$TOOLS_LIB:$LD_LIBRARY_PATH
export SRUN=/opt/slurm/current/bin/srun

# ── Root proxy: already running on the login node (start it before submitting) ──
export ROOT_PROXY=login_node_hostname   # e.g. logc0002

# ── Per-node leaf proxies: launch one per node, overlap with the main job ──
$SRUN --nodes=${SLURM_NNODES} \
      --ntasks=${SLURM_NNODES} \
      --ntasks-per-node=1 \
      --cpus-per-task=1 \
      --overlap \
      proxy_v2 -i -r http://${ROOT_PROXY}:1337 -S 100 -m 128 &

CPUS_APP=${SLURM_CPUS_PER_TASK}

# ── Application (data generation, no proxy needed) ──
$SRUN --cpus-per-task=${CPUS_APP} \
      dlio_benchmark workload=my_workload \
        ++workload.workflow.generate_data=True \
        ++workload.workflow.train=False

# ── Application (training, wrapped with proxy_run) ──
$SRUN --cpus-per-task=${CPUS_APP} \
      proxy_run -- dlio_benchmark workload=my_workload \
        ++workload.workflow.generate_data=False \
        ++workload.workflow.train=True

EXITCODE=$?
exit $EXITCODE
```

> **Step 1 — start the root proxy on the login node** (do this once, before submitting):
> ```sh
> ssh <login_node>
> bash docs/start_root_proxy.sh   # starts proxy_v2 in background, prints PID and log path
> # verify: curl http://localhost:1337/job/list
> ```
> See [`docs/start_root_proxy.sh`](docs/start_root_proxy.sh) for the full script with flags explained.
>
> **Step 2 — submit the job** (leaf proxies inside the job connect back to the root):
> ```sh
> sbatch docs/sbatch_example.sh
> ```
>
> **Step 3 — view results** at `http://<login_node>:1337` → `ftio.html` → Parallel Analysis.

### Alternative: proxy on a compute node (no login-node access)

If you cannot run the root proxy on the login node, launch it on the first compute node instead:

```bash
export ROOT_PROXY=$(hostname)
$SRUN --nodes=1 --ntasks=1 --ntasks-per-node=1 --overlap \
      proxy_v2 -t . -S 100 -m 128 &
sleep 20   # wait for proxy to start

# then launch leaf proxies pointing to ROOT_PROXY, as above
```

---

## Web UI and FTIO Analysis

Open `http://localhost:1337` (or `http://ROOT_PROXY:1337`) for the dashboard.

Key pages:

| URL | Description |
|-----|-------------|
| `/` or `/jobs.html` | Live job metrics |
| `/trace.html` | Per-metric time-series viewer and offline FTIO analysis |
| `/ftio.html` | **Parallel FTIO analysis** — run FTIO on all metrics of a stored job at once, view frequency correlations, category waves, and dominant frequencies |
| `/alarms.html` | Alarm configuration |
| `/profiles.html` | Browse finished job profiles |

### Running FTIO from the proxy

`ftio.html` → **Parallel Analysis** tab: select a job, click *Run Analysis*. The proxy sends all metrics to FTIO in one batch (Python `ProcessPoolExecutor` parallelises internally) and displays:

- Dominant frequency per metric (scatter + heatmap)
- Frequency correlation matrix
- Category waves (I/O, network, compute) with reconstructed and raw signals
- Per-metric selection plot

FTIO must be installed and `admire_proxy_zmq` must be in PATH. When the proxy starts it searches PATH for `admire_proxy_zmq`, spawns it automatically, and connects — no manual steps needed.

```sh
pip install ftio   # installs admire_proxy_zmq into the active environment
export PATH=$(python3 -c "import sysconfig; print(sysconfig.get_path('scripts'))"):$PATH
```

**Bandwidth metric:** For every `___size___<fn>` / `___time___<fn>` counter pair the proxy synthesises a live `___bandwidth___<fn>` gauge (bytes/s = Δsize / Δtime_in_call) and forwards it to FTIO. Disable with `--no-bandwidth`.

---

## Data Endpoints

### Job Data

| Endpoint | Description |
|----------|-------------|
| `GET /job/list` | List current jobs and metadata |
| `GET /job/` | All jobs with full counter data |
| `GET /job/?job=JOBID` | Single job data |
| `GET /metrics/?job=JOBID` | Prometheus format export for a job |

### Trace Data

| Endpoint | Description |
|----------|-------------|
| `GET /trace/list` | List stored trace jobs |
| `POST /trace/plot` | Raw time-series data: `{"jobid":"...", "filter":"metric_name", "derivate":false}` |
| `POST /ftio/run_all?jobid=JOBID` | Run FTIO on every metric of a job; returns the models as JSON |
| `POST /ftio/run_metric?jobid=JOBID&metric=METRIC` | Run FTIO on a single metric |
| `GET /ftio/all_models?jobid=JOBID` | Return the models from the last analysis (run `run_all` first) |
| `GET /ftio/progress?jobid=JOBID` | Analysis progress: `{"processed": N, "total": M}` |
| `GET /ftio/logs` | Recent log lines from the `admire_proxy_zmq` backend |

### Alarms

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/alarms` | GET | List raised alarms |
| `/alarms/list` | GET | List all registered alarms |
| `/alarms/add` | POST | Add alarm: `{"name":"...","target":"main","metric":"...","operation":">","value":33}` |
| `/alarms/del` | GET/POST | Delete alarm by `?targetjob=X&name=Y` or POST JSON |

Example:
```bash
curl -s http://localhost:1337/alarms/add \
  -H "Content-Type: application/json" \
  -d '{"name":"High CPU","target":"main","metric":"proxy_cpu_load_average_percent","operation":">","value":80}'
```

### Profiles and Scraping

| Endpoint | Description |
|----------|-------------|
| `GET /profiles` | List finished job profiles |
| `GET /percmd` | Profiles grouped by launch command |
| `GET /get?jobid=XXX` | Get a specific profile |
| `GET /join?to=HOST:PORT` | Add a scrape target (proxy or Prometheus exporter) |
| `GET /join/list` | List current scrape targets |

Example — add a node exporter:
```bash
curl http://localhost:1337/join?to=localhost:9100
```

---

## Malleability Support (TBON Expand / Shrink / Graceful Leave)

The proxy supports dynamic changes to the tree-based overlay network (TBON) at runtime. This is useful for malleable HPC jobs where nodes are added to or removed from an allocation while the proxy tree is live.

### How the TBON Works

Each node runs one `proxy_v2` process. One node is the **root** (no `--root-proxy`); the others are **children** (`--root-proxy <root-addr>`). Children register with the root at startup via `/join`, and the root periodically scrapes them.

### Auto-Discovery (`--auto-root` / `--root-url-dir`)

On a shared filesystem, the root proxy writes its URL to `<target-prefix>/root.url` at startup. Child proxies launched with `--auto-root` read this file instead of requiring a hardcoded `--root-proxy` address:

```bash
# Root node — writes root.url into its profile directory
proxy_v2 --port 1337 --target-prefix /shared/proxy/root

# Child node — discovers root from the shared file
proxy_v2 --port 1337 \
  --target-prefix /shared/proxy/$(hostname) \
  --auto-root \
  --root-url-dir /shared/proxy/root
```

`--root-url-dir` overrides the directory to search for `root.url`. Without it, `--auto-root` reads from `<target-prefix>/root.url`. The root URL can also be injected via the `PROXY_ROOT_URL` environment variable (takes precedence over `--auto-root`).

### Graceful Leave (`/leave` endpoint)

When a child proxy receives SIGTERM, it sends a `/leave?from=<my-url>` request to the root before exiting. The root immediately removes the departing node from the TBON — no waiting for a missed scrape.

```
GET http://<root>:<port>/leave?from=<child-url>
```

This is handled automatically by the signal handler; no user action is needed. In the worst case (SIGKILL / crash), the existing self-repair mechanism takes over (see below).

### TBON Self-Repair (Shrink)

If the root fails to scrape a child proxy (connection refused / timeout), it removes the dead node from the topology. The repair happens within one sampling period (`--sampling-period`, default 1000 ms).

### Using the Proxy with DMR (Dynamic Resource Manager)

Each node in the DMR allocation should run one proxy:

| Role | Command |
|------|---------|
| Root node | `proxy_v2 --port 1337 --target-prefix <shared-fs>/root` |
| Worker nodes (static) | `proxy_v2 --port 1337 --root-proxy <root-addr>:1337` |
| Worker nodes (malleable) | `proxy_v2 --port 1337 --auto-root --root-url-dir <shared-fs>/root` |

The instrumented application communicates with the local proxy via the UNIX socket (`proxy_run` or the `libproxyclient.so` LD_PRELOAD). No special environment variable is needed beyond `PROXY_JOB_ID` (or the SLURM / MPI job ID picked up automatically by `proxy_run`).

> **Note on `libproxyclient.so` LD_PRELOAD**: The ELF constructor that auto-connects the client library on load may not fire in all build configurations. If metrics are not appearing, call `proxy_init()` explicitly early in your application or use `proxy_run` as the launcher wrapper.

### Malleability Experiment

An automated end-to-end test lives in `experiment/run_malleability_test.sh`. It uses a 4-node Docker cluster (`dmr01`–`dmr04`) to exercise all three scenarios in sequence:

1. **Graceful leave** — SIGTERM a child; `/leave` triggers immediate TBON repair
2. **Shrink** — SIGKILL a child; root detects the missing scrape and self-repairs
3. **Expand** — restart the killed child with `--auto-root`; it reads `root.url` and self-registers

See [`experiment/`](experiment/) for setup instructions and expected output.

## Acknowledgments

This project has received funding from the European Union's Horizon 2020 JTI-EuroHPC research and innovation programme with grant Agreement number: 956748
