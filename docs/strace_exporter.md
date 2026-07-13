# The strace exporter: what it costs, and how to control it

How syscall tracing is priced, why the default filters some syscalls, exactly
which metrics that drops, and how to get them back.

All numbers below were measured on 2026-07-13 (16-core host, single process,
3 repetitions).

---

## 0. The strace exporter is ON by default

`proxy_run` activates **every detected exporter** unless you restrict it with
`-e`:

```sh
proxy_run -l
 - mpi
 - strace
 - finstrument
```

So a plain `proxy_run -- ./myprogram` **is** running syscall tracing. This is the
single most surprising thing about the tool, and it is why the cost model below
matters to everyone, not just to people who explicitly asked for `strace`.

To opt out entirely:

```sh
proxy_run -e mpi -- ./myprogram      # MPI metrics only, no ptrace, free
```

---

## 1. The cost model: you pay per traced syscall, not per sample

The strace exporter is a patched `strace`. It uses **`ptrace`**, which means the
application is **stopped twice for every traced syscall** — once on entry, once
on exit — and each stop is a context switch into the tracer and back.

Measured: **~50 µs per traced syscall.**

The consequence is the single most important thing to understand about this
exporter:

> **The cost is set by how many syscalls are traced. It is *not* set by the
> sampling period.**

Lowering `-S` (the sampling/push period) makes the proxy read the counters out
less often. It does **not** make the tracing cheaper, because the counters are
incremented *per syscall* and the interception happens whether or not anyone is
looking. There is no such thing as "sampling" a byte counter — miss a `write`
and the byte total is wrong.

This is why the two exporters have completely different cost profiles:

| Exporter | Mechanism | Cost |
|---|---|---|
| `-e mpi` | In-process function interposition (PMPI wrappers) | **Free.** No context switch, no `ptrace`. |
| `-e strace` | `ptrace` syscall interception | ~50 µs × number of traced syscalls |

If you only need MPI-level metrics, **`-e mpi` costs essentially nothing** and
you should prefer it. The strace exporter buys syscall-level detail (including
I/O that never goes through MPI) at a price proportional to how chatty the
application is.

### Worked example

A workload issuing 2,000,000 syscalls:

```
2,000,000 syscalls x 2 stops x ~25 µs  ≈  100 s of pure tracer overhead
```

Measured on exactly that workload: **1.18 s untraced → 106.96 s traced (90×).**

An application doing a few large `write()` calls (e.g. HACC-IO checkpointing
31 MB per rank) issues very few syscalls and is therefore cheap to trace. An
application doing many small reads/writes, or heavy metadata work, is expensive.
**Syscall count, not I/O volume, is what you pay for.**

---

## 2. The fix: don't trap on syscalls you don't measure

`strace` supports **seccomp-BPF filtering** (`--seccomp-bpf`). With it, the
kernel decides which syscalls are interesting, and **non-matching syscalls never
stop the tracee at all** — they run at full speed. Only matching syscalls trap
into the tracer.

Without seccomp, `-e trace=…` still stops on *every* syscall and merely declines
to *count* the uninteresting ones — which saves almost nothing:

| Invocation | Wall time |
|---|---|
| `-c` (trace everything) | 96.03 s |
| `-c -e trace=%desc` (filter, **no** seccomp) | 75.71 s |
| `-c -f --seccomp-bpf -e trace=%desc` | **4.49 s** |

The entire speedup comes from seccomp-BPF. Note that **`--seccomp-bpf` requires
`-f`** (follow-forks); without it, `strace` prints a warning and silently
disables the filter.

---

## 3. The default

`proxy_run` launches the exporter with a **denylist** of syscalls that are hot
but carry no useful metric — MPI progress engines spin on these:

```
!getpid,gettid,futex,sched_yield,clock_gettime,clock_nanosleep,nanosleep,
 rt_sigprocmask,rt_sigaction
```

This is a **denylist, not an I/O allowlist.** Everything else is still traced:
file, descriptor, network, memory, process, and signal syscalls all still produce
metrics. The proxy is not an I/O-only tool and the default does not make it one.

**Workload A** — 100k `write` + 500k `getpid`, single process:

| `-T` value | Wall time | Metrics produced |
|---|---|---|
| `all` | 23.76 s | 43 |
| *(default denylist)* | **4.35 s** | 41 |

→ **5.5× faster, 2 metrics dropped.**

**Workload B** — 100k `write` + 2M `getpid` (untraced: 1.11 s). Choosing a
*narrower* set does not buy much more — once the hot syscalls are gone, the
remaining cost is the syscalls you actually asked for:

| `-T` value | Wall time | vs untraced | Distinct syscalls kept |
|---|---|---|---|
| `all` | 108.77 s | 98× | 21 |
| *(default denylist)* | **6.35 s** | **5.7×** | **20** |
| `%desc,%file,%network,%process,%memory` | 6.20 s | 5.6× | 14 |
| `%desc,%file,%network` | 5.75 s | 5.2× | 11 |
| `%desc` (I/O only) | 6.44 s | 5.8× | 9 |

Note the shape of that table: every filtered variant lands at 5.2–5.8×. The
denylist gets essentially all of the available speedup **while keeping 20 of the
21 syscalls**, where an I/O-only allowlist would keep 9.

**You do not have to give up compute/network metrics to get the speedup.** Drop
the nine hot syscalls and you have already collected the win.

---

## 4. Exactly what the default drops

For each denylisted syscall, these counters are **not** produced:

- `strace___hits___<syscall>`
- `strace___time___<syscall>`

That is **up to 18 metrics** (9 syscalls × 2). None of the nine are I/O calls, so
no `strace___size___<syscall>` counter exists for them in the first place.

**Nothing else changes.** Verified on an identical workload, tracing everything
vs. the default:

```
LOST  strace___hits___getpid
LOST  strace___time___getpid
(nothing else)

OK   strace___hits___write     all=100000     default=100000
OK   strace___size___write     all=6400000    default=6400000
OK   strace___size___read      all=832        default=832
OK   strace___size___pread64   all=1680       default=1680
```

All byte counters are **identical**. (`___time___` counters differ in the 3rd
decimal between any two runs — they are measured wall-clock durations, so they
are never bit-identical; this is jitter, not loss.)

### The one judgement call: `futex`

`futex` is on the denylist because MPI progress engines spin on it, so it is
typically the hottest syscall in an MPI run and the largest single contributor to
tracing cost.

But `strace___time___futex` is **meaningful**: it is synchronisation/wait time,
which is exactly what you want for load-imbalance analysis. If you need it:

```sh
proxy_run -T '!getpid,gettid,sched_yield,clock_gettime,clock_nanosleep,nanosleep,rt_sigprocmask,rt_sigaction' \
          -e strace -- ./myprogram
```

Expect to give back a large part of the speedup on MPI codes. This trade-off has
not yet been measured on a real MPI application — see `docs/paper_plan.md`.

---

## 5. Controlling it

### Flag

```
-T, --strace-trace <EXPR>
```

`<EXPR>` is any `strace -e trace=` expression.

| Value | Effect |
|---|---|
| *(unset)* | The denylist above. Fast; drops the nine hot syscalls. |
| `all` | Trace every syscall. **Exactly the pre-2026-07 behaviour** — complete, but slow. |
| `%desc,%network` | Only these syscall classes. |
| `!futex,getpid` | Trace everything except these. |

Classes include `%desc` (file descriptors), `%file` (path-based), `%network`,
`%process`, `%memory`, `%signal`, `%ipc`. See `strace -h`.

### Environment variable

```sh
export METRIC_PROXY_STRACE_TRACE='all'
```

Same values as `-T`. The flag wins if both are given. The variable is useful when
you do not control the `proxy_run` command line inside a batch script.

### Examples

Note that `-e strace` is **not** needed — the strace exporter is active by default
(see §0). `-e` is only for *restricting* which exporters run.

```sh
# fast default (all exporters; syscall tracing with the denylist)
proxy_run -- ./myprogram

# previous behaviour: trace absolutely every syscall
proxy_run -T all -- ./myprogram

# keep futex (MPI wait time), drop the rest of the hot set
proxy_run -T '!getpid,gettid,sched_yield,clock_gettime,nanosleep' -- ./myprogram

# I/O and network syscalls only
proxy_run -T '%desc,%network' -- ./myprogram

# no ptrace at all: MPI metrics only, essentially free
proxy_run -e mpi -- ./myprogram
```

Measured on 100k `write` + 500k `getpid` (untraced: 0.30 s):

| Invocation | Wall time |
|---|---|
| `proxy_run -T all -- app` | 23.01 s |
| `proxy_run -- app` *(default)* | 4.59 s |
| `proxy_run -e mpi -- app` | **0.29 s** |

---

## 6. Behaviour change to be aware of: `-f` (follow-forks)

`--seccomp-bpf` **requires** `-f`, so whenever a filter is active the exporter now
follows forked children. The previous invocation (`-c` alone) did **not** — it
traced only the direct child.

- For a single-process rank (the normal HPC case) this changes nothing. Verified:
  43 metrics either way, 21.94 s vs 23.76 s.
- For an application launched through a **wrapper script**, this is a real
  difference: the old exporter would trace the wrapper and never follow into the
  actual workload. You now get the workload's metrics — and pay for them.

Practical consequence: if a previous strace measurement went through a shell
wrapper, it may have been **silently under-tracing**, and its numbers are not
comparable to current ones.

With `-T all` the exporter reverts to `-c --` with no `-f`, i.e. the exact
historical command line.

---

## 7. Summary

- **Syscall tracing is on by default.** `proxy_run -- app` traces syscalls; `-e`
  only *restricts* exporters. `-e mpi` opts out of `ptrace` entirely and is free.
- Cost is **per traced syscall** (~50 µs), not per sample. **`-S` does not help.**
- The default skips 9 hot, uninformative syscalls via seccomp-BPF: ~5× faster,
  drops up to 18 metrics, **all byte counters unaffected and identical**.
- `-T all` restores the complete, slow, historical behaviour.
- `futex` is the one denylist entry worth reconsidering if you care about MPI
  wait time — not yet measured on a real MPI code.
