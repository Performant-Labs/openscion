The 72-hour soak of issue #239's run-length decision is ready for the
day a VM is rented for it, and runs nothing until then. A
dispatch-only `soak-72h` workflow waits for one self-hosted runner
with the `scion-soak` label (a label no other job names), gives it
4400 minutes, and runs one uninterrupted 72-hour soak (RUN_MINUTES
4320, the same ceilings and warm-up window as the nightly). It is
never a pull_request, push or scheduled run, and the fault job stays a
nightly of its own.

The job's `GITHUB_TOKEN` is dead after 24 hours, so the long job calls
nothing with it after its checkout: the samples stream to files on the
runner's disk while it runs (`metrics.csv`, `metrics-store.csv`, the
logs), and a separate short job - `needs: soak-72h`, `if: always()`, a
fresh token - uploads them. With one `scion-soak` runner, the upload
job lands on the same machine and reads the same disk.

`scripts/soak-runner-setup.md` registers the runner on a fresh Ubuntu
VM (`config.sh --ephemeral --labels scion-soak`), sizes the disk at
about 300 GB for the store (one 30-minute release run: 334 MB ->
8.95 GB beside 22,930 lands, slowing as it goes) plus system and
margin, and deletes the VM after: `--ephemeral` de-registers the
runner with its one job, so the VM is disposable.
