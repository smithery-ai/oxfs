FROM rust:latest AS oxfs-builder
WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY crates/ crates/
RUN cargo build --release

FROM ubuntu:24.04

RUN apt-get update && apt-get install -y --no-install-recommends \
    fuse3 curl git python3 ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# oxfs
COPY --from=oxfs-builder /app/target/release/oxfs /usr/local/bin/oxfs

# tigrisfs
ARG TIGRISFS_VERSION=1.2.1
RUN ARCH=$(dpkg --print-architecture) && \
    curl -fsSLo /tmp/tigrisfs.deb \
    "https://github.com/tigrisdata/tigrisfs/releases/download/v${TIGRISFS_VERSION}/tigrisfs_${TIGRISFS_VERSION}_linux_${ARCH}.deb" && \
    apt-get update && apt-get install -y --no-install-recommends /tmp/tigrisfs.deb && \
    rm /tmp/tigrisfs.deb && rm -rf /var/lib/apt/lists/*

# geesefs
RUN ARCH=$(dpkg --print-architecture) && \
    if [ "$ARCH" = "amd64" ]; then GOARCH=amd64; else GOARCH=arm64; fi && \
    curl -fsSLo /usr/local/bin/geesefs \
    "https://github.com/yandex-cloud/geesefs/releases/latest/download/geesefs-linux-${GOARCH}" && \
    chmod +x /usr/local/bin/geesefs

# juicefs
RUN curl -sSL https://d.juicefs.com/install | sh -

RUN echo "user_allow_other" >> /etc/fuse.conf

COPY bench/quick.sh /bench/quick.sh
COPY bench/run_all.sh /bench/run_all.sh
RUN chmod +x /bench/*.sh

WORKDIR /bench
ENTRYPOINT ["/bin/bash", "-c"]
CMD ["/bench/run_all.sh"]
