#!/bin/bash
# Start the root proxy on the LOGIN NODE.
#
# Run this script ONCE on the login node before submitting your SLURM job.
# Keep it running for the entire duration of your experiments; the compute-node
# leaf proxies (started inside the job script) will relay their metrics here.
#
# Usage:
#   ssh <login_node>          # log into the login node
#   bash start_root_proxy.sh  # start the root proxy in the background
#
# Then check it is running:
#   curl http://localhost:1337/job/list   # should return JSON
#   # or open http://<login_node>:1337 in a browser
#
# To stop the proxy later:
#   pkill proxy_v2

# ── Adjust these to match your cluster ───────────────────────────────────────
PORT=1337
MAX_TRACE_MB=512         # total trace storage on the login node (MB)
DATA_DIR="$HOME/.proxyprofiles"   # where profiles and traces are stored

# ── Load the same environment your jobs use ───────────────────────────────────
module purge
source ~/loads                           # your module-load script
export PATH=$TOOLS_BIN:$PATH
export LD_LIBRARY_PATH=$TOOLS_LIB:$LD_LIBRARY_PATH

# ── Start proxy_v2 ────────────────────────────────────────────────────────────
echo "Starting root proxy on $(hostname):${PORT}  (data → ${DATA_DIR})"
proxy_v2 \
    --port ${PORT} \
    --max-trace-size ${MAX_TRACE_MB} \
    --target-prefix "${DATA_DIR}" \
    >> "${DATA_DIR}/proxy_root.log" 2>&1 &

ROOT_PID=$!
echo "Root proxy PID: ${ROOT_PID}  (log: ${DATA_DIR}/proxy_root.log)"

# Brief sanity check
sleep 3
if kill -0 "${ROOT_PID}" 2>/dev/null; then
    echo "Root proxy is running. Open http://$(hostname):${PORT}"
else
    echo "ERROR: root proxy failed to start. Check ${DATA_DIR}/proxy_root.log"
    exit 1
fi
