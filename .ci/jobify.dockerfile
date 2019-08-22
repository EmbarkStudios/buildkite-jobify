FROM rust:1.35.0 as build

WORKDIR /usr/src

# For now we stick to a specific nightly, at least until async/await has
# stabilized and various dependencies have updated and won't break constantly
RUN rustup default nightly-2019-04-28-x86_64-unknown-linux-gnu
RUN rustup target add x86_64-unknown-linux-musl

RUN USER=root cargo new buildkite-jobify
WORKDIR /usr/src/buildkite-jobify
COPY Cargo.toml Cargo.lock ./
RUN cargo build --release --target x86_64-unknown-linux-musl

# Copy our source files in so we can build
COPY src ./src
RUN cargo install --target x86_64-unknown-linux-musl --path .

FROM scratch
COPY --from=build /usr/local/cargo/bin/buildkite-jobify .
USER 1000

CMD ["./buildkite-jobify"]
