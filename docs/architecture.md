<!--
SPDX-FileCopyrightText: 2026 Slavi Pantaleev

SPDX-License-Identifier: Apache-2.0
-->

# Architecture

kukaroll is 1 binary in 1 container image, used in 2 roles.

## Receiver (`kukaroll receive --config <file>`)

The receiver is an unprivileged, long-running HTTP server, typically behind a reverse proxy.
Each configured project gets its own webhook path.
For each delivery, the receiver checks the signature for the project's forge (Forgejo/Gitea HMAC, GitHub `X-Hub-Signature-256`) and ignores events for other branches.
For a push to the project's branch, it **creates the project's [spool file](installing.md#spool-files)**.

That is all it can do.
It passes no commit along, so a compromised receiver can at most start runs that deploy the branch's current tip.
On start it creates every project's spool file, so deploys missed while the host was down catch up.

The receiver has no Docker socket and never sees the executor's configuration.

## Executor (`kukaroll execute --config <file> <project>`)

systemd starts the executor per project.
A `.path` unit watches the project's spool file and starts a oneshot `.service`.
The service first deletes the spool file, before anything can fail, so a failing deploy cannot retrigger itself.
Then it runs the image privileged, with the host's PID namespace.

systemd provides the rest:

- 1 run at a time per project;
- triggers that arrive during a run collapse into 1 follow-up run;
- different projects run in parallel;
- a journal per project;
- a timeout for the whole run.

The executor enters the host's namespaces and stays root there.
It runs `git` as the project's **git user** and the hooks as its **hooks user**, each with its home directory, groups, SSH keys and tools, so they behave as they do for a human.
Typically the git user owns the checkout and holds the key that fetches.
The hooks user is the one allowed to run `docker compose` (often root), so the git user needs no such rights.

The run's steps are in [Writing hooks](hooks.md).
At the end of a successful deploy, the executor records the commit in the ref `refs/kukaroll/deployed` of the checkout.
git writes that ref as the git user, so nothing in the git directory changes owner.
A fresh clone has no such ref, so its first run deploys.

The executor stops reading a hook's output once the hook exits, so a background process the hook leaves behind does not hold up the run.

See also: [Operations](operations.md) for retries and Healthchecks, [Webhooks](webhooks.md#behind-a-reverse-proxy) for hardening the webhook endpoint, and [Installing](installing.md) for running it.

## Trust

The executor runs the hooks of whatever the checkout holds, as the hooks user (often root).
So whoever can push to the deployed branch can run commands as the hooks user.
So can whoever can write to the checkout or to the files the hooks read, typically the git user's account.
So can whoever the git user mistakes for the forge: pin the forge's host key (or use HTTPS) so that fetches cannot be redirected.
When the hooks user is root, treat the git user's account (often the one a web server runs as) as root-equivalent, and keep it off anything less trusted.
Better, when the hooks run as root, let a dedicated user that nothing else runs as own the checkout (`git_user`), and give the application's user only the directories it writes to (dependencies, build output, data).
Nothing root runs or reads should be writable by that user.

See [The lock](hooks.md#the-lock) for where to keep the lock file.
