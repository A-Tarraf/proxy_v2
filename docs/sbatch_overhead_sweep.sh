#!/bin/bash
# Exporter overhead sweep: what does instrumentation actually cost?
#
# Runs the same application five ways on the same nodes, alternating between
# configurations so that filesystem drift hits all of them equally:
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
#   leaf proxy : one per compute node, `-i -r http://$ROOT_PROXY:1337`, pinned
#                to a dedicated core. `-i` = relay only; the ROOT persists.
#
#   Start the root first:   bash docs/start_root_proxy.sh   (on the login node)
#
# ── Two traps this script guards against ────────────────────────────────────
#   * A leaf given `-r` whose root is unreachable calls exit(1)
#     (src/main.rs:203). With no proxy on the node, metric_proxy_init() fails,
#     every counter handle is NULL, and the application runs COMPLETELY
#     UNINSTRUMENTED at full speed. You would record a fast, wrong number and
#     conclude the exporter is cheap. Hence the pre-flight check below, and the
#     per-run "did the root actually see this job?" check.
#   * `-i` makes a proxy persist NOTHING: no traces (src/exporter.rs:928), no
#     profiles (src/exporter.rs:1011). That is correct for a leaf relaying to a
#     root, and fatal without one. So the root is mandatory in this topology.
#
#   If you cannot run a root on the login node:
#       ROOT_ON_ALLOC=1 sbatch sbatch_overhead_sweep.sh
#   -> the root goes on the first allocated node instead (it dies with the job,
#      but its -t directory is on shared scratch, so the data survives).
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

# ── Environment ─────────────────────────────────────────────────────────────
module purge
source ~/loads                                  # your module/env setup

export PATH="$TOOLS_BIN:$PATH"                  # must contain proxy_v2, proxy_run
export LD_LIBRARY_PATH="$TOOLS_LIB:$LD_LIBRARY_PATH"
export SRUN=/opt/slurm/current/bin/srun

# ── Configuration ───────────────────────────────────────────────────────────
ROOT_PROXY=${ROOT_PROXY:-LOGIN_NODE}      # hostname of the login node root proxy
ROOT_ON_ALLOC=${ROOT_ON_ALLOC:-0}         # 1 = run the root on the first alloc node

APP=${APP:-./YOUR_APPLICATION}            # the binary under test
APP_ARGS=${APP_ARGS:-}                    # extra args (before the output path)
PARTICLES=${PARTICLES:-1000000}           # HACC-IO: number of particles

REPS=${REPS:-3}
PROXY_S=${PROXY_S:-1000}                  # proxy scrape period (ms)
PUSH_PERIOD=${PUSH_PERIOD:-1000}          # client push period (ms) -> proxy_run -S

SCRATCH=${SCRATCH:-/path/to/scratch/$USER}
DATADIR=$SCRATCH/overhead_sweep_${SLURM_JOB_ID}
RESULTS=./overhead_sweep_${SLURM_JOB_ID}.csv
mkdir -p "$DATADIR"

CPUS_PROXY=1
CPUS_APP=${SLURM_CPUS_PER_TASK:-3}

if [ "$ROOT_ON_ALLOC" = "1" ]; then
    ROOT_PROXY=$(scontrol show hostnames "$SLURM_JOB_NODELIST" | head -1)
fi

echo "===== JOB ====="
echo "JobID     : $SLURM_JOB_ID"
echo "Nodes     : $SLURM_NNODES   Ranks: $SLURM_NTASKS"
echo "Nodelist  : $SLURM_JOB_NODELIST"
echo "Root proxy: $ROOT_PROXY (on_alloc=$ROOT_ON_ALLOC)"
echo "Results   : $RESULTS"
echo "==============="
echo

# ── Optional: root inside the allocation ────────────────────────────────────
ROOT_SRUN=""
if [ "$ROOT_ON_ALLOC" = "1" ]; then
    echo ">>> starting ROOT proxy on $ROOT_PROXY (inside allocation)"
    $SRUN --nodes=1 --ntasks=1 --nodelist="$ROOT_PROXY" \
          --cpus-per-task=${CPUS_PROXY} --overlap \
          proxy_v2 -t "$DATADIR/proxy_root" -S "$PROXY_S" -m 128 &
    ROOT_SRUN=$!
    sleep 15
fi

# ── PRE-FLIGHT: the root MUST be up, or every instrumented run is fiction ────
if ! curl -s -m 5 -o /dev/null "http://${ROOT_PROXY}:1337/joblist"; then
    echo "FATAL: no root proxy answering at ${ROOT_PROXY}:1337"
    echo
    echo "  Leaves are launched with '-r'. If the root is unreachable they"
    echo "  exit(1), leaving no proxy on the node. The exporters then connect to"
    echo "  nothing, all counters are NULL, and the application runs"
    echo "  UNINSTRUMENTED at full speed -- a fast, wrong, convincing number."
    echo
    echo "  Start the root on the login node first:"
    echo "      ssh ${ROOT_PROXY} && bash docs/start_root_proxy.sh"
    echo "  ...or re-submit with:  ROOT_ON_ALLOC=1 sbatch \$0"
    exit 1
fi
echo ">>> root proxy reachable at ${ROOT_PROXY}:1337"

# ── Leaf proxies: one per node, relaying to the root ─────────────────────────
echo ">>> starting leaf proxies (one per node, -i relay mode)"
$SRUN --nodes="${SLURM_NNODES}" --ntasks="${SLURM_NNODES}" --ntasks-per-node=1 \
      --cpus-per-task=${CPUS_PROXY} --overlap \
      proxy_v2 -i -r "http://${ROOT_PROXY}:1337" -S "${PROXY_S}" -m 128 &
LEAF_SRUN=$!
sleep 25

LEAVES=$(curl -s -m 5 "http://${ROOT_PROXY}:1337/join/list" | grep -o 'http' | wc -l)
echo ">>> leaves registered with root: ${LEAVES} (expected ~${SLURM_NNODES})"
if [ "${LEAVES:-0}" -eq 0 ]; then
    echo "FATAL: no leaf proxy registered. The leaves died; nothing would be"
    echo "       instrumented. Aborting rather than producing fiction."
    kill $LEAF_SRUN $ROOT_SRUN 2>/dev/null
    exit 1
fi
echo

# ── Job IDs ─────────────────────────────────────────────────────────────────
# proxy_run resolves the job id as
#     PROXY_JOB_ID (from `proxy_run -j`) -> SLURM_JOBID -> PMIX_ID -> PPID
# and then APPENDS "-$SLURM_STEP_ID" (src/proxywireprotocol.rs:542).
# Every srun (including the proxy launches above) bumps the step counter, so
# `-j` behaves as a PREFIX: `-j mpi_r1` lands in the proxy as `mpi_r1-<step>`.
# Each run therefore gets its own trace; we grep the root's joblist for the
# prefix rather than trying to predict the suffix.

echo "config,rep,tag,wall_s,instrumented,app_out" > "$RESULTS"

run_cfg() {
    local cfg="$1" rep="$2"; shift 2       # remaining args: proxy_run flags
    local tag="${cfg}_r${rep}"
    local out="$DATADIR/${tag}.out"
    local t0 t1 wall seen="n/a"

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

    # Did the root actually SEE this job? If not, the run was uninstrumented and
    # its wall time is meaningless -- flag it rather than averaging it in. This
    # matters because an uninstrumented run is FAST and looks like a win.
    if [ "$cfg" != "plain" ]; then
        if curl -s -m 5 "http://${ROOT_PROXY}:1337/joblist" | grep -q "$tag"; then
            seen="yes"
        else
            seen="NO-UNINSTRUMENTED"
            echo "    !! WARNING: root never saw job '$tag' -- run was NOT instrumented"
        fi
    fi

    printf '%-16s rep %s   %8.2f s   instrumented=%s\n' "$cfg" "$rep" "$wall" "$seen"
    echo "$cfg,$rep,$tag,$wall,$seen,$out" >> "$RESULTS"

    rm -rf "${DATADIR}/data_${tag}"        # keep the filesystem from filling
    sleep 5
}

# futex kept, the rest of the hot set dropped. THE open question: futex is
# usually the hottest syscall in an MPI run (progress engines spin on it), but
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
awk -F, 'NR>1 && $5 != "NO-UNINSTRUMENTED" { s[$1]+=$4; n[$1]++ }
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
wait 2>/dev/null
exit 0
