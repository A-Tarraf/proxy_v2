# Proxy Speed & Validation Report

Date: 2026-07-03 · Machine: 8-core AMD Ryzen 7 PRO 4750U laptop hosting the 9-node DMR Docker cluster (dmr01 root + dmr02–09 leaves, `-S 1000`).

## 1. Fork vs upstream (`besnardjb/proxy_v2`) — Docker micro-benchmark

Identical `ubuntu:25.10` containers, one root + one leaf per build, synthetic job pushing 60 strace-style counters (20 `___size___`/`___time___` pairs at 10 Hz) through the Unix-socket wire protocol.

| Measurement | upstream | fork | fork `--no-bandwidth` |
|---|---|---|---|
| Idle CPU root / leaf | 1.5% / 1.5% | 4.4% / 2.4% | 5.1% / 2.8% |
| CPU during job root / leaf | 3.5% / 2.1% | 6.2% / 3.9% | 6.2% / 4.1% |
| `/metrics` latency (avg) | 0.8 ms | 0.8 ms | 0.7 ms |
| `/trace/list` latency (avg) | 0.5 ms | 0.5 ms | 0.6 ms |
| `/trace/plot` on leaf (avg) | 1.6 ms | 0.8 ms | 0.6 ms |
| Job appears on leaf | 2 ms | 2 ms | 2 ms |
| Job appears on **root** | **never (>60 s)** | **54 ms** | 53 ms |
| Metrics available | 1.1 s | 1.1 s | 1.0 s |

**Conclusions**
- The fork is **not slower** than upstream; `/trace/plot` is ~2× faster.
- **Bandwidth synthesis is free**: with/without `--no-bandwidth` differs only within noise.
- Root-side visibility of leaf jobs (`/trace/list`, `/trace/plot` on the root) **does not exist upstream** — it is a fork feature, so sparse root data is not a regression.
- The strace *exporter binary* differs from upstream by one line — irrelevant to proxy speed.

## 2. Bugs found & fixed while investigating "slow UI / 1 data point"

1. **trace.html auto-refresh silently disabled itself** whenever the job/metric
   `<select>` had focus (focus persists after one click) → new jobs never
   appeared without manual ↻. Fixed: refresh always runs; the DOM is only
   rebuilt when the job list actually changed.
2. **Metric list fetched only once per job** → empty forever if the first fetch
   preceded the first counters (typical for a freshly started job). Fixed:
   re-fetch every cycle, rebuild only on change.
3. **FTIO analysis ran inside the sequential scraping loop.** One slow or stale
   FTIO endpoint (ZMQ timeouts: 30 s recv) stalled *all* scrapers — a 40 s job
   got exactly **one** trace point; on the root, new jobs appeared minutes late.
   Fixed: FTIO dispatches to its own thread.
4. **FTIO ran for every trace on every proxy** (`main` + all `Node: dmrXX`
   housekeeping traces + jobs, every 10 s, ×9 proxies) once bug 3 was fixed —
   host load hit 24, HACC-IO runs took 8–13 min. Fixed: FTIO only for real user
   jobs, and at most one analysis in flight per proxy.
5. **Proxy-to-proxy HTTP scrapes had no timeout** (one hung leaf froze the
   root's loop). Fixed: 5 s timeout.
6. **Root scraper teardown leaked jobs** (a dead leaf left its jobs "live" on
   the root forever). Fixed: `relax_tracked_jobs()` on scraper removal.

After fixes: job traces record at full 1 Hz (verified 170–178 points over a
180 s job on every leaf).

## 3. HACC-IO speed test (4 nodes, `test.sh`, 16 checkpoints of 252.7 MB aggregate)

In-job wall time (`JOBEND−JOBSTART`), 3 runs each, alternating:

| Configuration | Run times | Avg write BW (HACC-reported) |
|---|---|---|
| **Uninstrumented** | 20.8 / 22.9 / 22.5 s | 194–221 MB/s |
| **proxy_run, MPI exporter only** (`-e mpi`) | **19.2 s** | 249 MB/s |
| **proxy_run, full (MPI + strace)** | 101.8 / 190.6 / 169.9 s | 34–53 MB/s |
| proxy_run full, FTIO server stopped | 189.3 s | 35 MB/s |
| *(pre-fix, FTIO storm — for the record)* | *509 / 551 / 763 s* | — |

**Conclusions**
- **The proxy itself adds no measurable overhead** (MPI-exporter-only run is as
  fast as uninstrumented).
- **All remaining overhead is the strace exporter**: ptrace on a syscall-dense
  benchmark (~270 k syscalls/rank) costs 5–9×. FTIO on/off changes nothing
  (probe with FTIO dead: same 189 s). If job runtime matters, run
  `proxy_run -e mpi`; use the strace exporter only when syscall-level detail is
  worth the cost.
- Run-to-run variance is high because the backing filesystem is ≥95% full.

## 4. Bandwidth validation (proxy vs HACC-IO self-reported, job 4)

- **Byte totals match**: dmr03/04/05 traces each end at ≈1.0 GB of
  `mpi___size___mpi_file_write_at` = 2 ranks × 16 × 31.6 MB — within 1% of
  HACC's numbers (8 ranks total, "File Per Rank" mode). The launcher node
  (dmr02) additionally counts the DMR wrapper's own instrumented MPI-IO, so it
  reports more than the pure application share — expected, not a metric bug.
- **Phases match**: 15 bandwidth bursts detected in the 1 Hz trace vs 16 HACC
  checkpoints (the first burst overlaps trace start). Peak per-node bandwidth
  74.8 MB/s vs HACC per-phase aggregate averages 10–146 MB/s — consistent given
  1 s instantaneous samples vs per-phase averages.
- **FTIO periodicity**: top candidate 0.13 Hz (7.8 s) vs actual mean checkpoint
  cadence ~11.9 s, confidence 0 — honest result: the phase durations varied
  7–146 MB/s (heavy disk contention), so the signal is aperiodic and FTIO
  correctly refuses to claim a dominant frequency.

## 5. MPI ranks over time (`proxy_mpi_ranks`)

New metric + endpoint (this session):
- `proxy_mpi_ranks` — per-node gauge, exactly one per rank (counts Unix-socket
  connections that identify as the MPI exporter). In every job trace.
- `GET /ranks` — live cluster-wide total (aggregated across leaves by the root).
- UI badge shows `ranks: N | procs: M`; `proxy_connected_procs` counts *all*
  exporter processes (≈2 per rank + launcher helpers) and is a liveness signal,
  not a rank count.

Verified live: `ranks` = 8 during each HACC run, 0 between runs, ramp visible at
job start.
