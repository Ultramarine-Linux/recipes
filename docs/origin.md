---
title: Recipe origin builder
description: "How recipes-origin rebuilds and serves the catalog on every push."
---

`recipes-origin` rebuilds the [catalog](./catalog-format.md) when GitHub
reports a push, and Caddy serves the result. For each push to the watched
branch it downloads the commit's tarball, runs `check` and `build` in-process (no toolchain needed at
runtime), writes the result to `$DATA_DIR/releases/<sha>/v1/`, then
atomically points the `$DATA_DIR/current` symlink at
`$DATA_DIR/releases/<sha>` (an absolute path, so Caddy must mount the volume
at the same path). Failed builds leave `current` alone; the error is logged
and shown on the status endpoint. Builds are coalesced: at most one runs and
one waits, and the waiting one is always the newest push. The newest three
releases are kept. On startup it builds the branch head, catching up on
pushes it missed, and retries every five minutes until something is live.

## Configuration

| Variable                  | Default                      |                                                                         |
| ------------------------- | ---------------------------- | ----------------------------------------------------------------------- |
| `PORT`                    | `8080`                       |                                                                         |
| `DATA_DIR`                | `/data`                      | shared with Caddy, which serves `$DATA_DIR/current`                     |
| `RECIPES_REPO`            | `Ultramarine-Linux/recipes` |                                                                         |
| `RECIPES_BRANCH`          | `main`                       |                                                                         |
| `GITHUB_WEBHOOK_SECRET`   | required                     | unless `ALLOW_UNSIGNED_WEBHOOKS=1` (local development only)             |
| `GITHUB_TOKEN`            | none                         | optional; avoids API rate limits, needed for private repositories      |
| `CLOUDFLARE_API_TOKEN`    | none                         | with `CLOUDFLARE_ZONE_ID` and `PUBLIC_URL`: purge `index.json` on deploy |
| `CLOUDFLARE_ZONE_ID`      | none                         |                                                                         |
| `PUBLIC_URL`              | none                         | e.g. `https://recipes.fyralabs.com`                                     |
| `RUST_LOG`                | `info`                       |                                                                         |

## Endpoints

Caddy proxies `/hooks/*` to the builder:

- `POST /hooks/github`: GitHub webhook. Verifies `X-Hub-Signature-256`;
  `ping` → 200, a push to `refs/heads/$RECIPES_BRANCH` → 202 and a queued
  build, anything else → 204. Bodies over 5 MiB → 413.
- `GET /hooks/status`: `{ repo, branch, current, built_at, building, pending, last_error }`.
- `GET /healthz`: `ok`.

`recipes-origin healthcheck` requests `/healthz` on `127.0.0.1:$PORT` and
exits 0 or 1, so the distroless image has a `HEALTHCHECK` without curl.

## Webhook

In the repository settings → Webhooks, set the payload URL
`https://<origin>/hooks/github`, content type `application/json`, the
`GITHUB_WEBHOOK_SECRET` as the secret, and just the `push` event.

## Image

The image is `ghcr.io/ultramarine-linux/recipes-builder`, a signed
multi-arch (amd64, arm64) image built by `.github/workflows/origin-image.yml`
through the shared FyraLabs/actions `docker.yml` workflow. Pushes to `main`
publish `:main` (what infra-joever deploys) and `:main-<timestamp>`; pull
requests only build and smoke-test. To build it locally (Docker format,
since podman's OCI images drop `HEALTHCHECK`):

```sh
podman build --format docker -f origin/Containerfile -t recipes-builder .
```

It runs as the distroless `nonroot` user (65532); `/data` in the image is
owned by it, so a fresh named volume mounted there is writable.

## Local development

To try the builder locally:

```sh
ALLOW_UNSIGNED_WEBHOOKS=1 DATA_DIR=$PWD/.origin PORT=8081 cargo run -p recipes-origin
DATA_DIR=$PWD/.origin caddy run --config origin/Caddyfile.example   # serves :8080
```
