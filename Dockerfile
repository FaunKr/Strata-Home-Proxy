# syntax=docker/dockerfile:1
# Proxy for Strata (x86_64), with the web UI under /admin/ and the
# PC service for Windows to download under /admin/agent/
#
# cargo-chef builds the dependencies in layers of their own. As long as
# Cargo.toml and Cargo.lock do not change, CI takes them from the cache
# and only our own crates are compiled again.
# A prebuilt cargo-leptos instead of cargo install, which alone compiles for several minutes.
# A stage of its own, because BuildKit runs ADD from a URL again on every build; the
# COPY below, by contrast, stays cached as long as the content does not change.
FROM scratch AS cargo-leptos
ADD --checksum=sha256:1c9cf0e1e3ffae6c8b58d29281d949dab560cea613b80d361f323f93071df7c8 \
    https://github.com/leptos-rs/cargo-leptos/releases/download/v0.3.11/cargo-leptos-x86_64-unknown-linux-gnu.tar.gz \
    /cargo-leptos.tar.gz

FROM rust:1.97-slim-trixie AS chef
# mingw links the PC service for Windows; the .exe then needs only
# DLLs that ship with Windows.
RUN apt-get update \
    && apt-get install -y --no-install-recommends gcc-mingw-w64-x86-64 \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add wasm32-unknown-unknown x86_64-pc-windows-gnu \
    && cargo install cargo-chef --locked --version 0.1.78
COPY --from=cargo-leptos /cargo-leptos.tar.gz /tmp/
RUN tar xzf /tmp/cargo-leptos.tar.gz -C /usr/local/cargo/bin --strip-components=1 \
        cargo-leptos-x86_64-unknown-linux-gnu/cargo-leptos \
    && rm /tmp/cargo-leptos.tar.gz
WORKDIR /src

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS build
# The same variables cargo-leptos sets when building; otherwise the
# prebuilt dependencies do not match and are compiled once more.
ENV LEPTOS_OUTPUT_NAME=admin-ui \
    LEPTOS_SITE_ROOT=target/site \
    LEPTOS_SITE_PKG_DIR=admin/pkg \
    LEPTOS_SITE_ADDR=127.0.0.1:3000 \
    LEPTOS_RELOAD_PORT=3001 \
    LEPTOS_LIB_DIR=crates/admin-ui \
    LEPTOS_BIN_DIR=crates/nas-proxy \
    LEPTOS_JS_MINIFY=true \
    LEPTOS_HASH_FILES=false
COPY --from=planner /src/recipe.json recipe.json
# The same arguments as the two cargo build calls of cargo-leptos.
RUN cargo chef cook --recipe-path recipe.json --locked --release \
        -p strata-nas-proxy --bin strata-nas-proxy --no-default-features
RUN cargo chef cook --recipe-path recipe.json --locked --profile wasm-release \
        --target wasm32-unknown-unknown --target-dir target/front \
        -p strata-admin-ui --no-default-features --features hydrate
RUN cargo chef cook --recipe-path recipe.json --locked --release \
        --target x86_64-pc-windows-gnu -p strata-pc-agent
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# The proxy serves the PC service's install script (include_str!).
COPY deploy/windows/install.ps1 ./deploy/windows/
# The proxy sends the code benchmark's tasks to its runner (include_str!).
COPY tools/code-bench/tasks ./tools/code-bench/tasks
RUN cargo leptos build --release --project admin-ui
# The commit goes into the PC service's version (crates/pc-agent/build.rs);
# from it the Overview tells whether an update is available.
ARG STRATA_BUILD_ID=dev
RUN cargo build --locked --release --target x86_64-pc-windows-gnu -p strata-pc-agent
# The NAS reads the version from the .exe; without it, the Overview never
# shows an update. That should show up here, not only on the PC.

COPY scripts/check-exe-version.sh ./scripts/
RUN scripts/check-exe-version.sh \
        target/x86_64-pc-windows-gnu/release/strata-pc-agent.exe "$STRATA_BUILD_ID"

FROM debian:trixie-slim
COPY --from=build /src/target/release/strata-nas-proxy /usr/local/bin/strata-nas-proxy
COPY --from=build /src/target/site /usr/local/share/strata-proxy/site
COPY --from=build /src/target/x86_64-pc-windows-gnu/release/strata-pc-agent.exe \
    /usr/local/share/strata-proxy/strata-pc-agent.exe
ENV DATA_DIR=/data
ENV LEPTOS_SITE_ROOT=/usr/local/share/strata-proxy/site
VOLUME /data
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/strata-nas-proxy"]
