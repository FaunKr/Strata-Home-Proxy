# NAS-Proxy für Strata (x86_64), mit Weboberfläche unter /admin/
FROM rust:1.97-slim-trixie AS build
RUN rustup target add wasm32-unknown-unknown \
    && cargo install cargo-leptos --locked --version 0.3.11
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo leptos build --release --project admin-ui

FROM debian:trixie-slim
COPY --from=build /src/target/release/strata-nas-proxy /usr/local/bin/strata-nas-proxy
COPY --from=build /src/target/site /usr/local/share/strata-proxy/site
ENV DATA_DIR=/data
ENV LEPTOS_SITE_ROOT=/usr/local/share/strata-proxy/site
VOLUME /data
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/strata-nas-proxy"]
