FROM rust:1.50.0 as build

RUN set -eux; \
    apt-get update; \
    apt-get install musl-tools -y; \
    rustup target add x86_64-unknown-linux-musl;

WORKDIR /usr/src

# Add our source code.
ADD ./Cargo.toml ./Cargo.lock ./
ADD ./src/ ./src/
ADD ./buildkite/ ./buildkite/

# Build our application.
RUN cargo install --target x86_64-unknown-linux-musl --path . && strip /usr/local/cargo/bin/buildkite-jobify

FROM alpine:latest
RUN apk --no-cache add ca-certificates

COPY --from=build \
    /usr/local/cargo/bin/buildkite-jobify \
    /jobify/buildkite-jobify

CMD /jobify/buildkite-jobify
