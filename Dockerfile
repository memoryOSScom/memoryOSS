FROM rust:1.93-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/

RUN cargo build --release --locked

FROM debian:bookworm-slim

ARG VERSION=dev
ARG VCS_REF=unknown
ARG BUILD_DATE=unknown

LABEL org.opencontainers.image.title="memoryOSS" \
      org.opencontainers.image.description="Persistent memory for AI agents with local-first storage and hybrid recall." \
      org.opencontainers.image.url="https://memoryoss.com" \
      org.opencontainers.image.source="https://github.com/memoryOSScom/memoryOSS" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${VCS_REF}" \
      org.opencontainers.image.created="${BUILD_DATE}" \
      org.opencontainers.image.licenses="AGPL-3.0-only"

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libstdc++6 \
    && rm -rf /var/lib/apt/lists/*

RUN groupadd -r memoryoss && useradd -r -g memoryoss -d /data -s /sbin/nologin memoryoss
RUN mkdir -p /data && chown memoryoss:memoryoss /data

COPY --from=builder /build/target/release/memoryoss /usr/local/bin/memoryoss

USER memoryoss
WORKDIR /data

EXPOSE 8000

VOLUME ["/data"]

ENTRYPOINT ["memoryoss"]
CMD ["serve"]
