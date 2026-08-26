# external-dns-webhook-timeweb

ExternalDNS webhook provider for managing DNS records in [Timeweb Cloud](https://timeweb.cloud/).

## Purpose

Use ExternalDNS with domains managed by Timeweb Cloud. The provider supports A, AAAA, CNAME, MX, SRV, and TXT records.

## Prerequisites

- A Timeweb Cloud API token with permission to manage DNS records.
- A domain managed by Timeweb Cloud.
- An ExternalDNS version with webhook provider support.
- Docker Engine, Helm 3, or Rust and Cargo when building from source.
- A Kubernetes Secret named `timeweb-cloud-token` in the `external-dns` namespace with the token stored under the `token` key.

## Quick start

Build and run the provider:

```bash
WEBHOOK_TAG='vX.Y.Z'
docker build --pull --tag docker.io/library/external-dns-webhook-timeweb:"$WEBHOOK_TAG" .
docker run --rm \
  --name timeweb-webhook \
  --env TIMEWEB_CLOUD_TOKEN='your-timeweb-api-token' \
  --publish 127.0.0.1:8888:8888 \
  --publish 127.0.0.1:8080:8080 \
  docker.io/library/external-dns-webhook-timeweb:"$WEBHOOK_TAG"
```

Configure ExternalDNS to use the running provider:

```bash
external-dns \
  --provider=webhook \
  --webhook-provider-url=http://127.0.0.1:8888
```

### Install with ExternalDNS

ExternalDNS can run the webhook as a sidecar in its own pod. Save these values as `external-dns-values.yaml`:

```yaml
sources:
  - gateway-httproute
gatewayNamespace: monitoring
domainFilters:
  - example.com
txtOwnerId: external-dns
provider:
  name: webhook
  webhook:
    image:
      repository: ghcr.io/evoxide/external-dns-webhook-timeweb
    env:
      - name: TIMEWEB_CLOUD_TOKEN
        valueFrom:
          secretKeyRef:
            name: timeweb-cloud-token
            key: token
      - name: TIMEWEB_CLOUD_LISTEN_ADDR
        value: 127.0.0.1:8888
      - name: DOMAIN_FILTER
        value: example.com
    securityContext:
      runAsUser: 65532
      runAsGroup: 65532
```

Install the chart and pass the webhook image tag separately:

```bash
WEBHOOK_TAG='vX.Y.Z'
helm repo add external-dns https://kubernetes-sigs.github.io/external-dns/
helm repo update
helm upgrade --install external-dns external-dns/external-dns \
  --namespace external-dns \
  --create-namespace \
  --values external-dns-values.yaml \
  --set-string provider.webhook.image.tag="$WEBHOOK_TAG"
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
git tag vX.Y.Z
git push origin vX.Y.Z
```

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
