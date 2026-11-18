# syntax=docker/dockerfile:1
# SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
# SPDX-License-Identifier: AGPL-3.0-only

# ---- Build stage: the server and the WASM module of the page ----
FROM rust:1-bookworm AS build
WORKDIR /src

RUN rustup target add wasm32-unknown-unknown \
 && cargo install wasm-bindgen-cli --version 0.2.130 --locked

# Dependencies first, so that a change in the source does not rebuild them.
COPY Cargo.toml Cargo.lock ./
COPY crates/wire/Cargo.toml crates/wire/Cargo.toml
COPY crates/xsrv-server/Cargo.toml crates/xsrv-server/Cargo.toml
COPY crates/xsrv-wasm/Cargo.toml crates/xsrv-wasm/Cargo.toml
RUN mkdir -p crates/wire/src crates/xsrv-server/src crates/xsrv-wasm/src \
 && echo '#![no_std]' > crates/wire/src/lib.rs \
 && echo 'fn main() {}' > crates/xsrv-server/src/main.rs \
 && echo '' > crates/xsrv-server/src/lib.rs \
 && mkdir -p crates/xsrv-server/benches \
 && echo 'fn main() {}' > crates/xsrv-server/benches/updates.rs \
 && echo '' > crates/xsrv-wasm/src/lib.rs \
 && cargo build --release -p xsrv-server \
 && cargo build --release -p xsrv-wasm --target wasm32-unknown-unknown

COPY crates crates
COPY web web
# The sources are newer than the dummy build, so cargo rebuilds the crates of this workspace.
RUN find crates -name '*.rs' -exec touch {} + \
 && cargo build --release -p xsrv-server \
 && cargo build --release -p xsrv-wasm --target wasm32-unknown-unknown \
 && wasm-bindgen --target web --out-dir web/pkg \
      target/wasm32-unknown-unknown/release/xsrv_wasm.wasm \
 && rm -rf web/test web/package.json

# ---- Runtime stage: only the server and the page ----
FROM debian:bookworm-slim AS runtime
LABEL org.opencontainers.image.licenses="AGPL-3.0-only"

# A user that cannot write to the image. The X socket goes to /tmp/.X11-unix, which is a volume
# that other containers can share. The directory exists here so that a new volume copies its mode.
RUN useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin xsrv \
 && mkdir -p /tmp/.X11-unix \
 && chown xsrv /tmp/.X11-unix \
 && chmod 755 /tmp/.X11-unix

# `docker build --build-arg VIDEO=1 ...` adds ffmpeg (about 100 MB) for XSRV_VIDEO=h264.
ARG VIDEO=0
RUN if [ "$VIDEO" = "1" ]; then \
      apt-get update \
      && apt-get install -y --no-install-recommends ffmpeg \
      && rm -rf /var/lib/apt/lists/*; \
    fi

COPY --from=build /src/target/release/xsrv-web /usr/local/bin/xsrv-web
COPY --from=build /src/web /app/web
COPY LICENSE THIRD-PARTY-NOTICES.txt /usr/share/doc/xsrv-web/
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh

ENV DISPLAY=:1 \
    XSRV_ADDR=0.0.0.0:8080 \
    XSRV_X11_SOCKET=/tmp/.X11-unix/X1 \
    XSRV_WEB_DIR=/app/web

# One port: the page and the WebSocket.
EXPOSE 8080

USER xsrv
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
