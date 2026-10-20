# The 72-hour soak runner

The `soak-72h` workflow (`.github/workflows/soak-72h.yml`) runs the
soak for 72 hours on one rented VM. It is dispatch-only: nothing
schedules it, and no pull request runs it. It waits for a runner that
carries the `scion-soak` label; no other workflow names that label, so
the rented runner takes nothing else.

## Before the run

1. Rent a fresh Ubuntu 24.04 VM with **about 300 GB of free disk for
   the store** (one 30-minute release soak grew the store 334 MB ->
   8.95 GB beside 22,930 lands, about 400 KB a land and slowing, and
   the 72-hour run lands about 3.3 million times; 300 GB covers it with
   margin). 2 vCPUs and 8 GB of RAM match a hosted runner. Give root or
   sudo, and open outbound HTTPS.
2. Install the toolchain the workflow assumes (the `dtolnay/rust-toolchain`
   step does it on a hosted runner too, on the runner user's account):
   rustup stable, protoc, git, curl, `/usr/bin/time`
   (Ubuntu: `apt install time`), and `lsof`.
3. Register the runner as **one-job and ephemeral**, so the runner
   takes the dispatch and then de-registers itself, and label it
   `scion-soak`:

   ```sh
   # A registration token: the repository's Settings -> Actions ->
   # Runners -> New self-hosted runner, or the API (valid 1 h):
   curl -sS -X POST \
     -H "Authorization: Bearer $GH_TOKEN" \
     https://api.github.com/repos/bnsd55/openscion/actions/runners/registration-token \
     | jq -r .token

   ./config.sh --url https://github.com/bnsd55/openscion \
     --token "$TOKEN" \
     --ephemeral \
     --labels scion-soak \
     --unattended \
     --work /home/runner/_work
   ./svc.sh install && ./svc.sh start   # or ./run.sh in a tmux
   ```

   `--ephemeral` is what makes the VM disposable: the runner serves one
   job (here, the `soak-72h` job or its `upload` job) and removes
   itself, so no stale runner stays registered and no second job can
   land on the VM by accident.

## During

Nothing to watch but the run's own page. The samples stream to the
runner's disk as they are taken (`metrics.csv`, `metrics-store.csv`,
the logs) into `/home/runner/soak-72h/soak-artifacts/` — the run is
readable even if the VM dies at hour 71. The job's `GITHUB_TOKEN` is
dead after 24 h, which is why the upload is a separate short job with
a fresh token (`needs: soak-72h`, `if: always()`).

## After the run

1. The `upload` job puts `soak-72h-artifacts` on the run page. Save it.
2. Delete the VM (the runner de-registered itself: `--ephemeral`).
3. If a runner entry lingers anyway (the VM died mid-job): the
   repository's Settings -> Actions -> Runners, remove the entry, or
   `DELETE /repos/bnsd55/openscion/actions/runners/{id}`.
4. Delete `/home/runner/soak-72h` only if the VM survives (an ephemeral
   one does not need it).

Nothing is registered until Ben rents the VM; this file and the
workflow are only the preparation.
