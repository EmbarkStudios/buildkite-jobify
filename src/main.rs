// BEGIN - Embark standard lints v0.4
// do not change or add/remove here, but one can add exceptions after this section
// for more info see: <https://github.com/EmbarkStudios/rust-ecosystem/issues/59>
#![deny(unsafe_code)]
#![warn(
    clippy::all,
    clippy::await_holding_lock,
    clippy::char_lit_as_u8,
    clippy::checked_conversions,
    clippy::dbg_macro,
    clippy::debug_assert_with_mut_call,
    clippy::doc_markdown,
    clippy::empty_enum,
    clippy::enum_glob_use,
    clippy::exit,
    clippy::expl_impl_clone_on_copy,
    clippy::explicit_deref_methods,
    clippy::explicit_into_iter_loop,
    clippy::fallible_impl_from,
    clippy::filter_map_next,
    clippy::float_cmp_const,
    clippy::fn_params_excessive_bools,
    clippy::if_let_mutex,
    clippy::implicit_clone,
    clippy::imprecise_flops,
    clippy::inefficient_to_string,
    clippy::invalid_upcast_comparisons,
    clippy::large_types_passed_by_value,
    clippy::let_unit_value,
    clippy::linkedlist,
    clippy::lossy_float_literal,
    clippy::macro_use_imports,
    clippy::manual_ok_or,
    clippy::map_err_ignore,
    clippy::map_flatten,
    clippy::map_unwrap_or,
    clippy::match_on_vec_items,
    clippy::match_same_arms,
    clippy::match_wildcard_for_single_variants,
    clippy::mem_forget,
    clippy::mismatched_target_os,
    clippy::mut_mut,
    clippy::mutex_integer,
    clippy::needless_borrow,
    clippy::needless_continue,
    clippy::option_option,
    clippy::path_buf_push_overwrite,
    clippy::ptr_as_ptr,
    clippy::ref_option_ref,
    clippy::rest_pat_in_fully_bound_structs,
    clippy::same_functions_in_if_condition,
    clippy::semicolon_if_nothing_returned,
    clippy::string_add_assign,
    clippy::string_add,
    clippy::string_lit_as_bytes,
    clippy::string_to_string,
    clippy::todo,
    clippy::trait_duplication_in_bounds,
    clippy::unimplemented,
    clippy::unnested_or_patterns,
    clippy::unused_self,
    clippy::useless_transmute,
    clippy::verbose_file_reads,
    clippy::zero_sized_map_values,
    future_incompatible,
    nonstandard_style,
    rust_2018_idioms
)]
// END - Embark standard lints v0.4
#![allow(clippy::exit)]

use anyhow::{anyhow, bail, Context, Error};
use buildkite_jobify::{jobifier::Jobifier, monitor::Monitor, scheduler::Scheduler};
use serde::Deserialize;
use std::path::PathBuf;
use structopt::StructOpt;
use tracing::error;
use tracing_subscriber::filter::LevelFilter;

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Config {
    /// The API token used for communicating with the Buildkite API, **must** have GraphQL enabled
    api_token: Option<String>,
    /// The namespace under which kubernetes jobs are created
    namespace: Option<String>,
    /// The list of pipeline identifiers within the organization to watch
    pipelines: Vec<String>,
    /// Cluster filter, only agents specified with the same cluster will be spawned
    cluster: Option<String>,
}

fn parse_level(s: &str) -> Result<LevelFilter, Error> {
    s.parse::<LevelFilter>()
        .with_context(|| format!("failed to parse level '{}'", s))
}

#[derive(StructOpt)]
#[structopt(name = "jobify")]
struct Opts {
    /// Path to a configuration file
    #[structopt(short, long, parse(from_os_str))]
    config: Option<PathBuf>,
    /// The API token used for communicating with the Buildkite API, **must** have GraphQL enabled.
    /// If not specified, the value is taken from the configuration file,
    /// or the `BUILDKITE_API_TOKEN` environment variable
    #[structopt(short = "t", long)]
    api_token: Option<String>,
    /// The namespace under which kubernetes jobs are created. Defaults to "buildkite".
    #[structopt(short, long)]
    namespace: Option<String>,
    /// Optional cluster identifier, if present, only jobs tagged with the same
    /// cluster identifier will be scheduled by this instance
    #[structopt(long)]
    cluster: Option<String>,
    #[structopt(
        short = "L",
        long,
        default_value = "info",
        parse(try_from_str = parse_level),
        long_help = "The log level for messages, only log messages at or above the level will be emitted.

Possible values:
* off
* error
* warn
* info (default)
* debug
* trace"
    )]
    log_level: LevelFilter,
    /// Output log messages as json
    #[structopt(long)]
    json: bool,
    /// The pipeline identifiers to watch for builds
    #[structopt(name = "PIPELINE")]
    pipelines: Vec<String>,
}

async fn real_main() -> Result<(), Error> {
    let args = Opts::from_args();

    let mut env_filter = tracing_subscriber::EnvFilter::from_default_env();

    // If a user specifies a log level, we assume it only pertains to cargo_fetcher,
    // if they want to trace other crates they can use the RUST_LOG env approach
    env_filter = env_filter.add_directive(args.log_level.into());

    let subscriber = tracing_subscriber::FmtSubscriber::builder().with_env_filter(env_filter);

    if args.json {
        tracing::subscriber::set_global_default(subscriber.json().finish())
            .context("failed to set default subscriber")?;
    } else {
        tracing::subscriber::set_global_default(subscriber.finish())
            .context("failed to set default subscriber")?;
    };

    // The tokio wrapper macro only supports unit returns at the moment,
    // so wrap it up in a function instead
    let get_cfg = || {
        let mut cfg: Config = match args.config {
            Some(ref path) => {
                let contents = std::fs::read_to_string(&path).map_err(|e| {
                    anyhow!(
                        "failed to read configuration from {}: {}",
                        path.display(),
                        e
                    )
                })?;

                toml::from_str(&contents).map_err(|e| {
                    anyhow!(
                        "failed to deserialize configuration from {}: {}",
                        path.display(),
                        e
                    )
                })?
            }
            None => Config {
                api_token: None,
                namespace: None,
                cluster: None,
                pipelines: Vec::new(),
            },
        };

        if let Some(token) = args.api_token {
            cfg.api_token = Some(token);
        }

        if let Some(ns) = args.namespace {
            cfg.namespace = Some(ns);
        }

        if let Some(cluster) = args.cluster {
            cfg.cluster = Some(cluster);
        }

        if !args.pipelines.is_empty() {
            // Could also extend, but probably doesn't make sense
            cfg.pipelines = args.pipelines;
        }

        let api_token = match cfg.api_token {
            Some(tok) => tok,
            None => match std::env::var("BUILDKITE_API_TOKEN") {
                Ok(tok) => tok,
                Err(e) => {
                    bail!(
                        "failed to read BUILDKITE_API_TOKEN environment variable: {}",
                        e
                    );
                }
            },
        };

        if cfg.pipelines.is_empty() {
            bail!("no pipelines were specified to monitor");
        }

        Ok((
            api_token,
            cfg.namespace.unwrap_or_else(|| "buildkite".to_owned()),
            cfg.pipelines,
            cfg.cluster,
        ))
    };

    let (token, namespace, pipelines, cluster) = match get_cfg() {
        Ok(cfg) => cfg,
        Err(e) => {
            error!("{}", e);
            std::process::exit(1);
        }
    };

    tracing::info!("namespace: {} cluster: {:?}", namespace, cluster);

    let start: Result<_, Error> = async {
        let monitor = Monitor::with_token(token.clone())
            .await
            .context("buildkite org monitor")?;
        let jobifier = Jobifier::create(token, namespace, cluster).context("k8s jobifier")?;
        let scheduler = Scheduler::new(monitor, jobifier);

        for pipeline in pipelines {
            scheduler.watch(&pipeline).await.context("pipeline watch")?;
        }

        Ok(scheduler)
    }
    .await;

    let scheduler = match start {
        Ok(s) => s,
        Err(e) => {
            error!("{:#}", e);
            std::process::exit(1);
        }
    };

    scheduler.wait().await;
    Ok(())
}

#[tokio::main]
async fn main() {
    match real_main().await {
        Ok(_) => {}
        Err(e) => {
            tracing::error!("{:#}", e);
            std::process::exit(1);
        }
    }
}
