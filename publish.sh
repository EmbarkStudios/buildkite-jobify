#!/bin/bash
set -e

tag=${1:-"gcr.io/embark-shared/ark/buildkite-jobify:latest"}

cargo build --release --target x86_64-unknown-linux-musl
strip target/x86_64-unknown-linux-musl/release/buildkite-jobify

podman build -t "$tag" -f Dockerfile .
podman push "$tag"
