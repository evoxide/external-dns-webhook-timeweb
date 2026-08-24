FROM docker.io/library/rust:1.89-bookworm AS builder

WORKDIR /workspace

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN --mount=type=cache,id=timeweb-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=timeweb-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=timeweb-target,target=/workspace/target \
    cargo build --locked --release \
    && cp /workspace/target/release/external-dns-webhook-timeweb /workspace/external-dns-webhook-timeweb

FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder /workspace/external-dns-webhook-timeweb /usr/local/bin/external-dns-webhook-timeweb

ENV TIMEWEB_CLOUD_LISTEN_ADDR=0.0.0.0:8888 \
    TIMEWEB_CLOUD_METRICS_ADDR=0.0.0.0:8080

EXPOSE 8888 8080

USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/external-dns-webhook-timeweb"]
