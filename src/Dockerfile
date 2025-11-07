FROM rust:1.84 AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY .cargo ./.cargo
COPY vendor ./vendor
COPY src ./src
COPY config ./config
RUN cargo build --release --offline

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates curl nano wget git iputils-ping && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/zeitpeer /usr/local/bin/zeitpeer
COPY config /app/config
ENV CONFIG=/app/config/peer1.toml
ENTRYPOINT ["/usr/local/bin/zeitpeer"]
