# syntax=docker/dockerfile:1

# Builder stage: compile the Rust project with dependencies cached.
FROM rust:1.91-alpine3.22 AS builder

WORKDIR /app

# Install build dependencies needed by some crates.
RUN apk add --no-cache \
    build-base \
    pkgconfig \
    openssl-dev \
    openssl-libs-static

# Copy manifest separately to leverage Docker layer caching for dependencies.
COPY Cargo.toml ./
COPY Cargo.lock ./

# Provide a temporary target so `cargo fetch` succeeds without the full source tree.
RUN mkdir src \
    && printf 'fn main() {}\n' > src/main.rs \
    && cargo fetch \
    && rm -rf src

# Copy the actual source code.
COPY src ./src

# Build the release binary.
RUN cargo build --release

# Runtime stage: use a tiny base image to run the compiled binary.
FROM alpine:3.22 AS runtime

WORKDIR /app

RUN addgroup -S fotobot \
    && adduser -S -h /home/fotobot -G fotobot fotobot \
    && mkdir -p /app/cache /home/fotobot/.config/fotobot \
    && chown -R fotobot:fotobot /app /home/fotobot

# Copy the statically linked binary from the builder stage.
COPY --from=builder /app/target/release/fotobot_rs /usr/local/bin/fotobot_rs
COPY locales /app/locales

ENV HOME=/home/fotobot
ENV RUST_LOG=info

USER fotobot:fotobot

ENTRYPOINT ["/usr/local/bin/fotobot_rs"]
