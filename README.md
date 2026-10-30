<!--
SPDX-FileCopyrightText: 2026 Slavi Pantaleev

SPDX-License-Identifier: Apache-2.0
-->

<p align="center"><img src="assets/branding/kukaroll.svg" width="128" alt="kukaroll logo"></p>

# kukaroll

kukaroll is a tool which helps you implement "git push to deploy".

You configure & run a kukaroll instance alongside your project. When you push, your forge ([Forgejo](https://forgejo.org/), [Gitea](https://about.gitea.com/) or [GitHub](https://github.com/)) notifies kukaroll via a webhook, and kukaroll moves the project's checkout to the tip of its branch and runs the project's own deploy command (if any).

Each project decides what "deploy" means for it. kukaroll only knows git.

## How it works

A project `myapp`, checked out in `/srv/myapp`, in kukaroll's [executor configuration](docs/configuration.md#executor):

```yaml
projects:
  myapp:
    checkout: /srv/myapp
    remote: origin
    branch: main
    git_user: myapp
    hooks_user: root
    lock_file: /srv/myapp/var/deploy.lock
    pre_flight: null
    deploy: ["just", "deploy"]
    healthchecks: null # or {ping_url: https://hc-ping.com/<uuid>}
```

The project's repository decides what `deploy` means, e.g. in its `justfile`:

```just
deploy:
    docker compose pull
    docker compose up --detach --remove-orphans
```

After a push to `main`, kukaroll:

1. fetches `main`, as `myapp`;
2. runs the project's `pre_flight` command (none here) while the checkout is still at the old commit; if it fails, nothing changes;
3. runs `git checkout -f <new commit>` in `/srv/myapp`;
4. runs the `deploy` command, `just deploy`, there as `root`.

Pushes that arrive during a deploy collapse into 1 follow-up deploy of the latest tip.
Each run can [report to Healthchecks](docs/operations.md#healthchecks).

## Installation

kukaroll is a container image, [`ghcr.io/devture/kukaroll`](https://github.com/devture/kukaroll/pkgs/container/kukaroll), run by systemd units.
See [Installing](docs/installing.md).

If you manage your server with the [MASH playbook](https://github.com/mother-of-all-self-hosting/mash-playbook), the [ansible-role-kukaroll](https://github.com/mother-of-all-self-hosting/ansible-role-kukaroll) Ansible role installs and configures it.

## Documentation

Start at the [documentation index](docs/README.md), or go straight to a topic:

- 📦 [Installing](docs/installing.md): the container image, the systemd units, versions
- ⚙️ [Configuration](docs/configuration.md): every field of the receiver's and the executor's configuration
- 🪝 [Writing hooks](docs/hooks.md): the pre-flight and deploy commands, their environment, failures, the lock
- 📨 [Webhooks](docs/webhooks.md): setting up Forgejo, Gitea and GitHub; the reverse proxy in front
- 🛠️ [Operations](docs/operations.md): triggering deploys, logs, Healthchecks, upgrading
- 🏛️ [Architecture](docs/architecture.md): how the receiver and the executor work, and what they trust
- 🧑‍💻 [Development](docs/development.md): building, checks and the end-to-end test

## The name

*kuka* (Bulgarian кука: a hook, also a crochet hook) + *rollout*.
The logo shows commits on a line rolling up into a spiral.

## License

[Apache-2.0](LICENSES/Apache-2.0.txt)
