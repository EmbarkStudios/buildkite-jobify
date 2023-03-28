use anyhow::{anyhow, bail, Context as _, Error};
use buildkite_jobify::{jobifier::Jobifier, monitor::Monitor, scheduler::Scheduler};
use camino::Utf8PathBuf as PathBuf;
use clap::Parser;
use serde::Deserialize;
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

#[derive(Parser)]
#[clap(name = "jobify")]
struct Opts {
    /// Path to a configuration file
    #[clap(short, long, action)]
    config: Option<PathBuf>,
    /// The API token used for communicating with the Buildkite API, **must** have GraphQL enabled.
    /// If not specified, the value is taken from the configuration file,
    /// or the `BUILDKITE_API_TOKEN` environment variable
    #[clap(short = 't', long, action)]
    api_token: Option<String>,
    /// The namespace under which kubernetes jobs are created. Defaults to "buildkite".
    #[clap(short, long, action)]
    namespace: Option<String>,
    /// Optional cluster identifier, if present, only jobs tagged with the same
    /// cluster identifier will be scheduled by this instance
    #[clap(long, action)]
    cluster: Option<String>,
    #[structopt(
        short = 'L',
        long,
        default_value = "info",
        value_parser = parse_level,
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
    #[clap(long, action)]
    json: bool,
    /// The pipeline identifiers to watch for builds
    #[clap(name = "PIPELINE", action)]
    pipelines: Vec<String>,
}

async fn real_main() -> Result<(), Error> {
    let args = Opts::parse();

    let mut env_filter = tracing_subscriber::EnvFilter::from_default_env();
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
                let contents = std::fs::read_to_string(path)
                    .map_err(|e| anyhow!("failed to read configuration from {path}: {e}"))?;

                toml::from_str(&contents)
                    .map_err(|e| anyhow!("failed to deserialize configuration from {path}: {e}"))?
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

    let (token, namespace, pipelines, cluster) = get_cfg()?;

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

    let scheduler = start?;
    scheduler.wait().await;
    Ok(())
}

#[tokio::main]
async fn main() {
    match real_main().await {
        Ok(_) => {}
        Err(e) => {
            tracing::error!("{e:#}");
            #[allow(clippy::exit)]
            std::process::exit(1);
        }
    }
}
