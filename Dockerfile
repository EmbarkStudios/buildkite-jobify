FROM ekidd/rust-musl-builder:nightly-2019-04-25-openssl11 AS builder

# Add our source code.
ADD ./Cargo.toml ./
ADD ./src/ ./src/
ADD ./buildkite/ ./buildkite/

# Fix permissions on source code.
RUN sudo chown -R rust:rust /home/rust

# Build our application.
RUN cargo build --release

FROM alpine:latest
RUN apk --no-cache add ca-certificates

COPY --from=builder \
    /home/rust/src/target/x86_64-unknown-linux-musl/release/buildkite-jobify \
    /jobify/buildkite-jobify

CMD /jobify/buildkite-jobify
