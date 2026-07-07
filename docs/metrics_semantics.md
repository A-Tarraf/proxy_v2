# Metric semantics — how the numbers are produced

What the proxy's numbers actually mean, and how they travel to FTIO.
Everything here was established empirically on 2026-07-06 (HACC-IO on the DMR
Docker cluster and on bare metal).

## 1. Interception happens at call *return*

The MPI wrapper (`exporters/mpi/mpi_wrappers.w`) timestamps around each MPI
call and increments the cumulative `hits`/`size`/`time` counters **when the
call returns** (`CALL_END`). The strace exporter accounts syscalls at syscall
exit. Consequences:

- While a call is in flight, nothing is recorded — a call spanning several
  sampling periods shows zeros followed by one spike at its completion sample.
- All of a burst's bytes and duration land on a single sample, regardless of
  when the burst actually started.

## 2. The proxy averages runtime over ranks

Each rank's exporter is a separate connection; the per-node job profile
**sums** cumulative counters across the ranks on that node. `___time___<fn>`
is therefore *total seconds spent inside `<fn>` across all local ranks* — in a
1 s sampling window, Δtime can legitimately reach `n_ranks` seconds.

Any rate computed as Δsize/Δtime is consequently the **average per-rank speed
while inside the I/O calls**, not the node's wall-clock throughput. (The
formerly stored `___bandwidth___` gauge had exactly this semantics and was
removed in favour of the dewrapped reconstruction below.) This is also why
proxy rates and HACC-IO's self-reported bandwidth differ legitimately: HACC
divides by the *slowest rank's* time (MaxTime), the proxy data by the *summed*
in-call time — under rank imbalance HACC's number is systematically lower,
while burst counts and byte totals match.

## 3. Dewrapped bandwidth — creating the points in the past

`___bandwidth_dewrap___<fn>` reconstructs aggregate wall-clock bytes/s from a
size/time counter pair: each burst is spread **backwards** from its completion
sample over its estimated wall-clock span (Δtime ÷ concurrent ranks, taken
from `job_mpi_ranks`). Each point carries the rate of the interval *starting*
at its timestamp (sample-and-hold; a final 0 closes the series). The integral
over time equals the transferred bytes exactly.

It is computed in two places, by consumer:

| Consumer | Where computed | When |
|---|---|---|
| Trace UI plot | **proxy** (`dewrap_bandwidth` in `src/trace.rs`) | on the fly, only when the virtual metric is plotted |
| FTIO analysis (FTIO tab / server) | **FTIO** (`dewrap_bandwidth` in `ftio/api/metric_proxy/parse_proxy.py`) | when `--dewrap` is in the FTIO arguments — **on by default** (clear the custom-args field in the FTIO tab to disable) |

Nothing is ever stored: the trace file stays append-only, and the "past"
points exist only in the derived view. The proxy deliberately does **not**
pre-compute dewrapped series for FTIO — FTIO numerically derivates every
metric not named `deriv*`, which would mangle an already-derived rate.

## 4. Counter vs gauge aggregation (what the root shows)

- **Counters merge by summation** — across ranks on a node and across nodes at
  the root. Sizes, times, hits, and `job_mpi_ranks` all behave this way, which
  is why per-node byte totals sum to the job total on the root.
- **Gauges merge by averaging**: min/max are kept, `hits` and `total`
  accumulate, and the displayed value is `total/hits`. That is why node-wide
  gauges like `proxy_mpi_ranks` can show *decimals* on aggregating proxies —
  the root shows the average per contribution, not the sum. Use them as
  node-liveness signals, not counts.
- **`job_mpi_ranks`** (in every job trace) is the rank count done right for
  malleable jobs: counted from live rank connections (incremented when a
  connection identifies as an MPI rank of the job, decremented on disconnect),
  written with *set* semantics per node and summed across nodes at the root —
  always an integer, and it steps when a malleable job grows or shrinks.
  Caveat: on the launcher node, the DMR wrapper's own MPI helper processes
  register under the same job id and are included (they also account their own
  MPI-IO bytes there — same known launcher-side over-count).

## 5. FTIO communication

- **Transport**: ZMQ REQ/REP, msgpack. The proxy sends
  `{argv, metrics: {name → [(ts, value), …]}, disable_parallel}` — *every*
  metric of the job (including the cumulative size/time counters, the
  `deriv__*` series and `job_mpi_ranks`), so FTIO always has what it needs to
  dewrap; nothing extra must be passed.
- **Cadence**: one export per user job every ~10 s, dispatched on its own
  thread, at most one analysis in flight per proxy. Housekeeping traces
  (`main`, `Node: *`) are not analyzed.
- **Arguments**: from the proxy's `FtioArguments` (FTIO tab → `POST
  /ftio/args`). `custom_args` is split on whitespace and forwarded verbatim —
  that is how `--dewrap` reaches FTIO; the server strips it before invoking
  ftio core (`proxy_zmq.handle_request`). The proxy's default arguments
  include `--dewrap`, so dewrapped analyses run out of the box.
- **Signal preparation in FTIO**: every metric not named `deriv*` is
  numerically derivated (cumulative → rate over wall time). With `--dewrap`,
  reconstructed bandwidth signals are analyzed **in addition**, stored under
  the `___bandwidth_dewrap___<fn>` name — the same name as the proxy UI's
  virtual metric, so the FTIO overlay on a dewrapped plot finds its model.
  A per-metric FTIO run on a dewrap virtual sends the size/time/ranks series.
- **Server discovery**: the proxy looks for `admire_proxy_zmq` in `PATH`,
  spawns it, and reads the ZMQ address from its first stdout line. The port is
  ephemeral per start — in the Docker setup a wrapper script on the shared
  volume prints the host gateway address and must be refreshed after every
  FTIO server restart.
- **Fallback**: if ZMQ fails, the proxy pipes the export JSON through
  `admire_proxy_invoke_ftio` as a subprocess.

## 6. Quick reference

| Metric | Meaning | Aggregation |
|---|---|---|
| `mpi___size___<fn>` / `strace___size___<fn>` | cumulative bytes, accounted at call return | sum |
| `mpi___time___<fn>` | cumulative in-call seconds, **summed over ranks** | sum |
| `___bandwidth_dewrap___<fn>` (virtual) | aggregate wall-clock bytes/s, burst start corrected | derived on demand |
| `job_mpi_ranks` | ranks of *this job*, live, integer | set per node, sum at root |
| `proxy_mpi_ranks` / `proxy_connected_procs` | node-wide gauges (all jobs) | averaged → decimals possible |
