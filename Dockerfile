# The service and the browser client in one image: the client is built with Trunk and served by
# the service from /web. Runs as an unprivileged user with /data as its only writable place (the
# Compose file mounts the rest read-only).
FROM rust:1-bookworm AS build
RUN rustup target add wasm32-unknown-unknown && cargo install trunk --locked --version 0.22.0
WORKDIR /src
COPY . .
RUN cargo build --release -p server
RUN cd web && trunk build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home review
COPY --from=build /src/target/release/server /usr/local/bin/designcraft-review
COPY --from=build /src/web/dist /web
USER review
ENV BIND=0.0.0.0:8080 DATA_DIR=/data WEB_DIR=/web TRUST_PROXY=1 RUST_LOG=info
EXPOSE 8080
ENTRYPOINT ["designcraft-review"]
