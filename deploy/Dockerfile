# Two stages: the builder carries the Rust toolchain, the runtime carries a
# single static-ish binary and nothing else worth attacking.

FROM rust:1-slim-bookworm AS builder
WORKDIR /build

RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config \
 && rm -rf /var/lib/apt/lists/*

# Copy manifests first so the dependency build layer is cached independently of
# source edits — a one-line change should not recompile the whole tree.
COPY Cargo.toml Cargo.lock ./
COPY crates/query-core/Cargo.toml crates/query-core/
COPY crates/postgres/Cargo.toml   crates/postgres/
COPY crates/graphql/Cargo.toml    crates/graphql/
COPY crates/server/Cargo.toml     crates/server/
COPY bins/superquery-query/Cargo.toml bins/superquery-query/
RUN mkdir -p crates/query-core/src crates/postgres/src crates/graphql/src crates/server/src bins/superquery-query/src \
 && touch crates/query-core/src/lib.rs crates/postgres/src/lib.rs crates/graphql/src/lib.rs crates/server/src/lib.rs \
 && echo 'fn main(){}' > bins/superquery-query/src/main.rs \
 && cargo build --release --locked \
 && rm -rf crates bins

COPY . .
# Touch the real sources so cargo rebuilds them over the stub layer.
RUN find crates bins -name '*.rs' -exec touch {} + \
 && cargo build --release --locked -p superquery-query

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --create-home --uid 10001 superquery

COPY --from=builder /build/target/release/superquery-query /usr/local/bin/superquery-query

USER superquery
EXPOSE 3000
ENTRYPOINT ["/usr/local/bin/superquery-query"]
