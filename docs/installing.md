<!--
SPDX-FileCopyrightText: 2026 Slavi Pantaleev

SPDX-License-Identifier: Apache-2.0
-->

# Installing

kukaroll runs as 2 kinds of systemd services, both from the same container image:

- the **receiver** (`kukaroll-receiver.service`): an unprivileged HTTP server for the forge's webhooks;
- the **executor** (`kukaroll-execute@<project>.service`): 1 deploy run of 1 project, started by `kukaroll-execute@<project>.path` when the receiver asks for a deploy.

[Architecture](architecture.md) explains how they fit together.

If you use the [MASH playbook](https://github.com/mother-of-all-self-hosting/mash-playbook), the [ansible-role-kukaroll](https://github.com/mother-of-all-self-hosting/ansible-role-kukaroll) Ansible role does the steps below for you.

## Spool files

The receiver asks for a deploy through a **spool file**: an empty file per project, named after the project, in the spool directory (`/var/lib/kukaroll/spool/` in the examples).
The receiver creates it to request a deploy.
The project's systemd `.path` unit starts the deploy when it appears, and the deploy's `.service` unit deletes it first thing.

## Requirements

- Linux with [systemd](https://systemd.io/) and Docker ([install Docker Engine](https://docs.docker.com/engine/install/)).
  The executor's container runs privileged, with the host's PID namespace.
  Docker's [user namespace remapping](https://docs.docker.com/engine/security/userns-remap/) is not supported.
- `git` on the host.
  The executor runs the host's `git` and the host's tools for the hooks.
  The image contains only the `kukaroll` binary.
- For each project, 2 host users, set in the [executor's configuration](configuration.md#executor):
  - the **git user** (`git_user`), which owns the checkout and runs `git`.
    It must be able to fetch from the checkout's remote without a prompt, e.g. with an SSH deploy key in its `~/.ssh` and the forge's host key in its `known_hosts`;
    check that key against the forge's published fingerprints instead of accepting it on first use, and set `StrictHostKeyChecking yes` (see [Trust](architecture.md#trust));
  - the **hooks user** (`hooks_user`), which runs the [hooks](hooks.md).
    It may be the git user, or e.g. `root` to run `docker compose`.

  Both must be listed in the host's `/etc/passwd`.
  Users from LDAP or other directory services are not found.
- A reverse proxy (or another way) to make the receiver reachable over HTTPS from the forge.

## Container image

The image is [`ghcr.io/devture/kukaroll`](https://github.com/devture/kukaroll/pkgs/container/kukaroll), for `amd64` and `arm64`.
Its tags:

| Tag | What it is |
|---|---|
| `YYYY.MM.DD.N`, e.g. `2026.10.30.0` | a release (the Git tag `v2026.10.30.0`) |
| `latest` | the newest release |
| `edge` | the newest commit on `main` that passed CI (not a release) |
| `edge-<full commit hash>` | that commit on `main` |
| `<tag>-amd64`, `<tag>-arm64` | 1 architecture of a release or `edge-<full commit hash>` tag |

There is no fixed release schedule.
Each release has a [GitHub release](https://github.com/devture/kukaroll/releases) with its changes.
Pin a release tag rather than `latest`, and upgrade on purpose (see [Upgrading](operations.md#upgrading)).

Every published tag has a [build provenance attestation](https://docs.github.com/en/actions/security-for-github-actions/using-artifact-attestations/using-artifact-attestations-to-establish-provenance-for-builds):
proof that this repository's workflow built it, and from which commit.
Check one with the [GitHub CLI](https://cli.github.com/):

```sh
gh attestation verify oci://ghcr.io/devture/kukaroll:2026.10.30.0 --owner devture
```

To build the image yourself: `just image` (or `docker build --tag kukaroll:dev .`).

## Commands

The image's entry point is the `kukaroll` binary:

- `kukaroll receive --config <file>`: runs the receiver;
- `kukaroll execute --config <file> <project>`: runs 1 deploy of a project;
- `kukaroll check-config receiver|executor <file>`: validates a configuration file;
- `kukaroll schema receiver|executor`: prints a configuration file's JSON Schema;
- `kukaroll --version`: prints the version the image was built as.

## Setting it up

The files in [`examples/`](../examples/) are a working starting point.
The steps below use their paths and a project named `myapp`, checked out in `/srv/myapp` and owned by the user `myapp`.

1. Create the checkout as the git user, and a git-ignored directory for the [lock file](hooks.md#the-lock):

   ```sh
   install -d -o myapp -g myapp /srv/myapp
   sudo -u myapp git clone git@git.example.com:example/myapp.git /srv/myapp
   sudo -u myapp mkdir /srv/myapp/var
   ```

   with `/var/` in the repository's `.gitignore`.

2. Create `/etc/kukaroll` (`mkdir -p /etc/kukaroll`) and write the configuration files (see [Configuration](configuration.md)):
   - `/etc/kukaroll/receiver.yaml`, from [`examples/receiver.yaml`](../examples/receiver.yaml);
   - `/etc/kukaroll/executor.yaml`, from [`examples/executor.yaml`](../examples/executor.yaml);
   - a webhook secret per project, e.g. `/etc/kukaroll/secrets/myapp`:

     ```sh
     install -d -m 0750 -g 65534 /etc/kukaroll/secrets
     (umask 027 && openssl rand -hex 32 > /etc/kukaroll/secrets/myapp)
     chgrp 65534 /etc/kukaroll/secrets/myapp
     ```

   Keep only your projects (remove `blog`), and set `pre_flight`, `deploy` and `healthchecks` to your project's commands or `null`.

   The receiver's paths are inside its container:
   the example unit mounts `/etc/kukaroll/secrets` at `/secrets` and `/var/lib/kukaroll/spool` at `/spool`, so `receiver.yaml` says `secret_file: /secrets/myapp` and `spool_dir: /spool`.
   The executor's paths are host paths.

   The receiver runs as UID/GID `65534` (`nobody`), so it must be able to read `receiver.yaml` and the secrets: make them readable by UID/GID `65534`, e.g. as above.
   It never needs `executor.yaml`.

3. Create the spool directory, writable by the receiver:

   ```sh
   mkdir -p /var/lib/kukaroll/spool
   chown 65534:65534 /var/lib/kukaroll/spool
   ```

4. Validate both files (this checks the files only, not that the users, paths or secrets exist):

   ```sh
   docker run --rm --user 0:0 \
       --mount type=bind,src=/etc/kukaroll,dst=/etc/kukaroll,readonly \
       ghcr.io/devture/kukaroll:2026.10.30.0 check-config executor /etc/kukaroll/executor.yaml
   ```

   and the same with `check-config receiver /etc/kukaroll/receiver.yaml`.

5. Install the units from [`examples/systemd/`](../examples/systemd/) into `/etc/systemd/system/`, replacing `ghcr.io/devture/kukaroll:latest` with the release you picked.
   Then:

   ```sh
   systemctl daemon-reload
   systemctl enable --now kukaroll-receiver.service
   systemctl enable --now kukaroll-execute@myapp.path
   ```

   Enable 1 `kukaroll-execute@<project>.path` per project.
   On start, the receiver requests a deploy of every project, so each project's first run starts right away.
   It deploys, since nothing has been deployed yet.

6. Confirm the first run.
   The output of

   ```sh
   journalctl -u kukaroll-execute@myapp.service
   ```

   has `kukaroll: deployed <commit>` as its last `kukaroll:` line.
   If it contains `kukaroll: run failed:` or another `kukaroll:` error, see [Logs](operations.md#logs).

7. Make `http://127.0.0.1:8080/hooks/myapp` reachable from the forge, e.g. as `https://deploy.example.com/hooks/myapp`, and add the webhook there.
   See [Webhooks](webhooks.md).

8. Confirm the webhook end to end:
   push to the branch (or, with Forgejo or Gitea, send a *Test delivery* if the branch is the repository's default branch), and check that the forge's delivery history shows `202 deploy requested` (see [Responses](webhooks.md#responses)).

Next: [Webhooks](webhooks.md) for the reverse proxy in front of the receiver, and [Operations](operations.md) for triggering deploys, logs, Healthchecks and upgrading.

## Adapting the units

- The receiver publishes port `8080` on `127.0.0.1` only, for a reverse proxy on the same host.
  If your proxy runs in a container, attach the receiver to the proxy's network instead (and drop `--publish`).
- `TimeoutStartSec=30min` in `kukaroll-execute@.service` bounds a whole run, including waiting for the [lock](hooks.md#the-lock).
  Raise it if your deploys take longer.
- Each path appears in several places.
  If you move one, change all of them:

  | Host path | In the container | Appears in |
  |---|---|---|
  | `/etc/kukaroll/receiver.yaml` | `/config/receiver.yaml` | the receiver unit's mount and `--config` |
  | `/etc/kukaroll/secrets` | `/secrets` | the receiver unit's mount, and each `secret_file` in `receiver.yaml` |
  | `/var/lib/kukaroll/spool` | `/spool` | the receiver unit's mount, and `spool_dir` in `receiver.yaml` |
  | `/var/lib/kukaroll/spool/%i` | (host path) | `PathExists=` in `kukaroll-execute@.path`, and `ExecStartPre=` in `kukaroll-execute@.service` |
  | `/etc/kukaroll/executor.yaml` | `/config/executor.yaml` | the executor unit's mount and `--config` |
