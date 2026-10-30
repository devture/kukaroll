<!--
SPDX-FileCopyrightText: 2026 Slavi Pantaleev

SPDX-License-Identifier: Apache-2.0
-->

# Writing hooks

A project's deploy logic lives in its own repository.
kukaroll only runs 2 commands, both set in the [executor's configuration](configuration.md#executor), both optional (`null` skips them):

| Hook | Runs | The checkout is at |
|---|---|---|
| `pre_flight` | before `git checkout -f` | the old commit |
| `deploy` | after `git checkout -f` | the new commit |

A run, in order:

1. Take the [lock](#the-lock).
2. Fetch the branch.
   If its tip is the commit deployed last, stop: nothing to do.
3. Run `pre_flight`.
   If it fails, stop.
4. `git checkout -f <tip>`.
5. Run `deploy`.
   If it fails, stop.
6. Record the tip as deployed.

Keep the commands in kukaroll's configuration short and stable, e.g. `["just", "deploy"]`, and put the steps in the repository, so they change with the code.

## What a hook gets

- **Working directory**: the checkout.
- **User**: the project's `hooks_user`, with its supplementary groups from the host's `/etc/group`.
- **Environment**: a fresh, minimal one; nothing from the container or from systemd:

  | Variable | Value |
  |---|---|
  | `HOME`, `USER`, `LOGNAME`, `SHELL` | the hooks user's, from the host's `/etc/passwd` |
  | `PATH` | `/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin` |
  | `KUKAROLL_PROJECT` | the project name |
  | `KUKAROLL_COMMIT` | the commit being deployed (full hash) |
  | `KUKAROLL_PREVIOUS_COMMIT` | the commit deployed last, or empty if none |
  | `GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_0`, `GIT_CONFIG_VALUE_0` | mark the checkout as a git [`safe.directory`](https://git-scm.com/docs/git-config#Documentation/git-config.txt-safedirectory), so `git` works even if the hooks user does not own the checkout |

  Tools outside that `PATH` (e.g. in `~/.local/bin`) need their full path.
- **Input**: none.
  stdin is `/dev/null`, so a prompt gets end-of-file and fails.
- **Output**: stdout and stderr go to the unit's journal.
  When the run fails, [Healthchecks](operations.md#healthchecks) gets the last 20,000 bytes of the run's output (kukaroll's lines, git's and the hooks'), cut at a line boundary.
  Treat both as semi-public: never print secrets (e.g. by printing configuration files, or tracing with `set -x`).

A hook fails when it exits with a non-zero status (or cannot be started).

## Pre-flight: checking before anything changes

The pre-flight hook runs while the checkout is still at the old commit, so it can refuse a deploy before anything has moved.
A typical check: does the host have the configuration the new commit needs?

Since the tree is still the old one, a plain path like `["ci/pre-flight"]` runs the **old** commit's script.
To run the new commit's version, read it from git:

```yaml
pre_flight: ["bash", "-c", "set -o pipefail; git show \"$KUKAROLL_COMMIT:ci/pre-flight\" | bash"]
```

and read the new commit's files the same way.
For example, `ci/pre-flight` could compare the keys in the new commit's `.env.dist` with the host's `.env`:

```bash
#!/usr/bin/env bash
set -euo pipefail

keys() { grep -oE '^[A-Za-z_][A-Za-z0-9_]*=' | sort -u; }

missing=$(comm -23 <(git show "$KUKAROLL_COMMIT:.env.dist" | keys) <(keys < .env))
if [ -n "$missing" ]; then
    echo "The new commit needs these settings in .env:" >&2
    echo "$missing" >&2
    exit 1
fi
```

## Deploy

The deploy hook runs with the checkout at the new commit.
For a [Docker Compose](https://docs.docker.com/compose/) project, with `deploy: ["just", "deploy"]` and a `justfile` like:

```just
deploy:
    docker compose pull
    docker compose up --detach --remove-orphans
```

`git checkout -f` discards changes to tracked files but never runs `git clean`: untracked and ignored files (`.env`, data directories, files rendered by other tools) stay.
It does not update submodules; run `git submodule update --init` in the deploy hook if you use them.

Processes a hook leaves running in the background are killed when the run ends (they belong to the run's container).
Start long-lived services through Docker or systemd, as `docker compose up` does.

## When a hook fails

kukaroll retries nothing in the deploy but the fetch: only your hooks know which of their steps are safe to repeat.

- **Pre-flight fails**: the checkout stays at the old commit and the deploy hook does not run.
- **Deploy fails**: the checkout is already at the new commit, and the new commit is not recorded as deployed.

Either way, the run fails (the unit ends up `failed`, and Healthchecks gets a failure), and the commit deployed last stays recorded.
The next trigger (a push, a restart of the receiver, or a [manual trigger](operations.md#triggering-a-deploy)) tries the branch's tip again from step 1.

After a failed deploy hook the checkout is already at the new commit, so the retry's pre-flight runs against the new tree while `KUKAROLL_PREVIOUS_COMMIT` still names the last *deployed* commit.
Don't assume the checkout matches it.

## Writing hooks that are safe to repeat

A hook can run more than once for the same commit: after a failure, or after you [force a redeploy](operations.md#redeploying-the-same-commit).
It can also be interrupted, by the unit's timeout or a host shutdown.
So:

- make each step safe to repeat (`docker compose up --detach` is; appending to a file is not);
- for database migrations, use a tool that records which migrations it has applied.

## The lock

Each run holds an exclusive [`flock`](https://man7.org/linux/man-pages/man2/flock.2.html) on the project's `lock_file` from start to end.
If the file is locked when a run starts, the run logs `waiting for the lock on <path>` and waits, up to the unit's timeout.

Other tools on the host, such as a maintenance script or an Ansible run that rewrites the project's files, can take the same lock to keep deploys out while they work on the project, e.g. with [`flock(1)`](https://man7.org/linux/man-pages/man1/flock.1.html):

```sh
flock /srv/myapp/var/deploy.lock ./maintenance.sh
```

Pushes that arrive meanwhile are not lost: the waiting run deploys the branch's tip once the lock is free.

kukaroll creates the lock file if it is missing (as root), but not its directory.
kukaroll refuses to open the lock file through a symlink.
Keep it in a directory that only root and the project's git and hooks users can write to.
Put it outside the paths that `git checkout` writes to, e.g. in an ignored directory of the checkout or elsewhere on the host.
