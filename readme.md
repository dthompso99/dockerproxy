# dockerproxy

[![Docker Image](https://github.com/dthompso99/dockerproxy/actions/workflows/docker-image.yml/badge.svg)](https://github.com/dthompso99/dockerproxy/actions/workflows/docker-image.yml)

A lightweight, multi-host caching Docker registry proxy.

I needed a simple local Docker cache to prevent image-pull-backoff errors when I restarted my cluster, and overall speed up the cluster on a full reboot. Additionally, if the cache is fully loaded, it would be great to be able to boot with no internet. There are other solutions out there, far more intricate than this, but they did not check the boxes for me:

- dead simple: I intended to run this as a Home Assistant add-on, which is an odd pick, yes, but it is outside of my cluster
- handle multiple repositories: the official Docker registry mirror wants a separate instance for each upstream registry
- cache management: storage is not free, and images get old, so clear out unused data after a period

This is not trying to be a full registry. It is more like a small, stubborn shelf in front of the registries I use. If Docker asks for something and it is already on the shelf, dockerproxy hands it back. If it is not there, dockerproxy fetches it from the real registry, stores a copy, and returns it to Docker.

## Status

This is still young software, but the core loop works:

- Docker Hub mirror requests
- explicit `localhost:8080/<registry>/<repo>:<tag>` style pulls
- Docker Hub, Quay, GCR, and private-ish registries configured by host
- bearer-token auth flows, including optional configured username/token
- local manifest/blob cache
- TTL-based cache cleanup
- tiny built-in cache UI
- tiny `FROM scratch` container image

## Running It

Local development:

```bash
cargo run -- --config-file ./proxy.yaml --cache-dir ./cache --log-level 1
```

Container:

```bash
docker build -t dockerproxy:local .

docker run --rm -p 8080:8080 \
  -v ./proxy.yaml:/proxy.yaml:ro \
  -v ./cache:/cache \
  dockerproxy:local \
  --config-file /proxy.yaml \
  --cache-dir /cache \
  --log-level 1
```

Or with Compose:

```bash
cp proxy.example.yaml proxy.yaml
docker compose -f docker-compose.example.yaml up -d
```

If your Docker install still uses the older standalone Compose binary, use `docker-compose` in place of `docker compose`.

The container image is built as a static musl binary and copied into `FROM scratch`. The only extra runtime file is the CA certificate bundle so outbound HTTPS registry calls work.

## Docker Configuration

For Docker Hub, the cleanest local setup is a registry mirror:

```json
{
  "registry-mirrors": ["http://localhost:8080"],
  "insecure-registries": ["localhost:8080"]
}
```

That makes ordinary Docker Hub pulls flow through dockerproxy:

```bash
docker pull nginx:latest
docker pull jc21/nginx-proxy-manager:latest
```

For non-Docker Hub registries, Docker does not treat the mirror as a universal proxy. Use the local registry prefix:

```bash
docker pull localhost:8080/gcr.io/google-samples/hello-app:1.0
docker pull localhost:8080/quay.io/redhat/ubi8-minimal:latest
```

For Kubernetes, that means non-Docker Hub images may still need to be referenced through the local prefix unless your runtime or registry configuration gives you another rewrite path.

One local-dev quirk: Docker may try HTTPS first against `localhost:8080` for some explicit local pulls, even with `insecure-registries` configured. That can create a short delay before it falls back to HTTP. In a real deployment, putting dockerproxy behind a normal TLS reverse proxy is the cleaner answer.

## Configuration

`proxy.yaml` tells dockerproxy which upstream registries it knows about:

```yaml
hosts:
  - name: dockerhub
    url: https://registry-1.docker.io

  - name: google
    url: https://gcr.io

  - name: quay
    url: https://quay.io
    username: your-username
    token: your-token

  - name: internal
    url: https://registry.example.com
    username: your-username
    token: your-token
```

`dockerhub` is special-cased as the default when Docker sends a plain mirror request. For other registries, dockerproxy matches either the configured `name` or the hostname from `url`.

Credentials are optional. If `username` and `token` are present, dockerproxy uses them when talking to that upstream. Tokens are logged only as `present` or `missing`, not as raw values.

## CLI Options

```text
--port <PORT>                listen port, default 8080
--config-file <CONFIG_FILE>  config path, default ./proxy.yaml
--cache-dir <CACHE_DIR>      cache path, default ./cache
--ttl <TTL>                  cache retention in seconds
--log-level <LOG_LEVEL>      0 is quiet, 1 is useful, 2 is chatty
```

The default TTL is about one year. `ttl` and `log_level` can also be set in the config file, which is handy for Home Assistant because add-on options are written to `/data/options.json`. CLI flags win when provided.

In this project, TTL means “how long should we keep the local copy on disk?” Once an entry is older than the TTL, dockerproxy deletes it and the next request has to fetch fresh.

## Cache Behavior

The cache is intentionally boring on disk:

```text
cache/
  dockerhub/
    manifests/
      library/nginx/latest/
        body
        content-type
        digest
    blobs/
      library/nginx/sha256_.../
        body
        content-type
        digest
```

On a request:

1. dockerproxy checks the local cache first.
2. If the entry exists and is still inside TTL, it returns it immediately.
3. If the entry is missing or expired, dockerproxy fetches from upstream.
4. Successful GET responses are written to disk.
5. Expired entries are pruned at startup and periodically while the process runs.

That cache-first behavior is the important bit for the no-internet scenario: if the cluster only asks for images that are already cached and still inside TTL, dockerproxy should not need the internet to serve them.

## UI

There is a deliberately crude UI at:

```text
http://localhost:8080/ui
```

It lists cached entries, their host, type, repository, reference, size, age, and how long until TTL cleanup removes them. Each row has a delete button. It is not fancy, but it is enough to clean up a bad or unwanted cache entry without spelunking through directories.

## Health

```text
GET /health
```

Returns `ok`.

## Published Image

GitHub Actions builds the multi-arch container image and publishes it to GHCR:

```text
ghcr.io/dthompso99/dockerproxy
```

Pushes to `main` publish a branch tag, and version tags like `v0.1.0` publish semantic version tags. Pull requests build the image but do not push it. Published images are built for `linux/amd64` and `linux/arm64`.

The scratch image defaults to:

```text
--config-file /data/options.json --cache-dir /data/cache
```

That lets a Home Assistant add-on pass its options file directly without needing a wrapper script.

The image intentionally does not set a fixed non-root `USER`. Home Assistant replaces `/data` with a Supervisor-managed mount, and a baked-in numeric user can lose write access to `/data/cache`. If you are running dockerproxy somewhere else and want a non-root user, set the user at deployment time after making sure the mounted cache directory is writable by that user.

## Home Assistant

Home Assistant add-on packaging intentionally lives outside this repo. This project stays focused on the proxy binary and container image; the add-on repository can reference the published GHCR image with its `image` field.

That keeps the Home Assistant `config.yaml` metadata separate from dockerproxy's runtime `proxy.yaml`, and keeps this repo from slowly turning into an add-on project by accident.

## What This Is Not

This is not a complete Docker registry implementation. It handles the pieces needed for pulling and caching image manifests/blobs through the Docker Registry HTTP API. Pushes, catalog browsing, garbage collection semantics, and registry administration are out of scope.

It is also not magic prewarm. If an image has never been pulled through dockerproxy, it is not cached yet. The practical pattern is to let normal cluster restarts populate the cache over time, then enjoy the faster and more resilient pulls later.

## Current TODO

- add a small test suite around request parsing, namespace rewriting, TTL, and auth challenge parsing
- do a real no-internet validation pass with a warmed cache
- add a `/ready` endpoint that checks config and cache writability
- reference the published image from a separate Home Assistant add-on repo
