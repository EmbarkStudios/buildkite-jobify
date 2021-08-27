# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- next-header -->
## [Unreleased] - ReleaseDate
## [0.6.1] - 2021-08-27
### Changed
- Updated dependencies

## [0.6.0] - 2021-03-12
### Changed
- [PR#28](https://github.com/EmbarkStudios/buildkite-jobify/pull/28) changed up how pipelines are specified, instead of specifying a slug, the user must specify the unique GraphQL pipeline ID that can be found in the pipeline's settings page (or via the GraphQL API). This means that the there is also no longer a need to specify the organization since pipelines are no longer queried for.

## [0.5.1] - 2021-03-12
### Changed
- Cluster can now be specified in the config as well as the CLI.

## [0.5.0] - 2021-03-12
### Changed
- [PR#26](https://github.com/EmbarkStudios/buildkite-jobify/pull/26) resolved [#5](https://github.com/EmbarkStudios/buildkite-jobify/issues/5) by introducing a `clusters` CLI argument and agents.toml config field so that a particular jobify instance will only start jobs with the same cluster.
- [PR#23](https://github.com/EmbarkStudios/buildkite-jobify/pull/23) resolved [#8](https://github.com/EmbarkStudios/buildkite-jobify/issues/8) by changing the configuration of which pipelines to watch from using the user facing name, eg `prefix/name-of-the-thing`, to the pipeline's slug, eg `prefix-name-of-the-thing`.

## [0.4.0] - 2020-06-12
### Added
- The before CHANGELOG times which we will just say are lost to history.

<!-- next-url -->
[Unreleased]: https://github.com/EmbarkStudios/buildkite-jobify/compare/0.6.1...HEAD
[0.6.1]: https://github.com/EmbarkStudios/buildkite-jobify/compare/0.6.0...0.6.1
[0.6.0]: https://github.com/EmbarkStudios/buildkite-jobify/compare/0.5.1...0.6.0
[0.5.1]: https://github.com/EmbarkStudios/buildkite-jobify/compare/0.5.0...0.5.1
[0.5.0]: https://github.com/EmbarkStudios/buildkite-jobify/compare/0.4.0...0.5.0
[0.4.0]: https://github.com/EmbarkStudios/buildkite-jobify/releases/tag/0.4.0
