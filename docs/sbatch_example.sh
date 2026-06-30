#!/bin/bash
# Example SLURM job script for running proxy_v2 with DLIO.
#
# ── Deployment topology ───────────────────────────────────────────────────────
#
#   Login node  ←─────────────────────────────────────────────────┐
#   (persistent)  proxy_v2  (root proxy, web UI at :1337)         │
#        ↑  ↑  ↑   relay (-r)                                     │
#   Compute nodes (one leaf proxy per node, started inside job)    │
#        ↑                                                         │
#   Application  (wrapped with proxy_run)                         │
#                                                                  │
# ── Step 1: start the root proxy on the login node (do this ONCE) ──────────
#
#   ssh <login_node>
#   bash docs/start_root_proxy.sh          # see that file for details
#
#   Verify it is up:
#   curl http://localhost:1337/job/list     # should return JSON
#
# ── Step 2: edit ROOT_PROXY and WORKLOAD below, then submit ──────────────────
#
#   sbatch sbatch_example.sh
#
# ── Step 3: watch results ─────────────────────────────────────────────────────
#
#   Open http://<ROOT_PROXY>:1337 in a browser.
#   Go to ftio.html → Parallel Analysis to run FTIO on collected metrics.

#SBATCH -J my_benchmark
#SBATCH -e ./%x.err
#SBATCH -o ./%x.out
#SBATCH --mail-type=NONE

# LB2 phase I nodes (96 cores, 3 CPUs/task → 1 core reserved for leaf proxy)
#SBATCH -n 64
#SBATCH -c 3
#SBATCH --mem-per-cpu=3760   # lower than default so one core is left for proxy
#SBATCH -t 00:40:00
#SBATCH -A YOUR_ACCOUNT

# ── Environment ──────────────────────────────────────────────────────────────
module purge
source ~/loads                                        # load modules
source /path/to/FTIO/.venv/bin/activate               # FTIO Python env

export PATH=$TOOLS_BIN:$PATH
export LD_LIBRARY_PATH=$TOOLS_LIB:$LD_LIBRARY_PATH
export SRUN=/opt/slurm/current/bin/srun

echo "===== JOB INFO ====="
echo "Job ID: ${SLURM_JOB_ID}"
echo "Nodes:  ${SLURM_NNODES}  (${SLURM_JOB_NODELIST})"
echo "Tasks:  ${SLURM_NTASKS}  (${SLURM_CPUS_PER_TASK} CPUs/task)"
echo "Time:   $(date)"
echo "===================="

# ── Configuration ─────────────────────────────────────────────────────────────
export WORKLOAD=my_workload        # dlio_benchmark workload name
export ROOT_PROXY=logc0002         # hostname of login node running root proxy_v2

# ── Leaf proxy: one instance per compute node ─────────────────────────────────
# -i  : inhibit local profile aggregation (root proxy does it)
# -r  : relay metrics to root proxy
# -S 100 : sample every 100 ms
# -m 128 : max trace size 128 MB per node
$SRUN \
  --nodes=${SLURM_NNODES} \
  --ntasks=${SLURM_NNODES} \
  --ntasks-per-node=1 \
  --cpus-per-task=1 \
  --overlap \
  proxy_v2 -i -r http://${ROOT_PROXY}:1337 -S 100 -m 128 &

CPUS_APP=${SLURM_CPUS_PER_TASK}

# ── Data generation (no tracing needed) ──────────────────────────────────────
$SRUN --cpus-per-task=${CPUS_APP} \
  dlio_benchmark workload=${WORKLOAD} \
    ++workload.workflow.generate_data=True \
    ++workload.workflow.train=False \
    ++workload.workflow.checkpoint=False

# ── Training run (wrapped with proxy_run for instrumentation) ─────────────────
$SRUN --cpus-per-task=${CPUS_APP} \
  proxy_run -- dlio_benchmark workload=${WORKLOAD} \
    ++workload.workflow.generate_data=False \
    ++workload.workflow.train=True \
    ++workload.workflow.checkpoint=True

EXITCODE=$?
exit $EXITCODE
