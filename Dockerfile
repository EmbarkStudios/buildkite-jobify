FROM docker.io/rust:1.63.0-slim-bullseye as build

FROM docker.io/alpine:latest
RUN apk --no-cache add ca-certificates

ADD target/x86_64-unknown-linux-musl/release/buildkite-jobify /jobify/buildkite-jobify

CMD /jobify/buildkite-jobify
