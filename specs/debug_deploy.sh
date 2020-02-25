#!/bin/bash
set -ex

context=$1
namespace=${2:-buildkite}

if kubectl --context "$context" -n "$namespace" delete deployments/buildkite-jobify; then
    # Now wait for the actual pod to be delete as well
    echo "waiting for buildkite-jobify pod to shutdown..."
    kubectl --context "$context" -n "$namespace" wait pods --for=delete --timeout=90s -l app=buildkite-jobify
fi

kubectl --context "$context" -n "$namespace" apply -f specs/deployment.yaml

# Wait for the pod to spin up completely
kubectl --context "$context" -n "$namespace" wait pods --for=condition=Ready -l app=buildkite-jobify

#kubectl --context "$context" -n "$namespace" logs --follow -l name=buildkite-jobify --all-containers
