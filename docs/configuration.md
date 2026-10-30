<!--
SPDX-FileCopyrightText: 2026 Slavi Pantaleev

SPDX-License-Identifier: Apache-2.0
-->

# Configuration

kukaroll has 2 YAML files: one for the receiver and one for the executor.
They are separate so that the internet-facing receiver never sees how projects are deployed.

Rules for both:

- **Every field is required.**
  Nothing has a default.
  A feature you don't use is set to `null` explicitly (only where the tables below say `null` is allowed).
- Unknown fields are an error, so a typo does not go unnoticed.
- `kukaroll check-config receiver|executor <file>` validates a file.
  It stops at the first syntax or structure error (an unknown or missing field, a wrong type); otherwise it lists every invalid value.
  It checks the file only, not that users, paths or secret files exist.
- `kukaroll schema receiver|executor` prints the file's [JSON Schema](https://json-schema.org/), e.g. for editor completion.

**Project names** (the keys under `projects`) appear in webhook URLs, [spool file](installing.md#spool-files) names and systemd unit names.
They are 1-64 characters of `a-z`, `0-9`, `-` and `_`, starting with a letter or a digit.
A project must have the same name in both files.

**Branch names** are plain names like `main` or `release/1.x`, not `refs/heads/main`.

## Receiver

Read once, when the receiver starts.
Restart `kukaroll-receiver.service` after changing it or a secret file.

Paths are **inside the receiver's container**: the [example unit](../examples/systemd/kukaroll-receiver.service) mounts `/etc/kukaroll/secrets` at `/secrets` and `/var/lib/kukaroll/spool` at `/spool`.

```yaml
schema_version: 1
listen: 0.0.0.0:8080
spool_dir: /spool
projects:
  myapp:
    branch: main
    webhook:
      provider: forgejo
      secret_file: /secrets/myapp
```

| Field | Value |
|---|---|
| `schema_version` | `1` |
| `listen` | IP address and port to listen on, e.g. `0.0.0.0:8080` or `[::]:8080` (no host names) |
| `spool_dir` | absolute path of the [spool directory](installing.md#spool-files) inside the receiver's container; it must be writable by the receiver |
| `projects` | map of project name to project; webhooks for a project arrive at `/hooks/<project>` |
| `projects.<name>.branch` | branch whose pushes trigger a deploy; pushes to other branches and tags are ignored |
| `projects.<name>.webhook.provider` | `forgejo`, `gitea` or `github` (see [Webhooks](webhooks.md)) |
| `projects.<name>.webhook.secret_file` | absolute path of a file holding the webhook's secret; surrounding whitespace is ignored |

The receiver refuses to start if a secret file cannot be read or is empty.

## Executor

Read at the start of each run, so changes apply from the next run on, with no restart.

Paths are **host paths**: the executor works inside the host's namespaces, not inside its container.

```yaml
schema_version: 1
projects:
  myapp:
    checkout: /srv/myapp
    remote: origin
    branch: main
    git_user: myapp
    hooks_user: root
    lock_file: /srv/myapp/var/deploy.lock
    pre_flight: ["bash", "-c", "set -o pipefail; git show \"$KUKAROLL_COMMIT:ci/pre-flight\" | bash"]
    deploy: ["just", "deploy"]
    healthchecks:
      ping_url: https://hc-ping.com/00000000-0000-0000-0000-000000000000
```

| Field | Value | `null` allowed |
|---|---|---|
| `schema_version` | `1` | no |
| `projects` | map of project name to project | no |
| `projects.<name>.checkout` | absolute path of the git working copy to deploy | no |
| `projects.<name>.remote` | name of the remote to fetch from, as configured in the checkout (e.g. `origin`) | no |
| `projects.<name>.branch` | branch to deploy; must match the receiver's `branch` for this project | no |
| `projects.<name>.git_user` | host user that runs `git`; usually the checkout's owner, holding the key that fetches | no |
| `projects.<name>.hooks_user` | host user that runs the hooks; may be the git user, or e.g. `root` to run `docker compose` | no |
| `projects.<name>.lock_file` | absolute path of the file locked for the whole run; created if missing (see [The lock](hooks.md#the-lock)) | no |
| `projects.<name>.pre_flight` | command run before the checkout moves (see [Writing hooks](hooks.md)) | yes: skips the step |
| `projects.<name>.deploy` | command run after the checkout has moved | yes: skips the step |
| `projects.<name>.healthchecks` | Healthchecks reporting (below) | yes: no reporting |
| `projects.<name>.healthchecks.ping_url` | the check's full ping URL, `http://` or `https://`, without a trailing slash, query or fragment | no |

A command is a list: the program, then its arguments.
It runs without a shell, so write `["bash", "-c", "..."]` when you need one.
The program must not be empty.

With both hooks `null`, moving the checkout is the whole deploy (e.g. for a static site served straight from the checkout).

`ping_url` takes any [Healthchecks](https://healthchecks.io/) ping URL, including self-hosted instances and the `<ping key>/<slug>` form.
See [Healthchecks](operations.md#healthchecks) for what gets reported.
