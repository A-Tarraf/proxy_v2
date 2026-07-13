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

## 2. Prerequisites

**A root proxy must be running on the login node** before you submit:

```sh
ssh <login_node>
bash docs/start_root_proxy.sh
curl http://localhost:1337/joblist     # should return JSON
```

**The build must have the `-T` flag** (otherwise `strace_all` and `strace_futex`
die on an unknown argument):

```sh
proxy_run --help | grep strace-trace
```

---

## 3. Running it

```sh
sbatch sbatch_overhead_sweep.sh

# knobs (all optional)
REPS=5            sbatch sbatch_overhead_sweep.sh   # more repetitions
PARTICLES=5000000 sbatch sbatch_overhead_sweep.sh   # larger problem
ROOT_PROXY=<host> sbatch sbatch_overhead_sweep.sh   # different login node
ROOT_ON_ALLOC=1   sbatch sbatch_overhead_sweep.sh   # root on a compute node
```

Edit the placeholders at the top first: `YOUR_ACCOUNT`, `YOUR_CONSTRAINT`,
`LOGIN_NODE`, `YOUR_APPLICATION`, and the scratch path.

Output:

- `overhead_sweep_<jobid>.csv` — one row per run
- a summary table with `vs plain` ratios at the end of the job's `.out`

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
running: 18.6 s, free"* — is almost certainly this artefact: it was not measuring
cheap tracing, it was measuring **no tracing**.

The script therefore checks three times:

1. **Pre-flight** — is the root answering at all? If not, abort with instructions.
2. **After launching leaves** — did any leaf register with the root? Zero ⇒ abort.
3. **After every run** — did the root actually *see* this job id? If not, the row
   is marked `NO-UNINSTRUMENTED` and **excluded from the averages**.

If you write your own script, keep check 3. It is the only one that catches a
mid-sweep proxy death.

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
