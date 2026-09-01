FROM rust:1.88-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
COPY templates ./templates
RUN cargo build --locked --release

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --create-home autoindex
COPY --from=builder /build/target/release/autoindex-rs /usr/local/bin/autoindex-rs
USER autoindex
WORKDIR /srv
EXPOSE 6701
ENTRYPOINT ["/usr/local/bin/autoindex-rs"]
CMD ["/srv"]
