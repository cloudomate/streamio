FROM rust:1.82-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.toml
COPY types/ types/
COPY gateway/ gateway/
COPY client/ client/
# Create stub dirs for other workspace members so cargo doesn't complain
RUN mkdir -p backend/src && echo "fn main(){}" > backend/src/main.rs && \
    cp types/Cargo.toml backend/Cargo.toml || true
RUN mkdir -p service/src && echo "fn main(){}" > service/src/main.rs
RUN mkdir -p display-ctl/src && echo "fn main(){}" > display-ctl/src/main.rs
RUN mkdir -p session-manager/src && echo "fn main(){}" > session-manager/src/main.rs

# Build only the gateway
RUN cargo build --release -p streamio-gateway 2>&1 || true
# Second pass to get the real binary (first may fail on workspace resolution)
RUN cargo build --release -p streamio-gateway

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/streamio-gateway /usr/local/bin/streamio-gateway
EXPOSE 8080
CMD ["streamio-gateway"]
