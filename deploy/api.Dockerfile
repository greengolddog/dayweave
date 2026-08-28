FROM rust:1.95-bookworm AS builder
WORKDIR /src
COPY . .
RUN cargo build --locked --release --package dayweave-api

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home dayweave
COPY --from=builder /src/target/release/dayweave-api /usr/local/bin/dayweave-api
USER 10001:10001
EXPOSE 8787
ENTRYPOINT ["/usr/local/bin/dayweave-api"]

