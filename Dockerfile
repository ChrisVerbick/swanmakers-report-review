# One static-ish binary on a slim Debian, running as an unprivileged user with /data as its only
# writable place (the Compose file mounts the rest read-only).
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home review
COPY --from=build /src/target/release/server /usr/local/bin/designcraft-review
USER review
ENV BIND=0.0.0.0:8080 DATA_DIR=/data TRUST_PROXY=1 RUST_LOG=info
EXPOSE 8080
ENTRYPOINT ["designcraft-review"]
