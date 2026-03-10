FROM rust:1.93 AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/

RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
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
