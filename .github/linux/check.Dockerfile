# Ubuntu 24.04 with the pinned Rust toolchain, for `.github/linux-check.sh`.
FROM ubuntu:24.04
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      build-essential git ca-certificates rustup rsync && rm -rf /var/lib/apt/lists/*
RUN useradd -m dev
USER dev
RUN rustup default 1.99.1 && rustup component add rustfmt clippy
RUN mkdir -p /home/dev/work /home/dev/target
WORKDIR /home/dev/work
