use crate::k8s::{config, APIClient};
use crate::monitor;
use anyhow::{Context, Error};
use futures::{channel::mpsc, stream::StreamExt};
use graphql_client::{GraphQLQuery, Response};
use k8s_openapi::{
    api::{batch::v1 as batch, core::v1 as core},
    List, ListOptional,
};
use reqwest::Client;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
};
use tracing::{error, info, warn};
use uuid::Uuid;

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
    pub fn create(
        buildkite_token: String,
        namespace: String,
        cluster: Option<String>,
    ) -> Result<Jobifier, Error> {
        let kubeconfig = match config::load_kube_config() {
            Ok(cfg) => cfg,
            Err(_) => config::incluster_config()?,
        };

        let kbctl = APIClient::new(kubeconfig);

        let (tx, rx) = mpsc::unbounded();

        let spawner = async move {
            jobify(kbctl, buildkite_token, namespace, cluster, rx).await;
        };

        tokio::spawn(spawner);

        Ok(Self { sender: tx })
    }

    pub fn queue(&self) -> mpsc::UnboundedSender<monitor::Builds> {
        self.sender.clone()
    }
}

/// Retrieves the agent configuration from a buildkite artifact
async fn get_jobify_config<'a>(
    client: &'a Client,       // The HTTP client to use
    bk_token: &'a str,        // The buildkite API token we use to query the artifact
    chksum: &'a str,          // The sha-1 checksum to check against
    artifact_id: &'a str,     // The UUID of the artifact
    cluster: Option<&'a str>, // Optional cluster filter to only spawn jobs assigned to the cluster
) -> Result<Config, Error> {
    use std::io::Read;

    info!("acquiring jobify artifact information {}", artifact_id);

    let req_body = GetArtifact::build_query(get_artifact::Variables {
        id: artifact_id.to_owned(),
    });

    // Attempt to retrieve the artifact details from Buildkite
    let res = client
        .post(crate::BK_API_URL)
        .bearer_auth(bk_token)
        .json(&req_body)
        .send()
        .await?;

    let res_body: Response<get_artifact::ResponseData> = res.json().await?;

    let artifact = res_body
        .data
        .and_then(|rd| rd.artifact)
        .context("failed to find artifact id")?;

    // Just sanity check that the artifact's stated checksum is the same as
    // was specified in the build's metadata
    if artifact.sha1sum != chksum {
        anyhow::bail!("checksum mismatch");
    }

    info!("acquiring jobify artifact {}", artifact_id);

    let res = client.get(&artifact.download_url).send().await?;

    info!("deserializing jobify configuration");
    let res_body = res.bytes().await?;

    // Just assume gzipped tar for now
    let files = {
        use bytes::buf::Buf;
        let reader = res_body.reader();

        let decoder = flate2::read::GzDecoder::new(reader);
        let mut archive = tar::Archive::new(decoder);

        let mut files = BTreeMap::new();

        for mut file in archive.entries()?.filter_map(|f| f.ok()) {
            if let tar::EntryType::Regular = file.header().entry_type() {
                let mut s = String::with_capacity(file.header().size().unwrap_or(1024) as usize);
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
        }

        files
    };

    // We require an agents.toml file at minimum to map agent tags to job specs
    let agents = files
        .get(Path::new("agents.toml"))
        .context("'agents.toml' was not in artifact")?;

    let agents: JobifyAgents = toml::from_str(&agents)?;

    // Attempt to deserialize a Job (spec, kinda) from the agents that have one
    let mut specs = BTreeMap::new();
    for (name, path) in agents.agents.iter().filter_map(|a| {
        if a.cluster.as_deref() != cluster {
            None
        } else {
            a.kubernetes.as_ref().map(|k| (&a.name, k))
        }
    }) {
        match files.get(path) {
            Some(cfg) => {
                let spec: batch::Job = serde_yaml::from_str(cfg).map_err(|e| {
                    anyhow::anyhow!(
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
    /// The buildkite job description
    bk_job: &'a monitor::Job,
    /// The slug for the pipeline the job is part of
    pipeline: &'a str,
    /// The slug for the organization the pipeline is a part of
    org: &'a str,
    /// The agent description that will execute the job
    agent: &'a Agent,
}

async fn spawn_job<'a>(
    kbctl: &'a APIClient,
    nfo: &'a JobInfo<'a>,
    spec: &'a batch::Job,
    namespace: &'a str,
) -> Result<(String, Option<batch::JobStatus>), Error> {
    let agent_name = format!("{}-{}", nfo.agent.name, nfo.bk_job.uuid);

    let labels = {
        let mut labels = BTreeMap::new();
        labels.insert("bk-jobify".to_owned(), "true".to_owned());
        labels.insert("bk-org".to_owned(), nfo.org.to_owned());
        labels.insert("bk-pipeline".to_owned(), nfo.pipeline.to_owned());
        labels
    };

    let mut k8_job = spec.clone();

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
                        tags.value = Some(nfo.agent.tags());

                        // Make the agent name the same as the k8s Job name, so that we can pair
                        // the job to it correctly
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

                    // This should never be the case, but...just in case
                    if container.args.is_none() {
                        container.args = Some(vec![
                            "start".to_owned(),
                            "--disconnect-after-job".to_owned(),
                        ]);
                    }

                    // Modify the arguments to include the bk job id that we want to run in this k8s job
                    if let Some(args) = container.args.as_mut() {
                        args.push("--acquire-job".to_owned());
                        args.push(format!("{}", nfo.bk_job.uuid));
                    }
                }
            }
        }
    }

    {
        k8_job.metadata.name = Some(agent_name.clone());

        if k8_job.metadata.labels.is_none() {
            k8_job.metadata.labels = Some(Default::default());
        }

        if let Some(k8_labels) = &mut k8_job.metadata.labels {
            for (k, v) in &labels {
                match k8_labels.get_mut(k) {
                    Some(old) => *old = v.to_string(),
                    None => {
                        k8_labels.insert(k.to_string(), v.to_string());
                    }
                }
            }
        }
    }

    // Construct the job creation request
    let (req, _) = batch::Job::create_namespaced_job(namespace, &k8_job, Default::default())
        .context("create_namespaced_job")?;

    // Make the request TODO: handle errors...
    let job = kbctl
        .request::<batch::Job>(req)
        .await
        .context("create_namespaced_job")?;

    Ok((agent_name, job.status))
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
        anyhow::anyhow!(
            "no suitable agent could be found for job {}({})",
            job.uuid,
            job.label
        )
    })?;

    let spec_path = agent.kubernetes.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "agent {} can't run {}({}), no k8s spec specified",
            agent.name,
            job.uuid,
            job.label
        )
    })?;
    let spec = cfg.k8s_specs.get(spec_path).ok_or_else(|| {
        anyhow::anyhow!(
            "agent {} points to missing spec {}, can't assign job {}({})",
            agent.name,
            spec_path.display(),
            job.uuid,
            job.label
        )
    })?;

    Ok((agent, spec))
}

async fn cleanup_jobs(kbctl: APIClient, namespace: String, pipeline: String) -> Result<u32, Error> {
    // We just aggressively delete all pods that match our labels, instead
    // of more carefully only deleting pods for jobs that are known to be completed

    let label_selector = format!("bk-jobify=true,bk-pipeline={}", pipeline);

    // TODO: Use watches
    let (req, _) = core::Pod::list_namespaced_pod(
        &namespace,
        ListOptional {
            label_selector: Some(&label_selector),
            ..Default::default()
        },
    )
    .context("list_namespaced_pod")?;

    let pods = kbctl
        .request::<List<core::Pod>>(req)
        .await
        .context("list_namespaced_pod")?;

    let mut deleted = 0;

    for pod in pods.items {
        if let Some(statuses) = pod.status.and_then(|s| s.container_statuses) {
            for status in statuses {
                if status.state.and_then(|s| s.terminated).is_some() {
                    if let Some(agent_name) = pod
                        .metadata
                        .labels
                        .as_ref()
                        .and_then(|labels| labels.get("job-name"))
                    {
                        let (req, _) = match batch::Job::delete_namespaced_job(
                            &agent_name,
                            &namespace,
                            k8s_openapi::DeleteOptional {
                                propagation_policy: Some("Foreground"),
                                ..Default::default()
                            },
                        ) {
                            Ok(r) => r,
                            Err(e) => {
                                error!("failed to make delete request: {}", e);
                                continue;
                            }
                        };

                        match kbctl.request::<batch::Job>(req).await {
                            Ok(_) => {
                                info!("deleted agent {}", agent_name);
                                deleted += 1;
                            }
                            Err(e) => {
                                error!("failed to delete agent {}: {}", agent_name, e);
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(deleted)
}

async fn jobify(
    kbctl: APIClient,
    bk_token: String,
    namespace: String,
    cluster: Option<String>,
    mut rx: mpsc::UnboundedReceiver<monitor::Builds>,
) {
    let mut known_jobs: HashMap<Uuid, Option<String>> = HashMap::new();
    let mut pending_agents: HashSet<String> = HashSet::new();
    let mut configs: lru_time_cache::LruCache<String, Option<Config>> =
        lru_time_cache::LruCache::with_capacity(10);
    let client = Client::new();

    while let Some(builds) = rx.next().await {
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
                    && !configs.contains_key(chksum)
                {
                    if let Some(artifact_id) = build.metadata.get("jobify-artifact-id") {
                        let val = match get_jobify_config(
                            &client,
                            &bk_token,
                            chksum,
                            artifact_id,
                            cluster.as_deref(),
                        )
                        .await
                        {
                            Ok(cfg) => Some(cfg),
                            Err(err) => {
                                // If we fail to find the artifact-id we just assume
                                // that something went wrong and mark the config
                                // as invalid and stop checking

                                // TODO: Add this into a queue to retry later
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
                        Some(cfg) => match get_best_agent(cfg, &job) {
                            Ok((agent, spec)) => {
                                let job_info = JobInfo {
                                    bk_job: &job,
                                    pipeline: &builds.pipeline,
                                    org: &builds.org,
                                    agent,
                                };

                                match spawn_job(&kbctl, &job_info, &spec, &namespace).await {
                                    Ok((name, _k8_job_status)) => {
                                        info!(
                                            "spawned job {} for {}({})",
                                            name, job.label, job.uuid,
                                        );

                                        known_jobs.insert(job.uuid, None);
                                        pending_agents.insert(name);
                                    }
                                    Err(e) => {
                                        warn!(
                                            "failed to spawn job for {}({}): {:#}",
                                            job.uuid, job.label, e
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("{}", e);
                            }
                        },
                        None => {
                            // TODO: Add this into a queue to retry after we retry getting the configuration
                            warn!(
                                "unable to assign job {}({}), no known agent configuration",
                                job.uuid, job.label
                            );
                        }
                    };
                }
            }
        }

        // Pair jobs with agents that we spun up
        for build in &builds.builds {
            for (job_id, agent) in build
                .jobs
                .iter()
                .filter_map(|j| j.agent.as_ref().map(|an| (j.uuid, an)))
            {
                let agent = agent.clone();
                if pending_agents.remove(&agent) {
                    if let Some(v) = known_jobs.get_mut(&job_id) {
                        info!("job {} running on agent {}", job_id, agent);
                        *v = Some(agent);
                    }
                }
            }
        }

        let exited: Vec<_> = builds
            .builds
            .into_iter()
            .flat_map(|b| {
                b.jobs
                    .into_iter()
                    .filter_map(|j| j.exit_status.map(|es| (j.uuid, es)))
            })
            .collect();

        let mut k8s_needs_cleanup = 0;
        // For all of the jobs that have exited, if any are in our known list, attempt
        // a cleanup of any kubernetes jobs that we have created that have exited, as
        // kubernetes will leave them around and clutter things, even though it's not
        // an issue since we always name them with UUID's
        for (uuid, _status) in &exited {
            if let Some(_known) = known_jobs.remove(uuid).and_then(|kj| kj) {
                k8s_needs_cleanup += 1;
            }
        }

        if k8s_needs_cleanup > 0 {
            let client = kbctl.clone();
            let ns = namespace.clone();
            let pipeline = builds.pipeline;

            let cleanup = async move {
                match cleanup_jobs(client, ns, pipeline).await {
                    Ok(da) => {
                        info!("cleaned up {} kubernetes agents", da);
                    }
                    Err(e) => {
                        error!("failed during kubernetes job cleanup: {}", e);
                    }
                }
            };

            tokio::spawn(cleanup);
        }
    }
}

struct Config {
    agents: JobifyAgents,
    k8s_specs: BTreeMap<PathBuf, batch::Job>,
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
struct JobifyAgents {
    agents: Vec<Agent>,
    automatic_retry: Option<Vec<i32>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Agent {
    name: String,
    kubernetes: Option<PathBuf>,
    cluster: Option<String>,
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
