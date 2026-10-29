# syntax=docker/dockerfile:1.7
# Flixnet: multi-stage build. Context is the repository root:
#   docker build -f docker/Dockerfile -t flixnet .

# ---- 1. Web UI -------------------------------------------------------------
FROM node:24-trixie-slim AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build

# ---- 2. Server binary (embeds web/dist) ------------------------------------
FROM rust:1-slim-trixie AS server
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY crates crates
COPY xtask xtask
COPY --from=web /src/web/dist web/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p flixnet-server \
    && install -Dm755 target/release/flixnet-server /out/flixnet-server

# ---- 3. Runtime ------------------------------------------------------------
FROM debian:trixie-slim
ARG DEBIAN_FRONTEND=noninteractive

# jellyfin-ffmpeg: FFmpeg with full VAAPI/QSV/NVENC pipelines, tone mapping and
# the Intel iHD driver bundled. mesa-va-drivers covers AMD.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl gnupg tini \
    && install -d /etc/apt/keyrings \
    && curl -fsSL https://repo.jellyfin.org/jellyfin_team.gpg.key \
       | gpg --dearmor -o /etc/apt/keyrings/jellyfin.gpg \
    && echo "deb [signed-by=/etc/apt/keyrings/jellyfin.gpg] https://repo.jellyfin.org/debian trixie main" \
       > /etc/apt/sources.list.d/jellyfin.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends jellyfin-ffmpeg7 mesa-va-drivers \
    && apt-get purge -y gnupg && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/*

COPY --from=server /out/flixnet-server /usr/local/bin/flixnet-server
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod 755 /usr/local/bin/entrypoint.sh

ENV PATH="/usr/lib/jellyfin-ffmpeg:${PATH}" \
    FLIXNET_CONFIG=/config/flixnet.toml \
    FLIXNET_PATHS__CONFIG_DIR=/config \
    FLIXNET_SERVER__BIND=0.0.0.0:8097 \
    PUID=99 \
    PGID=100 \
    UMASK=022

EXPOSE 8097
VOLUME ["/config"]
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s \
    CMD curl -fsS http://127.0.0.1:8097/api/v1/system/info >/dev/null || exit 1

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/entrypoint.sh"]
CMD ["flixnet-server", "serve"]
