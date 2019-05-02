use crate::monitor;
use failure::{Error, ResultExt};
use futures::{
    channel::mpsc,
    compat::Future01CompatExt,
    future::{FutureExt, TryFutureExt},
};
use graphql_client::{GraphQLQuery, Response};
use k8s_openapi::api::{
    //    apimachinery::pkg::apis::meta::v1 as meta,
    batch::v1 as batch,
    core::v1 as core,
};
use kubernetes::{client::APIClient, config};
use log::{error, info, warn};
use reqwest::r#async::Client;
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};
use tokio::await;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "buildkite/schema.json",
    query_path = "buildkite/artifact.gql",
    response_derives = "Debug"
)]
struct GetArtifact;

pub struct Jobifier {
    sender: mpsc::UnboundedSender<monitor::Builds>,
}

impl Jobifier {
    pub fn create(buildkite_token: String) -> Result<Jobifier, Error> {
        let kubeconfig = config::load_kube_config()?;
        let kbctl = APIClient::new(kubeconfig);

        let (tx, rx) = mpsc::unbounded();

        let spawner = async move {
            await!(jobify(kbctl, buildkite_token, rx));
        };

        let spawner = spawner.unit_error().boxed().compat();

        tokio::spawn(spawner);

        Ok(Self { sender: tx })
    }

    pub fn queue(&self) -> mpsc::UnboundedSender<monitor::Builds> {
        self.sender.clone()
    }
}

/// Retrieves the agent configuration from a buildkite artifact
async fn get_jobify_config<'a>(
    client: &'a Client,   // The HTTP client to use
    bk_token: &'a str,    // The buildkite API token we use to query the artifact
    chksum: &'a str,      // The sha-1 checksum to check against
    artifact_id: &'a str, // The UUID of the artifact
) -> Result<Config, Error> {
    use std::io::Read;
    use tokio::prelude::StreamAsyncExt;

    info!("acquiring jobify artifact information {}", artifact_id);

    let req_body = GetArtifact::build_query(get_artifact::Variables {
        id: artifact_id.to_owned(),
    });

    // Attempt to retrieve the artifact details from Buildkite
    let mut res = await!(client
        .post(crate::BK_API_URL)
        .bearer_auth(bk_token)
        .json(&req_body)
        .send())?;

    let res_body: Response<get_artifact::ResponseData> = await!(res.json())?;

    let artifact = res_body
        .data
        .and_then(|rd| rd.artifact)
        .ok_or_else(|| format_err!("Failed to find artifact {}", artifact_id))?;

    // Just sanity check that the artifact's stated checksum is the same as
    // was specified in the build's metadata
    if artifact.sha1sum != chksum {
        return Err(format_err!("Checksum mismatch"));
    }

    info!("acquiring jobify artifact {}", artifact_id);

    let res = await!(client.get(&artifact.download_url).send().compat())?;

    info!("deserializing jobify configuration");

    // Just assume gzipped tar for now
    let decompressed = {
        let mut body = Vec::with_capacity(res.content_length().unwrap_or(1024) as usize);
        let mut res_body = res.into_body();

        while let Some(chunk) = await!(res_body.next()) {
            let chunk = chunk?;
            body.extend_from_slice(&chunk[..])
        }

        smush::decode(&body, smush::Encoding::Gzip)?
    };

    let files = {
        let cursor = std::io::Cursor::new(decompressed);
        let mut archive = tar::Archive::new(cursor);

        let mut files = BTreeMap::new();

        for mut file in archive.entries()?.filter_map(|f| f.ok()) {
            match file.header().entry_type() {
                tar::EntryType::Regular => {
                    let mut s =
                        String::with_capacity(file.header().size().unwrap_or(1024) as usize);
                    if file.read_to_string(&mut s).is_ok() {
                        let path = file.header().path().context("failed to get path in tar")?;
                        // Strip off leading ./ since they just add noise
                        let path = PathBuf::from(match path.strip_prefix("./") {
                            Ok(p) => std::borrow::Cow::Borrowed(p),
                            _ => path,
                        });

                        files.insert(path, s);
                    }
                }
                _ => {}
            }
        }

        files
    };

    // We require an agents.toml file at minimum to map agent tags to job specs
    let agents = files
        .get(Path::new("agents.toml"))
        .ok_or_else(|| format_err!("'agents.toml' was not in artifact {}", artifact_id))?;

    let agents: JobifyAgents = toml::from_str(&agents)?;

    // Attempt to deserialize a Job (spec, kinda) from the agents that have one
    let mut specs = BTreeMap::new();
    for (name, path) in agents
        .agents
        .iter()
        .filter_map(|a| a.kubernetes.as_ref().and_then(|k| Some((&a.name, k))))
    {
        match files.get(path) {
            Some(cfg) => {
                let spec: batch::Job = serde_yaml::from_str(cfg).map_err(|e| {
                    format_err!(
                        "failed to deserialize job spec for in {}: {}",
                        path.display(),
                        e
                    )
                })?;

                specs.insert(path.to_owned(), spec);
            }
            None => warn!(
                "agent {} points to missing k8s config {}",
                name,
                path.display()
            ),
        }
    }

    Ok(Config {
        agents,
        k8s_specs: specs,
    })
}

struct JobInfo<'a> {
    bk_job: &'a monitor::Job,
    pipeline: &'a str,
    org: &'a str,
    agent: &'a Agent,
}

impl<'a> JobInfo<'a> {
    fn name(&self) -> String {
        format!("{}-{}", self.agent.name, self.bk_job.uuid)
    }
}

async fn spawn_job<'a>(
    kbctl: &'a APIClient,
    job: &'a JobInfo<'a>,
    spec: &'a batch::Job,
) -> Result<Option<batch::JobStatus>, Error> {
    let mut k8_job = spec.clone();

    // Make a unique name for the job/agent
    let agent_name = job.name();

    let labels = {
        let mut labels = BTreeMap::new();
        labels.insert("bk-jobify".to_owned(), "true".to_owned());
        labels.insert("bk-org".to_owned(), job.org.to_owned());
        labels.insert("bk-pipeline".to_owned(), job.pipeline.to_owned());
        labels
    };

    if let Some(ref mut job_spec) = k8_job.spec {
        // Setup naming/labeling
        {
            if job_spec.template.metadata.is_none() {
                job_spec.template.metadata = Some(Default::default())
            }

            if let Some(ref mut md) = job_spec.template.metadata {
                md.name = Some(agent_name.clone());

                if md.labels.is_none() {
                    md.labels = Some(Default::default());
                }

                if let Some(ref mut l) = md.labels {
                    for (k, v) in &labels {
                        match l.get_mut(k) {
                            Some(old) => *old = v.to_string(),
                            None => {
                                l.insert(k.to_string(), v.to_string());
                            }
                        }
                    }
                }
            }
        }

        {
            let spec = &mut job_spec.template.spec;

            if let Some(containers) = spec.as_mut().map(|s| &mut s.containers) {
                // We only expect one container, if there's more than that just don't make any changes
                if containers.len() == 1 {
                    let container = &mut containers[0];

                    // Set the agent tags to match those specified in the jobify configuration
                    if container.env.is_none() {
                        container.env = Some(Vec::new());
                    }

                    if let Some(env) = container.env.as_mut() {
                        let tags = match env.iter().position(|e| e.name == "BUILDKITE_AGENT_TAGS") {
                            Some(i) => &mut env[i],
                            None => {
                                env.push(core::EnvVar {
                                    name: "BUILDKITE_AGENT_TAGS".to_owned(),
                                    value: None,
                                    value_from: None,
                                });

                                let i = env.len() - 1;
                                &mut env[i]
                            }
                        };

                        // Make a single value for the env var, eg "somekey=somevalue,otherkey=othervalue"
                        tags.value = Some(job.agent.tags());

                        // Make the agent name the same as the k8s Job name
                        let agent_name_var =
                            match env.iter().position(|e| e.name == "BUILDKITE_AGENT_NAME") {
                                Some(i) => &mut env[i],
                                None => {
                                    env.push(core::EnvVar {
                                        name: "BUILDKITE_AGENT_NAME".to_owned(),
                                        value: None,
                                        value_from: None,
                                    });

                                    let i = env.len() - 1;
                                    &mut env[i]
                                }
                            };

                        agent_name_var.value = Some(agent_name.clone());
                    }
                }
            }
        }
    }

    {
        if k8_job.metadata.is_none() {
            k8_job.metadata = Some(Default::default())
        }

        if let Some(ref mut md) = k8_job.metadata {
            md.name = Some(agent_name.clone());

            if md.labels.is_none() {
                md.labels = Some(Default::default());
            }

            if let Some(ref mut l) = md.labels {
                for (k, v) in &labels {
                    match l.get_mut(k) {
                        Some(old) => *old = v.to_string(),
                        None => {
                            l.insert(k.to_string(), v.to_string());
                        }
                    }
                }
            }
        }
    }

    // Construct the job creation request
    let (req, _) = batch::Job::create_namespaced_job("buildkite", &k8_job, Default::default())
        .map_err(|e| format_err!("failed to create request: {}", e))?;

    // Make the request TODO: handle errors...
    let job = await!(kbctl.request::<batch::Job>(req))
        .map_err(|e| format_err!("request failed: {}", e))?;

    Ok(job.status)
}

/// Retrieves the status of the named job
async fn get_job_status<'a>(
    kbctl: &'a APIClient,
    job: &'a JobInfo<'a>,
) -> Result<batch::JobStatus, Error> {
    let agent_name = job.name();

    let (req, _) =
        batch::Job::read_namespaced_job_status(&agent_name, "buildkite", Default::default())
            .map_err(|e| format_err!("request creation failed: {}", e))?;

    await!(kbctl.request::<batch::JobStatus>(req))
}

fn get_best_agent<'a>(
    cfg: &'a Config,
    job: &monitor::Job,
) -> Result<(&'a Agent, &'a batch::Job), Error> {
    info!("attempting to find agent for {}({})", job.uuid, job.label);

    // Find a spec that matches the job
    let mut best = None;
    let mut score: u32 = 0;

    for agent in &cfg.agents.agents {
        let mut s = 0;
        for (k, v) in job.iter_query_rules() {
            match (v, agent.tags.get(k)) {
                (Some(v), Some(av)) => {
                    // Increase the score for each exactly
                    // matching kv pair
                    if av == v {
                        s += 1;
                    } else {
                        // A key with a different value is
                        // an automatic failure
                        s = 0;
                        break;
                    }
                }
                // This means the job specified a wildcard
                // match, so it doesn't matter what the value
                // of the tag is
                (None, Some(_av)) => {
                    s += 1;
                }
                (_, None) => {
                    s = 0;
                    break;
                }
            }
        }

        if s > score {
            score = s;
            best = Some(agent);
        }
    }

    let agent = best.ok_or_else(|| {
        format_err!(
            "no suitable agent could be found for job {}({})",
            job.uuid,
            job.label
        )
    })?;

    let spec_path = agent.kubernetes.as_ref().ok_or_else(|| {
        format_err!(
            "agent {} can't run {}({}), no k8s spec specified",
            agent.name,
            job.uuid,
            job.label
        )
    })?;
    let spec = cfg.k8s_specs.get(spec_path).ok_or_else(|| {
        format_err!(
            "agent {} points to missing spec {}, can't assign job {}({})",
            agent.name,
            spec_path.display(),
            job.uuid,
            job.label
        )
    })?;

    Ok((agent, spec))
}

async fn jobify(
    kbctl: APIClient,
    bk_token: String,
    mut rx: mpsc::UnboundedReceiver<monitor::Builds>,
) {
    use futures::StreamExt;

    let mut known_jobs: HashMap<String, Option<batch::JobStatus>> = HashMap::new();
    let mut configs: lru_time_cache::LruCache<String, Option<Config>> =
        lru_time_cache::LruCache::with_capacity(10);
    let client = Client::new();

    while let Some(builds) = await!(rx.next()) {
        // For each build, ensure that we get the agent configuration to use
        // for it via a tarball generated from the same commit as the build
        for build in &builds.builds {
            if let Some(chksum) = build.metadata.get("jobify-artifact-chksum") {
                if build
                    .jobs
                    .iter()
                    .filter(|j| j.state == monitor::JobStates::Scheduled)
                    .count()
                    > 0
                {
                    if !configs.contains_key(chksum) {
                        if let Some(artifact_id) = build.metadata.get("jobify-artifact-id") {
                            let val = match await!(get_jobify_config(
                                &client,
                                &bk_token,
                                chksum,
                                artifact_id
                            )) {
                                Ok(cfg) => Some(cfg),
                                Err(err) => {
                                    // If we fail to find the artifact-id we just assume
                                    // that something went wrong and mark the config
                                    // as invalid and stop checking
                                    error!(
                                        "failed to get jobify configuration for build {}: {}",
                                        build.uuid, err
                                    );
                                    None
                                }
                            };

                            configs.insert(chksum.to_owned(), val);
                        }
                    }
                }
            }
        }

        for build in &builds.builds {
            if let Some(chksum) = build.metadata.get("jobify-artifact-chksum") {
                for job in build
                    .jobs
                    .iter()
                    .filter(|j| j.state == monitor::JobStates::Scheduled)
                {
                    if known_jobs.contains_key(&job.uuid) {
                        continue;
                    }

                    match configs.get(chksum).and_then(|cfg| cfg.as_ref()) {
                        Some(cfg) => {
                            match get_best_agent(cfg, &job) {
                                Ok((agent, spec)) => {
                                    let job_info = JobInfo {
                                        bk_job: &job,
                                        pipeline: &builds.pipeline,
                                        org: &builds.org,
                                        agent,
                                    };

                                    // Check kubernetes to see if the job already exists
                                    // TODO: Check for watches
                                    match await!(get_job_status(&kbctl, &job_info)) {
                                        Err(e) => {
                                            warn!("failed to get job status: {}", e);
                                        }
                                        Ok(status) => {
                                            // Have we started the job?
                                            if status.start_time.is_some()
                                                && status.completion_time.is_none()
                                            {
                                                info!(
                                                    "job {} already exists for {}({}), status {:#?}",
                                                    job_info.name(),
                                                    job.label,
                                                    job.uuid,
                                                    status,
                                                );

                                                match known_jobs.get_mut(&job.uuid) {
                                                    Some(v) => *v = Some(status),
                                                    None => {
                                                        known_jobs
                                                            .insert(job.uuid.clone(), Some(status));
                                                    }
                                                }

                                                continue;
                                            }
                                        }
                                    }

                                    match await!(spawn_job(&kbctl, &job_info, &spec)) {
                                        Ok(k8_job) => {
                                            info!(
                                                "spawned job {} for {}({})",
                                                job_info.name(),
                                                job.label,
                                                job.uuid,
                                            );

                                            known_jobs.insert(job.uuid.clone(), k8_job);
                                        }
                                        Err(e) => {
                                            warn!("failed to spawn job for {}: {}", job.label, e);
                                        }
                                    }
                                }
                                Err(e) => {
                                    warn!("{}", e);
                                }
                            }
                        }
                        None => {
                            warn!(
                                "unable to assign job {}({}), no known agent configuration",
                                job.uuid, job.label
                            );
                        }
                    };
                }
            }
        }

        // TODO: Run separate task to cleanup finished kubernetes jobs

        // Remove any jobs that are no longer running nor scheduled
        // known_jobs.retain(|k, _| {
        //     jobs.jobs
        //         .iter()
        //         .filter(|j| {
        //             j.state == monitor::JobStates::Running
        //                 || j.state == monitor::JobStates::Scheduled
        //         })
        //         .find(|j| &j.uuid == k)
        //         .is_some()
        // });
    }
}

struct Config {
    agents: JobifyAgents,
    k8s_specs: BTreeMap<PathBuf, batch::Job>,
}

#[derive(Debug, Deserialize, Serialize)]
struct JobifyAgents {
    agents: Vec<Agent>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Agent {
    name: String,
    kubernetes: Option<PathBuf>,
    // TODO: VM
    tags: BTreeMap<String, String>,
}

impl Agent {
    fn tags(&self) -> String {
        let mut tags_str = String::with_capacity(
            self.tags
                .iter()
                .map(|(k, v)| k.len() + v.len() + 1)
                .sum::<usize>()
                + self.tags.len()
                - 1,
        );

        for (i, (k, v)) in self.tags.iter().enumerate() {
            if i > 0 {
                tags_str.push(',');
            }
            tags_str.push_str(&k);
            tags_str.push('=');
            tags_str.push_str(&v);
        }

        tags_str
    }
}
