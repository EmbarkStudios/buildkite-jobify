# buildkite-jobify
[![FOSSA Status](https://app.fossa.io/api/projects/git%2Bgithub.com%2FEmbarkStudios%2Fbuildkite-jobify.svg?type=shield)](https://app.fossa.io/projects/git%2Bgithub.com%2FEmbarkStudios%2Fbuildkite-jobify?ref=badge_shield)


Watches 1 or more Buildkite pipelines via the GraphQL API to kick off temporary Kubernetes jobs that (presumably)
spin up Buildkite agents to pick up the Buildkite jobs that Buildkite has scheduled but not yet found an agent for.


## Repository Setup
The configuration of the Kubernetes jobs specs and the which agent tags correspond to them are expected to be
stored in the repositories that the pipelines are attached to, thus keeping the CI configuration within that repository.

The pipeline upload job for your repository should create a gzipped tarball of your configuration and upload it before adding the rest of the pipeline's steps so that the program can download the exact configuration for the commit being built.

Here is an example bash script, intended to work in an Alpine Linux container.

```sh
#!/bin/bash
set -eu

echo "--- :aws-artifact: Uploading Jobify templates"
archive="jobify.tar.gz"

# Create a tarball of our jobify files
tar c -zf "$archive" -C .ci/jobify .

# Checksum the tarball and use that as our key
chksum=$(sha1sum $archive | awk '{ print $1 }')

# TODO: Just support CAS artifacts, and only upload unique ones (but maybe buildkite already does?)

# Upload the tarball as a Buildkite artifact
buildkite-agent artifact upload "$archive" &> upload_out
rm "$archive"

upload_info=$(cat upload_out)

# Buildkite doesn't seem to have a convenient way to know the ID assigned to the artifact
# so just cheat and parse its output for now
rx='Uploading\sartifact\s([0-9a-f-]{36})\sjobify\.tar\.gz'
if [[ "$upload_info" =~ $rx ]]; then
    # Set a metadata key to point to the archive we uploaded so that jobify can pick it up
    buildkite-agent meta-data set "jobify-artifact-id" "${BASH_REMATCH[1]}"
    # Set the chksum so we can avoid an additional lookup if jobify already has the data
    buildkite-agent meta-data set "jobify-artifact-chksum" "$chksum"
else
    echo "Failed to upload jobify configuration"
    exit 1
fi

echo "--- Uploading :pipeline:"
# Upload our main pipeline to kick off the rest of the build
buildkite-agent pipeline upload "$(dirname "$0")/pipeline.yml"
```

## License
[![FOSSA Status](https://app.fossa.io/api/projects/git%2Bgithub.com%2FEmbarkStudios%2Fbuildkite-jobify.svg?type=large)](https://app.fossa.io/projects/git%2Bgithub.com%2FEmbarkStudios%2Fbuildkite-jobify?ref=badge_large)