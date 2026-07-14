#!/bin/bash
# Exporter overhead sweep: what does instrumentation actually cost?
#
# Runs the same application five ways on the same nodes, alternating between
# configurations so filesystem drift hits all of them equally:
#
#   plain           uninstrumented baseline (no proxy_run)
#   mpi             -e mpi          PMPI wrappers only, no ptrace
#   strace_all      -T all          trace every syscall  (pre-2026-07 default)
#   strace_default  (no flag)       seccomp-BPF denylist (current default)
#   strace_futex    -T '!getpid,…'  denylist but KEEP futex
#
# See docs/cluster_benchmark.md for how to read the results, and
# docs/strace_exporter.md for the cost model.
#
# ── Topology ────────────────────────────────────────────────────────────────
#   root proxy : on the LOGIN node, started BEFORE submitting (persistent,
#                outlives the allocation, steals no compute cores).
#   leaf proxy : one per compute node, `-i -r http://$ROOT_PROXY:1337`, on a
#                dedicated core. `-i` = relay only; the ROOT persists.
#
#   Start the root first:   bash docs/start_root_proxy.sh   (on the login node)
#   Or use ROOT_ON_ALLOC=1 to put the root on the first allocated node.
#
# ── How this script knows a run was really instrumented ─────────────────────
# If no proxy is reachable on a node, metric_proxy_init() fails, every counter
# handle is NULL, and the application runs UNINSTRUMENTED at full speed. That
# produces a FAST number, so the failure looks like a win. Three defences:
#
#   1. Stray proxies from earlier jobs are killed on every node first. A leftover
#      proxy holds :1337, so a new leaf cannot start there.
#   2. Every node is polled for a live proxy before the sweep starts.
#   3. After every run, TWO independent checks (both must pass):
#        LOCAL  - the app's output contains no "Not Connected to Metric Proxy";
#                 i.e. the exporter reached the proxy ON ITS OWN NODE.
#        REMOTE - the ROOT's /metrics counter sum for that exporter INCREASED;
#                 i.e. the data actually travelled leaf -> root. This catches a
#                 leaf that is alive but not relaying, which LOCAL cannot see.
#      Runs failing either check are marked NO-UNINSTRUMENTED and EXCLUDED from
#      the averages.
#
#   Do NOT use /job/list for this: it only lists LIVE jobs, so it is always empty
#   by the time a run has finished. (An earlier version of this script did, and
#   flagged every single run as uninstrumented.)
#
# ── Usage ───────────────────────────────────────────────────────────────────
#   sbatch sbatch_overhead_sweep.sh
#   REPS=5 PARTICLES=5000000 sbatch sbatch_overhead_sweep.sh
#
#SBATCH -J proxy_overhead_sweep
#SBATCH --mail-type=NONE
#SBATCH -e ./%x.err
#SBATCH -o ./%x.out
#SBATCH -C YOUR_CONSTRAINT     # e.g. node feature/partition selector
#SBATCH -n 64                  # MPI ranks
#SBATCH -c 3                   # CPUs per task
#SBATCH --mem-per-cpu=3760     # under the node default so a core stays free
#SBATCH -t 01:30:00
#SBATCH -A YOUR_ACCOUNT

set -u

module purge
source ~/loads                                  # your module/env setup

export PATH="$TOOLS_BIN:$PATH"                  # must contain proxy_v2, proxy_run
export LD_LIBRARY_PATH="$TOOLS_LIB:$LD_LIBRARY_PATH"
export SRUN=/opt/slurm/current/bin/srun

# ── Configuration ───────────────────────────────────────────────────────────
ROOT_PROXY=${ROOT_PROXY:-LOGIN_NODE}      # login-node root (start it beforehand)
ROOT_ON_ALLOC=${ROOT_ON_ALLOC:-0}         # 1 = root on the first allocated node

APP=${APP:-./YOUR_APPLICATION}            # binary under test
APP_ARGS=${APP_ARGS:-}                    # extra args before the output path
PARTICLES=${PARTICLES:-1000000}

REPS=${REPS:-3}
PROXY_S=${PROXY_S:-1000}                  # proxy scrape period (ms)
PUSH_PERIOD=${PUSH_PERIOD:-1000}          # client push period -> proxy_run -S

SCRATCH=${SCRATCH:-/path/to/scratch/$USER}
DATADIR=$SCRATCH/overhead_sweep_${SLURM_JOB_ID}
RESULTS=./overhead_sweep_${SLURM_JOB_ID}.csv
mkdir -p "$DATADIR"

CPUS_PROXY=1
CPUS_APP=${SLURM_CPUS_PER_TASK:-3}
NODES=$(scontrol show hostnames "$SLURM_JOB_NODELIST")

if [ "$ROOT_ON_ALLOC" = "1" ]; then
    ROOT_PROXY=$(echo "$NODES" | head -1)
fi

echo "===== JOB ====="
echo "JobID     : $SLURM_JOB_ID"
echo "Nodes     : $SLURM_NNODES   Ranks: $SLURM_NTASKS"
echo "Root proxy: $ROOT_PROXY (on_alloc=$ROOT_ON_ALLOC)"
echo "Results   : $RESULTS"
echo "==============="
echo

# ── 1. Kill stray proxies from earlier jobs ─────────────────────────────────
# A leftover proxy holds :1337, so a new leaf cannot bind and will refuse to
# start -- leaving that node's ranks with no proxy, hence uninstrumented.
echo ">>> clearing stray proxies on all nodes"
$SRUN --nodes="${SLURM_NNODES}" --ntasks="${SLURM_NNODES}" --ntasks-per-node=1 \
      --overlap bash -c 'pkill -x proxy_v2 2>/dev/null; true' || true
sleep 3

# ── 2. Root proxy ───────────────────────────────────────────────────────────
ROOT_SRUN=""
if [ "$ROOT_ON_ALLOC" = "1" ]; then
    echo ">>> starting ROOT proxy on $ROOT_PROXY (inside allocation)"
    # -b large => flat tree: every leaf attaches directly to the root, so
    #             /join/list is a meaningful count (with the default -b 2 the
    #             root has only 2 direct children and the rest hang deeper).
    # --no-ftio => no FTIO server probe, no periodic "FTIO client address not
    #             set" errors, no analysis CPU. We only want metric collection.
    $SRUN --nodes=1 --ntasks=1 --nodelist="$ROOT_PROXY" \
          --cpus-per-task=${CPUS_PROXY} --overlap \
          proxy_v2 -t "$DATADIR/proxy_root" -S "$PROXY_S" -m 128 \
                   -b "$((SLURM_NNODES + 2))" --no-ftio &
    ROOT_SRUN=$!
    sleep 15
fi

if ! curl -sf -m 5 -o /dev/null "http://${ROOT_PROXY}:1337/job/list"; then
    echo "FATAL: no root proxy answering at ${ROOT_PROXY}:1337"
    echo "  Start it on the login node:  bash docs/start_root_proxy.sh"
    echo "  ...or re-submit with:        ROOT_ON_ALLOC=1 sbatch \$0"
    exit 1
fi
echo ">>> root proxy reachable at ${ROOT_PROXY}:1337"

# ── 3. Leaf proxies, one per node ───────────────────────────────────────────
# On the root's own node (ROOT_ON_ALLOC) the leaf will find :1337 taken and exit
# cleanly; that node's ranks use the root's UNIX socket. That is fine and safe.
echo ">>> starting leaf proxies (one per node, -i relay mode)"
$SRUN --nodes="${SLURM_NNODES}" --ntasks="${SLURM_NNODES}" --ntasks-per-node=1 \
      --cpus-per-task=${CPUS_PROXY} --overlap \
      proxy_v2 -i -r "http://${ROOT_PROXY}:1337" -S "${PROXY_S}" -m 128 --no-ftio &
LEAF_SRUN=$!
sleep 25

# ── 4. EVERY node must have a live proxy, or its ranks run uninstrumented ────
echo ">>> checking every node has a live proxy"
MISSING=""
for n in $NODES; do
    curl -sf -m 3 -o /dev/null "http://${n}:1337/job/list" || MISSING="$MISSING $n"
done
if [ -n "$MISSING" ]; then
    echo "FATAL: no proxy answering on:$MISSING"
    echo "  Ranks on those nodes would run UNINSTRUMENTED at full speed and the"
    echo "  timings would be meaningless. Check the .err file for AddrInUse."
    kill $LEAF_SRUN $ROOT_SRUN 2>/dev/null
    exit 1
fi
echo ">>> all ${SLURM_NNODES} nodes have a live proxy"
echo

# ── Job IDs ─────────────────────────────────────────────────────────────────
# proxy_run resolves the job id as
#     PROXY_JOB_ID (from `proxy_run -j`) -> SLURM_JOBID -> PMIX_ID -> PPID
# and then APPENDS "-$SLURM_STEP_ID" (src/proxywireprotocol.rs). Every srun bumps
# the step counter, so `-j` behaves as a PREFIX: `-j mpi_r1` lands as
# `mpi_r1-<step>`. Each run therefore gets its own trace.

echo "config,rep,tag,wall_s,local_ok,remote_ok,instrumented,app_out" > "$RESULTS"

# Sum of counter VALUES at the ROOT for a metric prefix.
# /metrics format is:  <name> <timestamp> <value>   -> $NF is the value.
metric_sum() {
    curl -sf -m 10 "http://${ROOT_PROXY}:1337/metrics" 2>/dev/null \
      | grep "^$1" | awk '{s+=$NF} END{printf "%.0f", s+0}'
}

run_cfg() {
    local cfg="$1" rep="$2"; shift 2       # remaining args: proxy_run flags
    local tag="${cfg}_r${rep}"
    local out="$DATADIR/${tag}.out"
    local t0 t1 wall prefix before after
    local local_ok="n/a" remote_ok="n/a" seen="n/a"

    # which exporter's counters must appear at the root for this config
    case "$cfg" in
        mpi)      prefix="mpi___"    ;;
        strace_*) prefix="strace___" ;;
        *)        prefix=""          ;;
    esac
    [ -n "$prefix" ] && before=$(metric_sum "$prefix")

    rm -rf "${DATADIR}/data_${tag}"; mkdir -p "${DATADIR}/data_${tag}"

    t0=$(date +%s.%N)
    if [ "$cfg" = "plain" ]; then
        $SRUN --cpus-per-task="${CPUS_APP}" \
              "$APP" $APP_ARGS "$PARTICLES" "${DATADIR}/data_${tag}/out" > "$out" 2>&1
    else
        $SRUN --cpus-per-task="${CPUS_APP}" \
              proxy_run -j "$tag" -S "$PUSH_PERIOD" "$@" -- \
              "$APP" $APP_ARGS "$PARTICLES" "${DATADIR}/data_${tag}/out" > "$out" 2>&1
    fi
    t1=$(date +%s.%N)
    wall=$(echo "$t1 - $t0" | bc)

    if [ -n "$prefix" ]; then
        # LOCAL: did the exporter reach the proxy ON ITS OWN NODE? The client
        # logs this line when it cannot. An uninstrumented run is FAST, so
        # without this check the failure looks like a win.
        # (Do NOT use /job/list: it lists only LIVE jobs, so it is always empty
        # once a run has finished. An earlier version did, and flagged every
        # single run as uninstrumented.)
        if grep -q "Not Connected to Metric Proxy" "$out" 2>/dev/null; then
            local_ok="NO"; else local_ok="yes"
        fi

        # REMOTE: did the data actually travel leaf -> root? Catches a leaf that
        # is alive but not relaying, which the LOCAL check cannot see.
        sleep 8                                  # let the root scrape the leaves
        after=$(metric_sum "$prefix")
        if [ "${after:-0}" -gt "${before:-0}" ]; then remote_ok="yes"; else remote_ok="NO"; fi

        if [ "$local_ok" = "yes" ] && [ "$remote_ok" = "yes" ]; then
            seen="yes"
        else
            seen="NO-UNINSTRUMENTED"
            echo "    !! WARNING: '$tag' NOT properly instrumented" \
                 "(local=$local_ok remote=$remote_ok, root ${prefix} sum ${before:-0} -> ${after:-0})"
        fi
    fi

    printf '%-16s rep %s   %8.2f s   local=%-3s remote=%-3s -> %s\n' \
        "$cfg" "$rep" "$wall" "$local_ok" "$remote_ok" "$seen"
    echo "$cfg,$rep,$tag,$wall,$local_ok,$remote_ok,$seen,$out" >> "$RESULTS"

    rm -rf "${DATADIR}/data_${tag}"        # keep the filesystem from filling
    sleep 5
}

# futex kept, rest of the hot set dropped. THE open question: futex is usually
# the hottest syscall in an MPI run (progress engines spin on it), but
# strace___time___futex is the synchronisation/wait metric we may actually want.
KEEP_FUTEX='!getpid,gettid,sched_yield,clock_gettime,clock_nanosleep,nanosleep,rt_sigprocmask,rt_sigaction'

for rep in $(seq 1 "$REPS"); do
    echo "================ rep $rep / $REPS ================"
    run_cfg plain          "$rep"
    run_cfg mpi            "$rep"  -e mpi
    run_cfg strace_all     "$rep"  -T all
    run_cfg strace_default "$rep"
    run_cfg strace_futex   "$rep"  -T "$KEEP_FUTEX"
    echo
done

# ── Summary ─────────────────────────────────────────────────────────────────
echo
echo "===================== SUMMARY ====================="
awk -F, 'NR>1 && $7 != "NO-UNINSTRUMENTED" { s[$1]+=$4; n[$1]++ }
     END {
       printf "%-16s %10s %10s\n", "config", "mean(s)", "vs plain";
       base = (n["plain"]>0) ? s["plain"]/n["plain"] : 0;
       split("plain mpi strace_all strace_default strace_futex", o, " ");
       for (i=1; i<=5; i++) {
         c=o[i];
         if (n[c]>0) {
           m=s[c]/n[c];
           if (base>0) printf "%-16s %10.2f %9.1fx\n", c, m, m/base;
           else        printf "%-16s %10.2f %10s\n", c, m, "-";
         }
       }
     }' "$RESULTS"
echo "==================================================="
grep -q "NO-UNINSTRUMENTED" "$RESULTS" && \
    echo "WARNING: some runs were NOT instrumented and were excluded. Check $RESULTS."
echo
echo "How to read this:"
echo "  mpi ~= plain                   -> PMPI interposition is free (expected)"
echo "  strace_all >> plain            -> the ptrace tax; this WAS the old default"
echo "  strace_default << strace_all   -> seccomp-BPF filtering works on real MPI"
echo "  strace_futex vs strace_default -> THE OPEN QUESTION:"
echo "     close to strace_default -> futex is cheap here; take it OFF the"
echo "                                denylist and get time___futex for free"
echo "     close to strace_all     -> futex is the hot path; the denylist is right"
echo
echo "Traces/profiles live on the ROOT proxy (${ROOT_PROXY}); leaves run with -i"
echo "and persist nothing by design."
echo "Raw results: $RESULTS"

kill $LEAF_SRUN $ROOT_SRUN 2>/dev/null
$SRUN --nodes="${SLURM_NNODES}" --ntasks="${SLURM_NNODES}" --ntasks-per-node=1 \
      --overlap bash -c 'pkill -x proxy_v2 2>/dev/null; true' || true
wait 2>/dev/null
exit 0
