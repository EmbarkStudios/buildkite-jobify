#![feature(await_macro, async_await)]

use buildkite_jobify::{jobifier::Jobifier, monitor::Monitor, scheduler::Scheduler};
use failure::{bail, format_err, Error};
use log::error;
use serde_derive::Deserialize;
use std::path::PathBuf;
use structopt::StructOpt;
use structopt_flags::LogLevel;
use tokio::await as async_wait;

#[derive(Deserialize)]
struct Config {
    /// The slug of the organization to watch
    organization: Option<String>,
    /// The API token used for communicating with the Buildkite API, **must** have GraphQL enabled
    api_token: Option<String>,
    /// The namespace under which kubernetes jobs are created
    namespace: Option<String>,
    /// The list of pipelines within the organization to watch
    pipelines: Vec<String>,
}

#[derive(StructOpt)]
#[structopt(name = "jobify")]
struct Opts {
    #[structopt(flatten)]
    log_level: structopt_flags::LogLevelOpt,
    /// Path to a configuration file
    #[structopt(short = "c", long = "config", parse(from_os_str))]
    config: Option<PathBuf>,
    /// The organization slug to watch
    #[structopt(short = "o", long = "org")]
    org: Option<String>,
    /// The API token used for communicating with the Buildkite API, **must** have GraphQL enabled.
    /// If not specified, the value is taken from the configuration file,
    /// or the `BUILDKITE_API_TOKEN` environment variable
    #[structopt(short = "t", long = "api-token")]
    api_token: Option<String>,
    /// The namespace under which kubernetes jobs are created. Defaults to "buildkite".
    #[structopt(short = "n", long = "namespace")]
    namespace: Option<String>,
    /// The pipeline(s) to watch for builds
    #[structopt(name = "PIPELINE")]
    pipelines: Vec<String>,
}

#[tokio::main]
async fn main() {
    let args = Opts::from_args();

    env_logger::builder()
        .filter_level(args.log_level.get_level_filter())
        .init();

    // The tokio wrapper macro only supports unit returns at the moment,
    // so wrap it up in a function instead
    let get_cfg = || {
        let mut cfg: Config = match args.config {
            Some(ref path) => {
                let contents = std::fs::read_to_string(&path).map_err(|e| {
                    format_err!(
                        "failed to read configuration from {}: {}",
                        path.display(),
                        e
                    )
                })?;

                toml::from_str(&contents).map_err(|e| {
                    format_err!(
                        "failed to deserialize configuration from {}: {}",
                        path.display(),
                        e
                    )
                })?
            }
            None => Config {
                organization: None,
                api_token: None,
                namespace: None,
                pipelines: Vec::new(),
            },
        };

        if let Some(o) = args.org {
            cfg.organization = Some(o);
        }

        if let Some(token) = args.api_token {
            cfg.api_token = Some(token);
        }

        if let Some(ns) = args.namespace {
            cfg.namespace = Some(ns);
        }

        if !args.pipelines.is_empty() {
            // Could also extend, but probably doesn't make sense
            cfg.pipelines = args.pipelines;
        }

        let org = cfg
            .organization
            .ok_or_else(|| format_err!("no organization slug was provided"))?;
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
            bail!("no pipelines were provided");
        }

        Ok((org, api_token, cfg.namespace.unwrap_or_else(|| "buildkite".to_owned()), cfg.pipelines))
    };

    let (org, token, namespace, pipelines) = match get_cfg() {
        Ok(cfg) => cfg,
        Err(e) => {
            error!("{}", e);
            std::process::exit(1);
        }
    };

    let start: Result<_, Error> = async_wait!(async {
        let monitor = async_wait!(Monitor::with_org_slug(token.clone(), &org,))
            .map_err(|e| format_err!("failed to create buildkite organization monitor: {}", e))?;
        let jobifier = Jobifier::create(token, namespace)
            .map_err(|e| format_err!("failed to create k8s jobifier: {}", e))?;
        let scheduler = Scheduler::new(monitor, jobifier);

        for pipeline in pipelines {
            async_wait!(scheduler.watch(&pipeline))
                .map_err(|e| format_err!("failed to watch pipeline '{}': {}", pipeline, e))?;
        }

        Ok(scheduler)
    });

    let scheduler = match start {
        Ok(s) => s,
        Err(e) => {
            error!("{}", e);
            std::process::exit(1);
        }
    };

    async_wait!(scheduler.wait());
}
