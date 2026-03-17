# syntax=docker/dockerfile:1.6

FROM --platform=$TARGETPLATFORM rust:1.85-alpine AS builder

WORKDIR /app

# Some Rust crates may compile small C/C++ shims; keep a minimal build toolchain installed.
RUN apk add --no-cache build-base

# Build deps first for better layer caching.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && printf '%s\n' 'fn main() {}' > src/main.rs
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --locked --release

COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --locked --release

FROM alpine:3.20 AS runtime

RUN apk add --no-cache ca-certificates \
  && adduser -D -H -u 10001 -s /sbin/nologin hachimi

WORKDIR /app

COPY --from=builder /app/target/release/hachimi_deployer /usr/local/bin/hachimi_deployer
COPY config/deployer.example.toml /app/config/deployer.example.toml

EXPOSE 3000

USER hachimi

ENTRYPOINT ["/usr/local/bin/hachimi_deployer"]
