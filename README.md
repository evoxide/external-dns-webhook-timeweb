# external-dns-webhook-timeweb

ExternalDNS webhook provider for managing DNS records in [Timeweb Cloud](https://timeweb.cloud/).

## Purpose

Use ExternalDNS with domains managed by Timeweb Cloud. The provider supports A, AAAA, CNAME, MX, SRV, and TXT records.

## Prerequisites

- A Timeweb Cloud API token with permission to manage DNS records.
- A domain managed by Timeweb Cloud.
- An ExternalDNS version with webhook provider support.
- Docker Engine, or Rust and Cargo when building from source.

## Quick start

Build and run the provider:

```bash
docker build --pull --tag docker.io/library/external-dns-webhook-timeweb:0.6.0 .
docker run --rm \
  --name timeweb-webhook \
  --env TIMEWEB_CLOUD_TOKEN='your-timeweb-api-token' \
  --publish 127.0.0.1:8888:8888 \
  --publish 127.0.0.1:8080:8080 \
  docker.io/library/external-dns-webhook-timeweb:0.6.0
```

Configure ExternalDNS to use the running provider:

```bash
external-dns \
  --provider=webhook \
  --webhook-provider-url=http://127.0.0.1:8888
```

The provider uses port `8888` for ExternalDNS and port `8080` for health checks and metrics by default.

## Commands and configuration

### Build and run from source

```bash
export TIMEWEB_CLOUD_TOKEN='your-timeweb-api-token'
cargo run --release
```

### Configuration

| Variable | Required | Default |
| --- | --- | --- |
| `TIMEWEB_CLOUD_TOKEN` | Yes | — |
| `TIMEWEB_CLOUD_API_URL` | No | `https://api.timeweb.cloud` |
| `TIMEWEB_CLOUD_LISTEN_ADDR` | No | `127.0.0.1:8888` |
| `TIMEWEB_CLOUD_METRICS_ADDR` | No | `0.0.0.0:8080` |
| `TIMEWEB_CLOUD_HTTP_TIMEOUT` | No | `10s` |
| `DOMAIN_FILTER` | No | Empty |
| `EXCLUDE_DOMAIN_FILTER` | No | Empty |
| `REGEXP_DOMAIN_FILTER` | No | Empty |
| `REGEXP_DOMAIN_FILTER_EXCLUSION` | No | Empty |
| `RUST_LOG` | No | `info` |

`DOMAIN_FILTER` and `EXCLUDE_DOMAIN_FILTER` accept comma-separated domain names. Regular-expression filters take precedence when configured.

### Publish a release image

Create a tag matching `v*.*.*` and push it to GitHub:

```bash
git tag v0.6.0
git push origin v0.6.0
```

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
