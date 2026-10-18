# NAS-Proxy für Strata (x86_64)
FROM rust:1.97-slim-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked -p strata-nas-proxy

FROM debian:trixie-slim
COPY --from=build /src/target/release/strata-nas-proxy /usr/local/bin/strata-nas-proxy
ENV DATA_DIR=/data
VOLUME /data
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/strata-nas-proxy"]
