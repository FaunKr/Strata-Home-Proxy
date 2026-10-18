# syntax=docker/dockerfile:1
# NAS-Proxy für Strata (x86_64), mit Weboberfläche unter /admin/ und dem
# PC-Dienst für Windows zum Herunterladen unter /admin/agent/
#
# Die Abhängigkeiten baut cargo-chef in eigenen Schichten. Solange sich
# Cargo.toml und Cargo.lock nicht ändern, kommen sie in der CI aus dem Cache
# und nur die eigenen Crates werden neu übersetzt.
# Fertiges cargo-leptos statt cargo install, das allein mehrere Minuten übersetzt.
# Eigene Stufe, weil BuildKit ADD von einer URL bei jedem Bau neu ausführt; das
# COPY unten bleibt dagegen im Cache, solange sich der Inhalt nicht ändert.
FROM scratch AS cargo-leptos
ADD --checksum=sha256:1c9cf0e1e3ffae6c8b58d29281d949dab560cea613b80d361f323f93071df7c8 \
    https://github.com/leptos-rs/cargo-leptos/releases/download/v0.3.11/cargo-leptos-x86_64-unknown-linux-gnu.tar.gz \
    /cargo-leptos.tar.gz

FROM rust:1.97-slim-trixie AS chef
# mingw linkt den PC-Dienst für Windows; die .exe braucht danach nur
# Windows-eigene DLLs.
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
# Dieselben Variablen, die cargo-leptos beim Bauen setzt; sonst passen die
# vorgebauten Abhängigkeiten nicht und werden noch einmal übersetzt.
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
# Gleiche Argumente wie die beiden cargo-build-Aufrufe von cargo-leptos.
RUN cargo chef cook --recipe-path recipe.json --locked --release \
        -p strata-nas-proxy --bin strata-nas-proxy --no-default-features
RUN cargo chef cook --recipe-path recipe.json --locked --profile wasm-release \
        --target wasm32-unknown-unknown --target-dir target/front \
        -p strata-admin-ui --no-default-features --features hydrate
RUN cargo chef cook --recipe-path recipe.json --locked --release \
        --target x86_64-pc-windows-gnu -p strata-pc-agent
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# Der Proxy liefert das Installationsskript des PC-Dienstes aus (include_str!).
COPY deploy/windows/install.ps1 ./deploy/windows/
RUN cargo leptos build --release --project admin-ui
# Der Commit kommt in die Version des PC-Dienstes (crates/pc-agent/build.rs);
# daran erkennt die Übersicht, ob ein Update verfügbar ist.
ARG STRATA_BUILD_ID=dev
RUN cargo build --locked --release --target x86_64-pc-windows-gnu -p strata-pc-agent
# Der NAS liest die Version aus der .exe; fehlt sie, zeigt die Übersicht nie
# ein Update an. Das soll hier auffallen, nicht erst auf dem PC.
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
