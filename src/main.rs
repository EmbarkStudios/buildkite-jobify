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
    /// The slug of the organization to watch
    organization: Option<String>,
    /// The API token used for communicating with the Buildkite API, **must** have GraphQL enabled
    api_token: Option<String>,
    /// The namespace under which kubernetes jobs are created
    namespace: Option<String>,
    /// The list of pipelines slugs within the organization to watch
    pipeline_slugs: Vec<String>,
}

fn parse_level(s: &str) -> Result<LevelFilter, Error> {
    s.parse::<LevelFilter>()
        .map_err(|_| anyhow!("failed to parse level '{}'", s))
}

#[derive(StructOpt)]
#[structopt(name = "jobify")]
struct Opts {
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
    #[structopt(
        short = "L",
        long = "log-level",
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
    /// The pipeline slug(s) to watch for builds
    #[structopt(name = "PIPELINE")]
    pipelines: Vec<String>,
}

async fn real_main() -> Result<(), Error> {
    let args = Opts::from_args();

    let mut env_filter = tracing_subscriber::EnvFilter::from_default_env();

    // If a user specifies a log level, we assume it only pertains to cargo_fetcher,
    // if they want to trace other crates they can use the RUST_LOG env approach
    env_filter = env_filter.add_directive(args.log_level.clone().into());

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
                organization: None,
                api_token: None,
                namespace: None,
                pipeline_slugs: Vec::new(),
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
            cfg.pipeline_slugs = args.pipelines;
        }

        let org = cfg
            .organization
            .context("no organization slug was provided")?;

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

        if cfg.pipeline_slugs.is_empty() {
            bail!("no pipelines were specified to monitor");
        }

        Ok((
            org,
            api_token,
            cfg.namespace.unwrap_or_else(|| "buildkite".to_owned()),
            cfg.pipeline_slugs,
        ))
    };

    let (org, token, namespace, pipelines) = match get_cfg() {
        Ok(cfg) => cfg,
        Err(e) => {
            error!("{}", e);
            std::process::exit(1);
        }
    };

    let start: Result<_, Error> = async {
        let monitor = Monitor::with_org_slug(token.clone(), &org)
            .await
            .context("buildkite org monitor")?;
        let jobifier = Jobifier::create(token, namespace).context("k8s jobifier")?;
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
