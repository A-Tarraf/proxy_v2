# Workshop Paper: Contributions and Experiment Plan

Working title: *Online I/O Periodicity Detection on Malleable HPC Jobs: Metric
Semantics, Overhead, and Fault Tolerance in a Tree-Based Monitoring Overlay*

Status: 2026-07-13. Fork `A-Tarraf/proxy_v2` @ `development`, 35 commits ahead of
`besnardjb/proxy_v2` @ `master`, 0 behind (~4.8k LOC added in `src/` + `static/`).

---

## 0. The claim

Neither Metric Proxy (Besnard) nor FTIO (Tarraf et al.) is a contribution of this
paper. **The contribution is the online coupling of the two, and the four things
that coupling breaks until they are fixed:**

1. the *metric semantics* are wrong for periodicity detection (bursts land on the
   wrong timestamps) — \S1;
2. the *concurrency* is not constant, because the job is malleable — \S2;
3. the *overlay* must survive nodes disappearing — \S3;
4. the detector was designed for **one** signal and is now handed $O(10^3)$ of
   them — \S4;

and, as a fifth item that turns a limitation into a result:

5. the *overhead* of interposition-based monitoring is not what everyone assumes
   it is — \S5.

State this in the introduction, explicitly, before a reviewer says it for you.

---

## 1. C1 — Dewrapped bandwidth (`src/trace.rs:45`)

### The pathology

Both exporters account **at call return** (MPI wrapper at `CALL_END`, strace at
syscall exit). Let $S(t)$ be the cumulative bytes counter and $T(t)$ the
cumulative in-call time counter, sampled at $t_0 < t_1 < \dots < t_N$, and write

$$\Delta S_k = S(t_k) - S(t_{k-1}), \qquad \Delta T_k = T(t_k) - T(t_{k-1}).$$

A burst that spans several sampling periods contributes **nothing** to the
samples it actually overlaps and **everything** to the single sample at which it
completes. The signal is *wrapped* onto the wrong timestamps. This is a general
property of interposition-based I/O monitoring, not a quirk of this tool — that
is what makes it publishable.

A second, independent pathology: the per-node profile **sums** counters across
the ranks on that node, so $T$ is *total seconds spent inside the call across all
local ranks*. In a 1 s window, $\Delta T_k$ can legitimately reach $n$ seconds
for $n$ ranks. Hence the upstream gauge

$$B_{\mathrm{wrap}}(t_k) = \frac{\Delta S_k}{\Delta T_k}$$

is the **average per-rank in-call speed**, not wall-clock node throughput — and
it is attributed to the completion sample. Upstream's `___bandwidth___` had
exactly these semantics. We removed it.

### The reconstruction

Let $n(t)$ be the live rank count of the job (`job_mpi_ranks`, \S2). Spread each
burst **backwards** from its completion sample over its estimated wall-clock span:

$$
\delta_k = \frac{\Delta T_k}{n(t_k)}, \qquad
s_k = \max\!\left(t_k - \delta_k,\; t_0\right), \qquad
r_k = \frac{\Delta S_k}{t_k - s_k}
$$

with sample-and-hold from $s_k$ and a closing zero. The virtual metric
`___bandwidth_dewrap___<fn>` is exactly this series.

**Invariant to state in the paper:** the reconstruction is
byte-conserving,

$$\int_{t_0}^{t_N} B_{\mathrm{dewrap}}(t)\,\mathrm{d}t \;=\; S(t_N) - S(t_0),$$

which the wrapped gauge does not satisfy in any meaningful sense.

### The architectural point (worth its own paragraph)

`___bandwidth_dewrap___` is a **virtual metric**: nothing is stored, the trace
file stays append-only, and the "past" points exist only in the derived view.
It is computed in **two** places by consumer:

| Consumer | Where | When |
|---|---|---|
| Trace UI plot | proxy, `dewrap_bandwidth()` in `src/trace.rs` | on the fly, only when plotted |
| FTIO analysis | FTIO, `parse_proxy.py` | when `--dewrap` is passed — **on by default** |

The proxy deliberately does **not** pre-compute dewrapped series for FTIO,
because FTIO numerically derivates every metric not named `deriv*` and would
mangle an already-derived rate. That is a real cross-tool design constraint and
it reads well in a paper.

### Supporting subsection: counter vs. gauge aggregation

Counters merge by **summation** (across ranks on a node, across nodes at the
root); gauges merge by **averaging** ($\mathrm{total}/\mathrm{hits}$), which is
why node-wide gauges show decimals at the root. `docs/metrics_semantics.md` is
effectively this subsection already.

---

## 2. C2 — Malleability-aware monitoring

`job_mpi_ranks`: per-job, live, counted from rank connections (incremented when a
connection identifies as an MPI rank of the job, decremented on disconnect),
written with **set** semantics per node and **summed** across nodes at the root.
Always an integer; it *steps* when a malleable job grows or shrinks.

**Why this is not bookkeeping:** it is the $n(t_k)$ in the dewrap of \S1. A
malleable job that resizes mid-run changes the divisor of the bandwidth
reconstruction. Assume $n$ constant and the reconstructed signal is wrong exactly
at the moments a malleability-aware scheduler cares about. This is what ties C2
to the core claim instead of leaving it as a feature bullet.

Contrast with `proxy_mpi_ranks` / `proxy_connected_procs`: node-wide gauges,
averaged on merge, useful only as liveness signals.

**Disclose:** on the launcher node, the DMR wrapper's own MPI helper processes
register under the same job id and are counted (they also account their own
MPI-IO bytes there). A stated limitation beats a suspiciously clean number.

**Be precise:** the proxy *observes* malleability; it does not *drive* it.

---

## 3. C3 — TBON topology, failure, auto-repair (`src/webserver.rs:601–1000`)

- **Binomial-tree construction** (`add_node_to_subtree` / `get_subtree_size`) when
  fan-out is unconstrained; min-depth $k$-ary placement when `--branches` caps it.
- **Idempotent join/pivot.** A proxy that re-pivots after a reconnect, or was
  manually joined and then pivots, must not acquire **two parents** — its metrics
  would be summed twice at the root. Concrete, easily-explained distributed-systems
  bug; concrete fix (`already_in_tree()`).
- **Auto-repair** (`handle_remove`): a parent detecting an unresponsive child
  selects a replacement, detaches it from its own parent, splices it into the
  failed node's slot, reparents the orphaned children onto it via
  `/join/multiple`, and propagates their sampling periods.
- **`relax_tracked_jobs()`** on scraper removal: without it, a dead leaf leaves its
  jobs marked *live* at the root forever. The difference between a monitoring
  system that recovers and one that lies.

**Disclose honestly:** the replacement heuristic is `max_by_key(depth)` and does
**not** provably preserve the binomial structure. Frame as: *restores connectivity
and aggregation correctness; degrades topology optimality.* Reporting the
degradation is stronger than pretending it does not happen.

---

## 4. C4 — Analysis at metric-space scale (the FTIO tab)

With the strace exporter a job trace carries hits/size/time **per syscall**, plus
`deriv__*` series, plus one virtual dewrap metric per size/time pair — $O(10^2)$
to $O(10^3)$ series per job. FTIO was designed for **one** bandwidth signal. So
the paper gets to ask a question nobody has had to ask:

> When the monitoring system hands you a thousand candidate signals, **which one
> do you give the periodicity detector?**

Built to answer it: run-all across every metric of a job with progress reporting
(`get_ftio_progress` $\to$ (done, total)), per-metric on-demand runs, a browsable
per-job model store, FTIO server log streaming into the UI, dynamic port
renegotiation, the full FTIO argument surface exposed and persisted.

**In-situ scheduling discipline** — this is the quantified systems result of C4:

- FTIO dispatches to its **own thread**. In the sequential scrape loop, one 30 s
  ZMQ timeout stalled *every* scraper; a 40 s job recorded exactly **one** trace
  point.
- **At most one analysis in flight per proxy** (`FtioClient::in_flight`).
- **Only real user jobs** are analyzed — not `main`, not the `Node: *`
  housekeeping traces. Pre-fix: 9 proxies $\times$ every trace $\times$ every 10 s
  $\Rightarrow$ host load 24, HACC-IO runs 8–13 min.

After the fixes: full 1 Hz trace fidelity (170–178 points over a 180 s job).

---

## 5. C5 — Overhead: it is the ptrace tax, and it scales with syscall count

> **2026-07-13 — E0 WAS RUN. THE ORIGINAL HYPOTHESIS IS FALSIFIED.**
> The write-amplification theory below (kept for the record) is **wrong as an
> explanation of the strace slowdown**. Measured on a syscall-dense workload
> (2M `write()` to `/dev/null`, 16-core host, 3 reps):
>
> | mode | mean | vs plain |
> |---|---|---|
> | plain | 1.18 s | 1.00× |
> | strace, **no proxy listening** | 106.96 s | **90.4×** |
> | strace + proxy, unbuffered push | 103.94 s | 87.8× |
> | strace + proxy, buffered push (fix) | 97.32 s | 82.2× |
>
> **ptrace alone accounts for the entire cost.** The proxy contributes nothing
> measurable; the push-path fix buys ~6 %, inside the noise (runs spanned
> 73–162 s). Independently confirmed by arithmetic: 2M syscalls × 2 stops ×
> ~25 µs ≈ 100 s. **The cost is ≈50 µs per traced syscall and scales with
> syscall count, full stop.**
>
> **Corrected story for the paper:** the MPI exporter is free because it
> intercepts *coarsely* (a few MPI calls); the strace exporter is expensive
> exactly in proportion to how many *fine-grained* syscalls the application
> issues. HACC-IO does large writes (few syscalls) and is therefore cheap to
> trace; a metadata- or small-write-heavy application is not. This is a
> quantified, useful result — and it argues directly for replacing ptrace with
> **eBPF** in the syscall exporter, which would be a real contribution.
>
> **Also invalidated:** `speed_report.md`'s "strace alone, no proxies = 18.6 s
> (free)" probe. Re-measured here, a tracer with no proxy costs the *same* as
> one with a proxy (106.96 s vs 103.94 s). That row must be re-run; the
> conclusion drawn from it ("bandwidth/accounting is what costs") does not hold.
>
> **What survives:** the write amplification is real and was fixed (see below) —
> 29,622 → 13 `sendto` per 10 push windows, and **flat in $N$** where it
> previously grew with $N$ and saturated the push thread at $N{=}1000$. Keep it
> as an engineering/scalability fix. It is **not** an application-overhead
> contribution.

### [SUPERSEDED — retained for the record] The observation that did not fit

| Configuration | HACC-IO wall time (Docker, 4 nodes) |
|---|---|
| uninstrumented | 20.8 / 22.9 / 22.5 s |
| `proxy_run -e mpi` (MPI exporter only) | **19.2 s** — free |
| `proxy_run` full, **no proxy running** | **18.6 s** — free |
| `proxy_run` full, proxies running | **60.1 s** — 3$\times$ |

Same binary, same ptrace, same syscall stops in rows 3 and 4. **ptrace
interception itself costs nothing.** The $3\times$ appears only when a proxy is
*listening*.

Note also that row 3 is **not** the ablation it was taken to be: with no proxy,
`metric_proxy_init` fails, `running = false`, every counter handle is `NULL`, and
`metric_proxy_counter_inc` returns at the null check (`src/lib.rs:625`). ptrace is
live but the entire accounting *and* push path is compiled out.

### The mechanism

`MetricProxyClient::send()` (`src/lib.rs:~305`):

```rust
serde_json::to_writer(&mut stream, cmd)?;   // raw UnixStream — NO BufWriter
stream.write_all(&null_byte)?;
```

There is **no `BufWriter` anywhere on the exporter$\to$proxy path**.
`serde_json::to_writer` against an unbuffered `Write` emits each structural token
as its own `write(2)`. And `dump_values()` sends **one message per updated
counter**, every `PROXY_PERIOD` (default **1000 ms**, `src/proxy_common.rs:58`).

Let $N$ be the number of live counters, $c$ the syscalls per JSON message
($c \sim 10$–$30$ unbuffered), $P$ the push period. The tracer performs

$$W \;\approx\; \frac{N \cdot c}{P} \quad \text{write(2) calls per second,}$$

**from inside the strace tracer process** — and the tracer sits on the critical
path of *every* application syscall (the tracee is frozen in a ptrace-stop until
the tracer completes `waitpid` + `PTRACE_SYSCALL`). Tracer syscalls and tracer CPU
therefore convert more or less directly into **application stall**.

### It explains every number

$N$ is the discriminator:

- **MPI exporter:** a handful of functions $\times$ hits/size/time $\Rightarrow$
  $N \sim 30$–$60$. Cheap push. *Measured free.* ✔
- **strace exporter:** every syscall number $\times$ hits/time/size $\Rightarrow$
  $N \sim 10^2$–$10^3$. *Measured $3\times$.* ✔
- **No proxy:** $N$ effectively 0 (null handles). *Measured free.* ✔

It also explains **probe C**, otherwise baffling: lowering the *proxy's scrape*
period `-S` by $10\times$ changed nothing. Of course — that is the wrong knob.
`PROXY_PERIOD` (the *client's push* period) is a different variable and was
**never varied**.

### Falsifiable prediction

Overhead scales as $N/P$. Sweeping `PROXY_PERIOD` must move the runtime. If it
does not, this section is wrong and must be cut. **Run E0 before writing a word.**

### The fixes (cheapest first)

1. `BufWriter` around the `UnixStream`, flush once per period. Near one line.
2. Batch the dump into **one** message per period (`ProxyCommand::Values(Vec<_>)`)
   instead of $N$ messages. Wire-protocol change on both ends; structurally right.
3. Switch the exporter wire protocol to **msgpack** — `rmp_serde` is already a
   dependency for the FTIO path, and this exact migration was already done for ZMQ
   (commit `ac0e47a`). JSON-per-counter over a Unix socket is the last holdout.
4. Move the `Desc` `send()` in `push_entry` off the syscall-exit path.
5. Optional: lock-free `inc` (`AtomicU64` bit-punned `f64`) to drop the per-syscall
   mutex.

### The result, if E0 confirms

> Interposition-based monitoring couples the tracer to the application's critical
> path, so the exporter's **push** cost is paid in **application stall** — an
> effect invisible to conventional overhead accounting, which measures the
> exporter's own CPU. We identify a write-amplification pathology in the metric
> push path, fix it, and reduce syscall-tracing overhead from $3\times$ to near-free.

Generalizes well beyond this tool. Obtainable on hardware already available.

---

## 6. C6 — Robustness (one paragraph, do not oversell)

NaN/Inf sanitization on serialization (was emitting `null` into Prometheus/JSON),
corrupt-profile skip at startup, 5 s proxy-to-proxy scrape timeouts, adaptive
tick. Nine unit tests where upstream had zero. Buys "this actually ran."

---

# Experiments

Ordered by how much each decides whether the paper lands. **E0 first** — it
determines what the paper *is*.

## E0 — `PROXY_PERIOD` sweep (settles C5) — **BLOCKING**

| | |
|---|---|
| **Question** | Does overhead scale as $N/P$, i.e. is the cost in the push path rather than ptrace? |
| **Method** | HACC-IO, full instrumentation, vary **only** `PROXY_PERIOD` $\in \{100, 1000, 10000\}$ ms. Independently, count $N$: `curl root:1337/metrics \| grep -c strace___` during a run. |
| **Predict** | Runtime falls monotonically as $P$ grows. If flat $\Rightarrow$ C5 is wrong, cut \S5. |
| **Then** | Apply fixes 1–3, rerun. Target: strace overhead $\to$ within noise of `-e mpi`. |
| **Where** | Laptop is sufficient to *falsify*; cluster for the publishable numbers. |
| **Cost** | ~30 min to falsify. Half a day with the fixes. |

**Report:** runtime and `write(2)` count in the tracer vs. $P$; before/after the
BufWriter+batch fix; $N$ for MPI vs. strace exporter. This is the money figure of
\S5.

## E1 — Dewrap ablation (settles C1) — **THE HEADLINE**

| | |
|---|---|
| **Question** | Does dewrapping actually recover the period that the wrapped signal loses? |
| **Method** | Synthetic MPI-IO benchmark with an **imposed, known** period. Feed FTIO (a) the raw cumulative counter, (b) upstream's $\Delta S/\Delta T$ gauge, (c) the dewrapped signal. |
| **Sweep** | The ratio $\rho = \delta_{\text{burst}} / P_{\text{sample}}$. The wrapped signal must degrade precisely as bursts start spanning multiple sampling periods. |
| **Report** | Detected frequency + FTIO confidence vs. ground truth, for each of (a)(b)(c), across $\rho$. **Predict the breakdown point of (b) analytically and show it.** |
| **Cost** | 1–2 days. |

Predicted-vs-observed breakdown is the figure the paper is built on. Without E1,
dewrapping is an implementation detail rather than a contribution.

## E2 — Malleability $\times$ dewrap (settles C2)

| | |
|---|---|
| **Question** | Does a static rank assumption break the reconstruction across a resize? |
| **Method** | `HACC_IO_Malleable` (already in `/d/github/HACC-IO`) + DMR. Grow/shrink mid-run. Compare dewrap with $n(t)$ from `job_mpi_ranks` vs. dewrap with $n$ pinned to the initial rank count. |
| **Report** | Reconstructed bandwidth vs. ground truth across the resize; FTIO detected period for both. Byte-conservation error for both. |
| **Cost** | 1 day. Cheap — the harness exists. |

Converts malleability from "we support this too" into a second real result.

## E3 — Scale on real nodes (C3 credibility) — **required to claim anything about a tree**

| | |
|---|---|
| **Question** | Does TBON aggregation scale, and does the overhead stay flat? |
| **Method** | Lichtenberg. $n \in \{4, 16, 64, 128, 256\}$ nodes. Wire up `ExperimentInstrumentation` (built for exactly this, currently **unused** — `-i` flag). Compare binomial vs. flat vs. $k$-ary (`--branches`). |
| **Report** | Aggregation latency and root-visibility delay vs. $n$; per-proxy CPU vs. $n$; end-to-end metric latency distribution. |
| **Cost** | 2–3 days incl. queue time. |

9 Docker containers on an 8-core laptop is **not** evidence about a tree-based
overlay. Reviewers will forgive modest scale; they will not forgive a laptop.

## E4 — Failure injection (settles C3)

| | |
|---|---|
| **Question** | Does the tree recover, and are metrics lost or **double-counted** during repair? |
| **Method** | Kill leaves and **interior** nodes at known times. The interior case is what actually tests the idempotence fix and the child-reparenting path. |
| **Report** | Detection latency; repair latency; metric loss **and double-count** in the repair window (feed a known constant-rate counter so the ground truth is exact); tree balance before/after. |
| **Cost** | 1–2 days, folds into the E3 allocation. |

Include the case where the `max_by_key(depth)` heuristic degrades the topology.

## E5 — Metric-space sweep (settles C4)

| | |
|---|---|
| **Question** | Given $O(10^3)$ signals, does the tool find the right one *without being told*? |
| **Method** | Run-all FTIO over a fully strace-instrumented job. |
| **Report** | $N$ metrics; sweep wall time; how many yield a confident model; **and which** — does dewrapped MPI-IO bandwidth rank at the top? |
| **Cost** | 1 day. |

Novel experiment; nobody has had to ask this question before.

## E6 — HACC-IO, properly (replaces the unusable numbers)

Current numbers (101.8 / 190.6 / 169.9 s on a $\geq$95 %-full filesystem) are noise
and the report says so. Redo on Lichtenberg: real parallel FS, dedicated nodes,
$\geq 5$ runs, **report variance**.

**Prediction to state beforehand and then confirm:** relative strace overhead
*shrinks* on a real parallel filesystem, because each traced syscall takes longer
anyway, so the fixed per-syscall tracer cost amortizes. Stating the prediction up
front and confirming it is far stronger than a bare table.

## E7 — One real periodic application (end-to-end)

An application with genuinely periodic I/O (checkpointing simulation; DLIO with
`checkpoint=True` is already wired in `sbatch_proxy.sh`). Show **online**
detection of the checkpoint period, in situ, while the job runs. One app suffices
for a workshop.

## E8 — Baseline / related-work defence

"Why not Darshan / LDMS / TAU?" is the first reviewer question. You need not beat
them; you must explain that they are post-mortem or do not feed an online
detector. Ideally one number: Darshan gives byte totals but not the timing needed
to recover the period. `sbatch_proxy.sh` already has a Darshan `LD_PRELOAD` block
commented out — reuse it.

## Priority

- **Must have:** E0, E1, E2, E5 — all runnable on hardware in hand. These are the
  novelty.
- **Credibility tax:** E3, E4, E6 — need the cluster. Required to claim anything
  about a *tree*.
- **Nice:** E7, E8.

If cluster time is scarce: run E0/E1/E2/E5 first and reassess. The strongest paper
may be *"metric semantics and overhead for online I/O analysis"*, in which case the
TBON work demotes to supporting infrastructure and needs far less scale to defend.

---

# Cluster harness: adapting the sbatch scripts

Existing, in `/d/github/HACC-IO`:

- `sbatch.sh` — HACC_ASYNC_IO, `-n 192`, plain `srun`. The **uninstrumented
  baseline**.
- `sbatch_proxy.sh` — actually DLIO, not HACC. Already does the right things:
  one proxy per node via `srun --overlap` with a dedicated core
  (`CPUS_PROXY=1`), `-r http://${ROOT_PROXY}:1337` to a root proxy on the login
  node, `-S 100`, and it already passes `-i` (**instrumentation**, i.e. the
  `ExperimentInstrumentation` hook E3 needs).
- `HACC_IO_Malleable` — the binary E2 needs.

### Proposed: one parameterized `sbatch_sweep.sh`

Drive everything from environment variables so a single script covers E0/E3/E6,
and each Slurm job emits one row of a results table.

| Variable | Sweeps | Used by |
|---|---|---|
| `PROXY_PERIOD` | 100 / 1000 / 10000 ms — **client push period** | **E0** |
| `EXPORTERS` | `mpi` / `mpi,strace` / none (uninstrumented) | E0, E6 |
| `SLURM_NNODES` | 4 / 16 / 64 / 128 / 256 | E3 |
| `PROXY_BRANCHES` | 0 (binomial) / 2 / 4 / $\infty$ (flat) | E3 |
| `PROXY_S` | 100 / 1000 — proxy **scrape** period (the *other* knob) | E3, control for E0 |
| `PROXY_BUFFERED` | pre-/post-BufWriter build | E0 |
| `KILL_NODE_AT` | seconds into the run; leaf vs. interior | E4 |

Each run must record: wall time, in-app time (HACC's own `JOBEND−JOBSTART`),
HACC-reported bandwidth, $N$ = live counter count
(`curl root:1337/metrics | grep -c strace___`), tracer `write(2)` count, and the
`ExperimentInstrumentation` dump (aggregation latencies, end-to-end).

**Critical for E0:** `PROXY_PERIOD` must be exported into the *application* srun
step (it is read by the exporter inside the traced process), **not** the proxy
step. This is exactly the distinction that made probe C misleading — `-S` is the
proxy's scrape period, `PROXY_PERIOD` is the client's push period, and they are
not the same knob.

**Note on `-i`:** `sbatch_proxy.sh` already passes it but nothing consumes the
output yet. Wiring `ExperimentInstrumentation`'s `Drop` dump into a per-node file
under the job's output directory is a prerequisite for E3 and E4.

---

# Venue and framing

Workshop targets: PDSW (SC), HPC-IODC (ISC), IPDPSW, or a malleability workshop
(e.g. co-located with Euro-Par).

- If **E0 lands** $\Rightarrow$ lead with C5 (overhead/critical path) and C1
  (semantics). Systems paper. PDSW is the natural home.
- If **E0 falsifies C5** $\Rightarrow$ lead with C1 + C2 (semantics for online
  detection on malleable jobs), demote overhead to a limitations paragraph, and
  HPC-IODC is the better fit.

Either way, **E1 is non-negotiable**: without the dewrap ablation there is no
contribution, only an integration.
