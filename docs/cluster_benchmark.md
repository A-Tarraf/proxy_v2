# Measuring exporter overhead on a cluster

How to find out what instrumentation actually costs *your* application, on real
nodes and a real parallel filesystem.

Script: **[`sbatch_overhead_sweep.sh`](sbatch_overhead_sweep.sh)**
Cost model: **[`strace_exporter.md`](strace_exporter.md)**

---

## 1. What the sweep measures

The same application is run five ways on the same nodes, alternating between
configurations within each repetition so that filesystem drift and node
variation hit every configuration equally:

| config | invocation | what it isolates |
|---|---|---|
| `plain` | bare `srun` | uninstrumented baseline |
| `mpi` | `proxy_run -e mpi` | PMPI wrappers only — no `ptrace` |
| `strace_all` | `proxy_run -T all` | trace **every** syscall (the pre-2026-07 default) |
| `strace_default` | `proxy_run` | seccomp-BPF denylist (current default) |
| `strace_futex` | `proxy_run -T '!getpid,…'` | denylist, but **keep `futex`** |

Three questions:

1. **Is `-e mpi` really free?** It should be indistinguishable from `plain` —
   PMPI wrappers are in-process function interposition, with no context switch.
2. **How bad is the ptrace tax on a real MPI code?** `strace_all` vs `plain`.
3. **Can we afford to keep `futex`?** — see §5. This is the open question.

---

## 2. Step by step

Verified end to end on Lichtenberg (TU Darmstadt), 2026-07-14.

### Step 0 — Build and install

Make sure a recent Rust toolchain is loaded (the *system* rustc may be too old and
gets picked up first; that produces a confusing "rustc 1.86 is not supported by
`home`/`time`" error). Then:

```sh
cd ~/github/proxy_v2
git pull
./install.sh $TOOLS_PREFIX          # such that $TOOLS_BIN = $TOOLS_PREFIX/bin
```

`install.sh` skips the strace build if `proxy_exporter_strace` already exists in
the prefix, so a re-install takes about a minute.

### Step 1 — Put the tools on your PATH

This is the **only** environment setup needed; the job script handles the rest.

```sh
export PATH=$TOOLS_BIN:$PATH
export LD_LIBRARY_PATH=$TOOLS_LIB:$LD_LIBRARY_PATH
```

### Step 2 — Check the build has the new flags

```sh
proxy_v2  --help | grep -c no-ftio        # must be 1
proxy_run --help | grep -c strace-trace   # must be 1
```

If `--strace-trace` is missing, `strace_all` and `strace_futex` will die on an
unknown argument.

### Step 3 — Check seccomp-BPF actually engages

**Do not skip this.** If the kernel filter is silently inert you pay the full
ptrace cost while believing you are filtered, `strace_default` comes back
identical to `strace_all`, and the whole sweep is meaningless. The sweep itself
cannot detect this.

First, the negative check — this must print **nothing**:

```sh
proxy_exporter_strace -c -f --seccomp-bpf -e trace=%desc -- /bin/true 2>&1 | grep -i seccomp
```

(A warning here usually means `--seccomp-bpf` was used without `-f`, which makes
strace disable it silently.)

Then the positive check — trace a workload made almost entirely of syscalls the
filter excludes, and compare:

```sh
time proxy_exporter_strace -c -- \
     python3 -c "import os
for _ in range(200000): os.getpid()"

time proxy_exporter_strace -c -f --seccomp-bpf -e trace=%desc -- \
     python3 -c "import os
for _ in range(200000): os.getpid()"
```

Measured on Lichtenberg:

| invocation | wall | syscalls trapped |
|---|---|---|
| `-c` (trace everything) | 2.043 s | 201,990 |
| `-c -f --seccomp-bpf -e trace=%desc` | **0.119 s** | **1,881** |

**17× faster, and `getpid` disappears from the summary table entirely** — those
200,000 calls never trapped into the tracer; the kernel dropped them. That is the
mechanism the whole sweep depends on. If the two runs take the same time, stop:
seccomp is not working.

### Step 4 — Start the root proxy on the login node

Leave it running for the duration of the experiments. `--no-ftio` avoids a ~5 s
startup probe for an FTIO server and a periodic "FTIO client address not set"
error on every trace cycle; the sweep only needs metric collection.

```sh
proxy_v2 -t $HOME/proxy_traces -S 1000 -m 128 --no-ftio

# in another shell:
curl http://localhost:1337/job/list        # must return JSON
```

Note the endpoint is `/job/list`, **not** `/joblist`.

---

## 3. Step 5 — Submit

```sh
sbatch sbatch_overhead_sweep.sh

# knobs (all optional)
REPS=5             sbatch sbatch_overhead_sweep.sh   # more repetitions
PARTICLES=20000000 sbatch sbatch_overhead_sweep.sh   # larger problem (see below)
ROOT_PROXY=<host> sbatch sbatch_overhead_sweep.sh   # different login node
ROOT_ON_ALLOC=1   sbatch sbatch_overhead_sweep.sh   # root on a compute node
```

Edit the placeholders at the top first: `YOUR_ACCOUNT`, `YOUR_CONSTRAINT`,
`LOGIN_NODE`, `YOUR_APPLICATION`, and the scratch path.

**Size the problem so the run is long enough to measure.** With HACC-IO at
`PARTICLES=1000000` over 64 ranks the application finishes in ~3 s — far too short
to separate instrumentation cost from startup noise. Start at `PARTICLES=20000000`
and adjust so an uninstrumented run takes at least a minute.

Output:

- `overhead_sweep_<jobid>.csv` — one row per run
- a summary table with `vs plain` ratios at the end of the job's `.out`

### Reading the CSV — check this before anything else

```
config,rep,tag,wall_s,local_ok,remote_ok,instrumented,app_out
```

**Only trust rows where `local_ok=yes` AND `remote_ok=yes`.** Anything else means
the run was not properly instrumented and its wall time is fiction; those rows are
marked `NO-UNINSTRUMENTED` and excluded from the summary averages.

- `local_ok=NO` — the exporter could not reach the proxy **on its own node**.
- `remote_ok=NO` — the exporter connected locally, but the data never reached the
  root (a leaf alive but not relaying).

---

## 4. The failure mode you must guard against

**This is the most important section of this document.**

Leaf proxies are started with `-r <root>`. If the root is unreachable, the leaf
calls `exit(1)` (`src/main.rs:203`). With no proxy on the node:

1. `metric_proxy_init()` fails in the application,
2. every counter handle comes back `NULL`,
3. `metric_proxy_counter_inc()` returns immediately at its null check,
4. **the application runs completely uninstrumented, at full speed.**

You record a beautiful number and conclude the exporter is cheap. Nothing warns
you. **An uninstrumented run is fast, so this failure looks exactly like a win.**

A stale measurement in this project's own history — *"strace with no proxy
running: 18.6 s, free"* — is this artefact. Confirmed on the cluster: a tracer
with no proxy still traces every syscall (200,000 `getpid` calls, `Not Connected
to Metric Proxy`), so that row was never measuring cheap tracing — it was
measuring an application that was not instrumented at all.

There is a second way to get here, and it is subtle: **two proxies on one node.**
A leftover proxy from a cancelled job, or (with `ROOT_ON_ALLOC=1`) the per-node
leaf landing on the node that already hosts the root. The second proxy unlinks the
live one's UNIX socket, binds its own, and only then dies on `AddrInUse` — leaving
a stale socket with no listener. The survivor keeps serving HTTP and looks
perfectly healthy while every exporter on that node gets `ECONNREFUSED`.
`proxy_v2` now refuses to start if its port is taken, before touching the socket,
so this cannot happen — but a proxy built before 2026-07-14 will still do it.

The script therefore checks four times:

1. **Kill stray proxies** on every node before starting.
2. **Pre-flight** — is the root answering at `/job/list`? If not, abort.
3. **Every node** — does each allocated node have a live proxy? If not, abort.
4. **After every run** — two independent checks, both must pass:
   - **LOCAL**: the application's output contains no `Not Connected to Metric
     Proxy` (the exporter reached the proxy on its own node).
   - **REMOTE**: the root's `/metrics` counter sum for that exporter *increased*
     (the data actually travelled leaf → root). This catches a leaf that is alive
     but not relaying, which LOCAL cannot see.

   Rows failing either check are marked `NO-UNINSTRUMENTED` and **excluded from
   the averages**.

**If you write your own script, do NOT use `/job/list` to check whether a run was
instrumented.** It lists only *live* jobs, so it is always empty by the time a run
has finished. (An earlier version of the sweep did exactly this — against the
nonexistent `/joblist` endpoint, no less — and flagged every single run as
uninstrumented.)

### Related trap: `-i` means "persist nothing"

`-i` (`--inhibit-profile-aggregation`) sets `aggregate = false`, which gates both:

```rust
// exporter.rs:928 — per-job trace
let trace = if self.aggregator { … } else { None };          // no trace kept

// exporter.rs:1011 — on job end
if self.aggregator { self.profile_store.saveprofile(…)?; }   // no profile saved
```

A leaf with `-i` holds live counters in memory **only** so a root can scrape
them. It writes nothing to disk. That is correct for a leaf relaying to a root,
and it means **the root is not optional in this topology** — without it, the data
is simply gone.

If you want a rootless setup, drop `-i` and give each node its own `-t`
directory on shared scratch; every proxy then persists locally. You lose the
aggregated view and the web UI, but nothing else.

---

## 5. Reading the results

```
config              mean(s)   vs plain
plain                 21.40      1.0x
mpi                   21.612     1.0x     <- free, as expected
strace_all           183.72      8.6x     <- the ptrace tax (old default)
strace_default        34.15      1.6x     <- seccomp-BPF filtering
strace_futex           ?           ?      <- the question
```

*(Illustrative shape, not measured values.)*

- **`mpi` ≈ `plain`** — expected. If not, something else is wrong.
- **`strace_all` ≫ `plain`** — the ptrace tax: ~50 µs per traced syscall, twice
  per call (entry + exit). It scales with **syscall count**, which is why the
  sampling period `-S` makes no difference. Large-I/O codes (few, big `write`s)
  are cheap to trace; metadata- or small-write-heavy codes are not.
- **`strace_default` ≪ `strace_all`** — confirms the seccomp-BPF denylist works
  on a real MPI code, not just on synthetic microbenchmarks.

### `strace_futex` — the open question

`futex` is on the default denylist because MPI progress engines spin on it, so
it is *usually* the hottest syscall in an MPI run. But `strace___time___futex` is
**synchronisation/wait time** — exactly the metric you want for load-imbalance
analysis. So:

| outcome | conclusion | action |
|---|---|---|
| `strace_futex` ≈ `strace_default` | futex is cheap on this code | **take `futex` off the default denylist** — the metric is free |
| `strace_futex` ≈ `strace_all` | futex *is* the hot path | the denylist is correct as shipped |

This has **not** been measured on a real MPI application — only on synthetic
workloads with no MPI at all. This sweep is what settles it.

---

## 6. What to do with the numbers

- If `strace_all` is catastrophic and `strace_default` is acceptable, the default
  filter is doing its job — document the measured ratio in
  [`strace_exporter.md`](strace_exporter.md).
- If **`-e mpi` is free and gives you the metrics you need**, prefer it. Syscall
  tracing buys detail that MPI-level interposition cannot see (I/O that bypasses
  MPI, metadata operations), at a price set by how chatty the application is.
- If the overhead is still too high with the default filter, narrow `-T` further
  (`-T '%desc,%network'`), or move to `-e mpi`. Do **not** reach for `-S`: the
  sampling period does not affect tracing cost.
