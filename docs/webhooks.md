<!--
SPDX-FileCopyrightText: 2026 Slavi Pantaleev

SPDX-License-Identifier: Apache-2.0
-->

# Webhooks

The receiver accepts webhooks for project `myapp` with `POST /hooks/myapp`.
It checks the delivery's signature with the project's secret, and for a push to the project's branch asks for a deploy.
It takes nothing else from the request: the executor deploys whatever the branch's tip is when it fetches.

`GET /health` answers `ok`, for monitoring the receiver itself.

## Setting up the forge

All supported forges sign the request body with [HMAC-SHA256](https://en.wikipedia.org/wiki/HMAC) using a secret you share with kukaroll.
Use the project's secret file (created in [Installing](installing.md), step 2) as the webhook's *Secret* on the forge.
Set `webhook.provider` in the [receiver's configuration](configuration.md#receiver) to match the forge:

| Forge | `provider` | Signature header | Event header |
|---|---|---|---|
| [Forgejo](https://forgejo.org/) | `forgejo` | `X-Forgejo-Signature` | `X-Forgejo-Event` |
| [Gitea](https://about.gitea.com/) | `gitea` | `X-Gitea-Signature` | `X-Gitea-Event` |
| [GitHub](https://github.com/) | `github` | `X-Hub-Signature-256` | `X-GitHub-Event` |

Forgejo also sends Gitea's headers, so `gitea` works with Forgejo too.
Other forges (e.g. GitLab, which sends a plain token instead of a signature) are not supported.

### Forgejo and Gitea

In the repository's *Settings > Webhooks*, add a *Forgejo* (or *Gitea*) webhook ([Forgejo's webhook documentation](https://forgejo.org/docs/latest/user/webhooks/), [Gitea's webhook documentation](https://docs.gitea.com/usage/webhooks)):

- **Target URL**: `https://deploy.example.com/hooks/myapp`
- **HTTP method**: `POST`
- **POST content type**: `application/json`
- **Secret**: the secret
- **Trigger on**: *Push events*
- **Branch filter**: optional; kukaroll ignores other branches anyway
- **Authorization header**: optional; see [Behind a reverse proxy](#behind-a-reverse-proxy)

The webhook's *Test delivery* sends a push event for the repository's default branch.
If that is the project's branch, it requests a deploy of the branch's current tip, which does nothing if that is already deployed.
Otherwise the receiver answers `200 ignored push to refs/heads/<default branch>`.

### GitHub

In the repository's *Settings > Webhooks > Add webhook* ([GitHub's webhook documentation](https://docs.github.com/en/webhooks/using-webhooks/creating-webhooks)):

- **Payload URL**: `https://deploy.example.com/hooks/myapp`
- **Content type**: `application/json` (not the default `application/x-www-form-urlencoded`, whose pushes the receiver rejects)
- **Secret**: the secret
- **Which events**: *Just the push event*

GitHub sends a `ping` event when the webhook is created, and the receiver answers `pong`.
To send a push again, use *Redeliver* on it in the webhook's *Recent Deliveries*.

## Responses

The forge shows these in the webhook's delivery history.
The receiver also logs 1 line per delivery to a configured project to its journal: `<project>: delivery <id>: <status> <outcome>`,
and 1 line per request for an unknown project: `"<name>": 404 unknown project` (the name quoted and cut at 64 characters).
Oversized, non-`POST`, busy (`503`) and timed-out (`408`) requests aren't logged.

| Status | Meaning |
|---|---|
| `202` `deploy requested` | a push to the project's branch |
| `200` `ignored push to <ref>` | a push to another branch, or a tag |
| `200` `ignored deletion of the branch` | the branch was deleted |
| `200` `ignored <event> event`, `pong` | another event |
| `400` | correctly signed, but not understood (e.g. not JSON, or no event header) |
| `401` `missing signature`, `malformed signature`, `wrong signature` | the secret or the provider does not match |
| `404` `unknown project` | no such project in the receiver's configuration |
| `405` | not a `POST` |
| `408` `timed out` | the request took over 30 s to arrive and be handled |
| `413` | a body over 16 MiB, rejected before any other check |
| `500` `cannot request a deploy` | the spool file could not be created (details in the journal) |
| `503` `busy` | 8 other webhook requests are being handled; redeliver it from the forge's delivery history (the receiver's next start also deploys every project) |

## Sending a test delivery by hand

```sh
body='{"ref":"refs/heads/main"}'
signature=$(printf '%s' "$body" | openssl dgst -sha256 -hmac "$(cat /etc/kukaroll/secrets/myapp)" | sed 's/^.* //')
curl -i -X POST http://127.0.0.1:8080/hooks/myapp \
    -H 'X-Forgejo-Event: push' -H "X-Forgejo-Signature: $signature" --data-binary "$body"
```

For GitHub, send `X-GitHub-Event` and `X-Hub-Signature-256: sha256=<signature>` instead.

## Behind a reverse proxy

The receiver authenticates every webhook itself.
It checks the signature, in constant time, before parsing the body.
To check it, the receiver buffers up to 16 MiB of body, so anyone who can reach it can make it do that.
It handles at most 8 webhook requests at once (others get a `503`) and gives each 30 s, so this takes at most about 128 MiB,
but a flood of such requests can delay or turn away real deliveries.

A reverse proxy in front of it can add defence in depth:

- **An `Authorization` header check.**
  Forgejo and Gitea webhooks can send a fixed `Authorization` header, e.g. `Bearer <random token>`, as a second secret per project.
  GitHub's cannot.
  A proxy that routes a project's path only when that header matches turns every other request away before it reaches kukaroll.
  Proxies compare such headers like any routing rule, not in constant time.
  So this narrows who reaches the receiver, and the signature stays the authentication.
- **A request body limit.**
  A push payload is usually a few KiB.
  A limit of about 1 MB keeps oversized requests from being buffered at all.

Neither protects requests that bypass the proxy.

A minimal [Caddy](https://caddyserver.com/) site with the body limit (see Caddy's [`request_body`](https://caddyserver.com/docs/caddyfile/directives/request_body)):

```caddy
deploy.example.com {
	request_body {
		max_size 1MB
	}
	reverse_proxy /hooks/* 127.0.0.1:8080
}
```

With [Traefik](https://traefik.io/)'s [file provider](https://doc.traefik.io/traefik/reference/install-configuration/providers/others/file/),
for a project `myapp` served at `https://deploy.example.com/hooks/myapp` by a receiver reachable as `kukaroll-receiver:8080`, with both the header check and the body limit:

```yaml
http:
  routers:
    kukaroll-myapp:
      rule: Host(`deploy.example.com`) && Path(`/hooks/myapp`) && Header(`Authorization`, `Bearer <token>`)
      middlewares: [kukaroll-buffering]
      service: kukaroll
      entryPoints: [websecure] # your HTTPS entry point
      tls: {}
  middlewares:
    kukaroll-buffering:
      buffering:
        maxRequestBodyBytes: 1048576
  services:
    kukaroll:
      loadBalancer:
        servers:
          - url: http://kukaroll-receiver:8080/
```

Use 1 router per project.
Requests for unknown projects or with a wrong header match none and get Traefik's 404.
Bodies over the limit get a 413.
Container labels work the same way.
The header value ends up in the proxy's routing configuration, so keep that private, along with any dashboard or API that shows it.
