# syntax=docker/dockerfile:1
# Static musl binary -> distroless/static:nonroot. Multi-arch: docker buildx build --platform linux/amd64,linux/arm64 -f server/Dockerfile .
# (Alpine's Rust toolchain targets the container's native musl arch, so each platform builds natively.)
FROM lukemathwalker/cargo-chef:latest-rust-1-alpine AS chef
RUN apk add --no-cache musl-dev build-base cmake perl
WORKDIR /src

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY core core
COPY server server
COPY devtools devtools
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked -p lexi --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY core core
COPY server server
COPY devtools devtools
RUN cargo build --release --locked -p lexi && cp target/release/lexi /lexi

FROM gcr.io/distroless/static:nonroot
COPY --from=builder /lexi /lexi
USER 65532:65532
EXPOSE 8080 9090
ENTRYPOINT ["/lexi"]
