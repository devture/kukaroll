<!--
SPDX-FileCopyrightText: 2026 Slavi Pantaleev

SPDX-License-Identifier: Apache-2.0
-->

# Operations

The examples use the units and paths from [`examples/`](../examples/) and a project named `myapp`.

## Triggering a deploy

A deploy is requested by creating the project's [spool file](installing.md#spool-files).
Any of these does it:

- a push to the project's branch (the receiver creates the file);
- a restart of the receiver, which requests a deploy of **every** project, so pushes missed while the host or the receiver was down are caught up;
- by hand: `systemctl start kukaroll-execute@myapp.service`.
  This also runs while the `.path` unit is stopped.

A run deploys the branch's tip as it is when the run fetches.
If that is the commit deployed last, the run stops there, so extra triggers are cheap.
Triggers that arrive during a run collapse into 1 follow-up run; runs of different projects happen in parallel.

## Redeploying the same commit

kukaroll records the commit deployed last in the ref `refs/kukaroll/deployed` of the checkout.
To run the hooks again for the current tip, delete it (as the git user) and trigger a deploy:

```sh
sudo -u myapp git -C /srv/myapp update-ref -d refs/kukaroll/deployed
systemctl start kukaroll-execute@myapp.service
```

## Pausing deploys

- `systemctl stop kukaroll-execute@myapp.path` stops starting runs.
  Triggers meanwhile leave the spool file in place, so starting the `.path` unit again runs 1 deploy.
- Or hold the project's [lock](hooks.md#the-lock): runs wait for it (up to the unit's timeout).

## Logs

```sh
journalctl -u kukaroll-receiver.service          # 1 line per delivery
journalctl -u kukaroll-execute@myapp.service     # each run: kukaroll's steps and the hooks' output
systemctl status kukaroll-execute@myapp.service  # whether the last run failed
```

The receiver doesn't log oversized, non-`POST`, busy (`503`) or timed-out (`408`) requests (see [Responses](webhooks.md#responses)).

A run's own lines start with `kukaroll:`, e.g. `kukaroll: deploying <commit> (previously deployed: <commit>)`.
A run ends with one of:

- `kukaroll: deployed <commit>`;
- `kukaroll: <commit> was already deployed`;
- `kukaroll: run failed: <reason>`.

A failure before the run starts (an invalid configuration, an unknown project, or failing to enter the host's namespaces) logs `kukaroll: <reason>` instead.

## Healthchecks

With `healthchecks` set for a project (see [Configuration](configuration.md#executor)), each run reports to [Healthchecks](https://healthchecks.io/) (see [Healthchecks' Pinging API](https://healthchecks.io/docs/http_api/)):

- `<ping_url>/start` when the run starts;
- `<ping_url>` when it succeeds, with `deployed <commit>` or `<commit> was already deployed`;
- `<ping_url>/fail` when it fails, with the last 20,000 bytes of the run's output (kukaroll's lines, git's and the hooks'), cut at a line boundary.

Every run pings, including the ones that find nothing new.
Pushes are irregular, so set the check's **period** to the longest you expect between pushes or receiver restarts, and its **grace time** above your longest deploy:
a `/start` without a result within the grace time also marks the check down.

Each ping is tried 3 times.
A ping that still fails is logged and does not fail the run.

Some failures happen before the executor can reach the network, and are **not** reported:
an executor configuration that does not load (or lacks the project), a missing image, a container that cannot start or cannot enter the host's namespaces (e.g. it is not privileged).
They show only in the unit's journal and its `failed` state.
Validate the configuration with `kukaroll check-config executor` whenever you change it.

## Fetch failures

A failed `git fetch` is retried 2 times, after 15 and 60 seconds, so a forge that is briefly unreachable doesn't fail the run.
An attempt that takes over 5 minutes is killed and counts as failed.
Nothing else is retried; see [When a hook fails](hooks.md#when-a-hook-fails).

## Adding or removing a project

To add one:

1. Add it to both [configuration files](configuration.md), and create its secret file.
2. Restart the receiver: `systemctl restart kukaroll-receiver.service`.
   It reads its configuration only on start.
   The executor's configuration is read by each run, so it needs no restart.
3. `systemctl enable --now kukaroll-execute@myapp.path`.
4. Add the [webhook](webhooks.md) on the forge.

To remove one, do the reverse: delete the webhook, `systemctl disable --now kukaroll-execute@myapp.path`,
remove it from both files, and restart the receiver.

## Upgrading

Check the [GitHub releases](https://github.com/devture/kukaroll/releases) for changes, then change the image tag in the units (see [Container image](installing.md#container-image)):

```sh
docker pull ghcr.io/devture/kukaroll:2026.10.30.0
systemctl daemon-reload
systemctl restart kukaroll-receiver.service
```

The executor units pick up the new image with their next run.
If you use the `latest` tag instead, `docker run` keeps using the image it already has: run `docker pull ghcr.io/devture/kukaroll:latest` to upgrade, then restart the receiver.
