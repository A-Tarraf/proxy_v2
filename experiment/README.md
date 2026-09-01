# Malleability Experiment

End-to-end test for the proxy TBON malleability features (graceful leave, shrink self-repair, auto-join expand).

## Quick Start

```bash
# From the repo root — single command, no manual setup needed:
bash experiment/run_malleability_test.sh
```

The script:
1. Checks that Docker containers `dmr01`–`dmr04` are running — exits early with a clear error if not.
2. Builds the proxy binary inside the cluster if it is missing (`cargo build --release` inside `dmr01`).
3. Runs all three malleability scenarios and prints `[PASS]` / `[FAIL]` for each.
4. Pulls logs back to `experiment/logs/` on the host when done (even on failure).

## Cluster Setup

The script targets the Docker cluster used for DMR development. Setup instructions are at:
https://gitlab.bsc.es/accelcom/releases/dmr/dmr/-/wikis/Docker-Sandbox

Start the cluster before running the experiment:

```bash
cd /path/to/docker-cluster-dmrv2
bash start.sh -n 4      # spins up dmr01..dmr04
```

The `start.sh` script creates two named Docker volumes (`build-dmr`, `install-dmr`) bound to the local `build/` and `install/` directories, then starts 4 containers named `dmr01`–`dmr04`. It also sets up SSH keys, MPI hostfiles, munge, and Slurm.

The cluster can be stopped with the generated shutdown script:

```bash
bash tmp/shutdown-$(hostname -s).sh
```

Shared volumes required inside each container:

| Path | Purpose |
|------|---------|
| `/opt/hpc/build` | Proxy source tree and release binary |
| `/opt/hpc/install` | Runtime data (root.url, profiles) |

## Expected Output

```
[HH:MM:SS] === Checking prerequisites ===
[HH:MM:SS] Docker containers dmr01-dmr04: OK
[HH:MM:SS] Proxy binary: OK
[HH:MM:SS] === STEP 1: Starting root proxy on dmr01 ===
[HH:MM:SS] Root proxy UP at dmr01:1337
[HH:MM:SS] === STEP 2: Starting child proxies ===
[HH:MM:SS] === STEP 3: Starting background I/O workload ===
[HH:MM:SS] === STEP 4: GRACEFUL LEAVE TEST — SIGTERM to dmr03 proxy ===
[HH:MM:SS]   [PASS] GRACEFUL LEAVE: dmr03 removed in ~2000ms (scrape timeout would be ~1000ms)
[HH:MM:SS] === STEP 5: SHRINK TEST — SIGKILL dmr04 proxy (no graceful leave) ===
[HH:MM:SS]   [PASS] SHRINK REPAIR COMPLETE: dmr04 removed in ~1200ms
[HH:MM:SS] === STEP 6: EXPAND TEST — dmr04 rejoins via --auto-root ===
[HH:MM:SS]   [PASS] AUTO-JOIN COMPLETE: dmr04 back in TBON in ~3400ms
[HH:MM:SS] === Experiment complete. Logs in /opt/hpc/build/proxy_v2/experiment/logs ===
```

Logs for each proxy instance are written to `experiment/logs/` on the host.

## What is Tested

| Step | Scenario | Mechanism | Expected |
|------|----------|-----------|---------|
| 4 | Graceful leave | SIGTERM → `/leave` endpoint → root removes node immediately | Node gone within ~2 s |
| 5 | Shrink (crash) | SIGKILL → root misses scrape → self-repair removes node | Node gone within 1 sampling period (~1 s) |
| 6 | Expand | New proxy + `--auto-root` reads `root.url` → `/join` | Node appears in TBON within ~5 s |
