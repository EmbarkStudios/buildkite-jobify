//use failure::{Error, ResultExt};
use futures::{
    compat::{Future01CompatExt, Stream01CompatExt},
    stream::StreamExt,
};
use graphql_client::{GraphQLQuery, Response};
use log::{error, info, warn};
use reqwest::r#async::Client;
use serde::{de::DeserializeOwned, Serialize};
use std::{collections::BTreeMap, fmt};
use tokio::await as async_wait;
use uuid::Uuid;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "buildkite/schema.json",
    query_path = "buildkite/org.gql",
    response_derives = "Debug"
)]
struct OrgId;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "buildkite/schema.json",
    query_path = "buildkite/org.gql",
    response_derives = "Debug"
)]
struct OrgAccess;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "buildkite/schema.json",
    query_path = "buildkite/jobs.gql",
    response_derives = "Debug"
)]
struct FindPipeline;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "buildkite/schema.json",
    query_path = "buildkite/jobs.gql",
    response_derives = "Debug"
)]
struct GetBuilds;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "buildkite/schema.json",
    query_path = "buildkite/jobs.gql",
    response_derives = "Debug"
)]
struct GetRunningBuilds;

#[derive(Fail, Debug)]
pub enum BkErr {
    #[fail(display = "Invalid header value specified {}", _0)]
    InvalidHttpHeader(#[fail(cause)] http::header::InvalidHeaderValue),
    #[fail(display = "HTTP error: {}", _0)]
    Http(reqwest::Error),
    #[fail(display = "Reqwest failure {}", _0)]
    Reqwest(#[fail(cause)] reqwest::Error),
    #[fail(display = "Failed to deserialize json value {}", _0)]
    Json(#[fail(cause)] serde_json::error::Error),
    #[fail(display = "Failed to find organization {}", _0)]
    UnknownOrg(String),
    #[fail(display = "Invalid organization id {}", _0)]
    InvalidOrgId(String),
    #[fail(display = "Failed to find pipeline {}", _0)]
    UnknownPipeline(String),
    #[fail(display = "Failed to create threadpool {}", _0)]
    Threadpool(#[fail(cause)] std::io::Error),
    #[fail(display = "Request error(s) {}", _0)]
    Request(#[fail(cause)] GraphQLErrors),
    #[fail(display = "Invalid response: {}", _0)]
    InvalidResponse(String),
}

impl From<reqwest::Error> for BkErr {
    fn from(e: reqwest::Error) -> Self {
        BkErr::Reqwest(e)
    }
}

#[derive(Fail, Debug)]
pub struct GraphQLErrors {
    errors: Vec<graphql_client::Error>,
}

impl fmt::Display for GraphQLErrors {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_list().entries(self.errors.iter()).finish()
    }
}

async fn send_request<'a, Req: Serialize, Res: DeserializeOwned + std::marker::Unpin + std::fmt::Debug>(
    client: &'a Client,
    req: &'a Req,
    hash: Option<u64>,
) -> Result<(Option<Res>, Option<u64>), BkErr> {
    use std::hash::Hasher;
    use tokio::prelude::StreamAsyncExt;

    let res = async_wait!(client.post(crate::BK_API_URL)
        .json(req).send())?;

    let res = res.error_for_status().map_err(BkErr::Http)?;

    let content_type = res.headers().get(http::header::CONTENT_TYPE);
    if content_type != Some(&http::header::HeaderValue::from_static("application/json; charset=utf-8")) {
        return Err(BkErr::InvalidResponse(format!("invalid content-type: {:?}", content_type)));
    }

    let mut json_body = Vec::with_capacity(res.content_length().unwrap_or(1024) as usize);
    let mut res_body = res.into_body();

    while let Some(chunk) = async_wait!(res_body.next()) {
        let chunk = chunk.map_err(|e| BkErr::from(e))?;
        json_body.extend_from_slice(&chunk[..])
    }

    let new_hash = {
        let mut hasher = twox_hash::XxHash::default();
        hasher.write(&json_body);
        Some(hasher.finish())
    };

    if hash.is_some() && new_hash == hash {
        return Ok((None, new_hash));
    }

    let res: Response<Res> = serde_json::from_slice(&json_body).map_err(|e| {
        error!("json error for response: {:?}", String::from_utf8(json_body));
        BkErr::Json(e)
    })?;

    if let Some(errs) = res.errors {
        return Err(BkErr::Request(GraphQLErrors { errors: errs }));
    }

    Ok((res.data, new_hash))
}

macro_rules! parse_uuid {
    ($uuid_str:expr) => {
        match Uuid::parse_str($uuid_str) {
            Ok(u) => u,
            Err(err) => {
                error!("failed to parse Buildkite UUID '{}': {}", $uuid_str, err);
                return None;
            }
        };
    };
}

#[derive(Debug, PartialEq)]
pub enum BuildStates {
    /// The build was skipped
    Skipped,
    /// The build has yet to start running jobs
    Scheduled,
    /// The build is currently running jobs
    Running,
    /// The build passed
    Passed,
    /// The build failed
    Failed,
    /// The build is currently being canceled
    Canceling,
    /// The build was canceled
    Canceled,
    /// The build is blocked
    Blocked,
    /// The build wasn't run
    NotRun,
    /// Unknown build state
    Unknown,
}

impl From<get_builds::BuildStates> for BuildStates {
    fn from(bs: get_builds::BuildStates) -> Self {
        use get_builds::BuildStates as BS;

        match bs {
            BS::SKIPPED => BuildStates::Skipped,
            BS::SCHEDULED => BuildStates::Scheduled,
            BS::RUNNING => BuildStates::Running,
            BS::PASSED => BuildStates::Passed,
            BS::FAILED => BuildStates::Failed,
            BS::CANCELING => BuildStates::Canceling,
            BS::CANCELED => BuildStates::Canceled,
            BS::BLOCKED => BuildStates::Blocked,
            BS::NOT_RUN => BuildStates::NotRun,
            _ => BuildStates::Unknown,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum JobStates {
    /// The job has just been created and doesn't have a state yet
    Pending,
    /// The job is waiting on a `wait` step to finish
    Waiting,
    /// The job was in a `Waiting` state when the build failed
    WaitingFailed,
    /// The job is waiting on a `block` step to finish
    Blocked,
    /// The job was in a `Blocked` state when the build failed
    BlockedFailed,
    /// This `block` job has been manually unblocked
    Unblocked,
    /// This `block` job was in a `Blocked` state when the build failed
    UnblockedFailed,
    /// The job is waiting for jobs with the same concurrency group to finish
    Limited,
    /// The job is scheduled and waiting for an agent
    Scheduled,
    /// The job has been assigned to an agent, and it's waiting for it to accept
    Assigned,
    /// The job was accepted by the agent, and now it's waiting to start running
    Accepted,
    /// The job is runing
    Running,
    /// The job has finished
    Finished,
    /// The job is currently canceling
    Canceling,
    /// The job was canceled
    Canceled,
    /// The job is timing out for taking too long
    TimingOut,
    /// The job timed out
    TimedOut,
    /// The job was skipped
    Skipped,
    /// The jobs configuration means that it can't be run
    Broken,
    /// An unknown job state
    Unknown,
}

impl From<get_builds::JobStates> for JobStates {
    fn from(js: get_builds::JobStates) -> Self {
        use get_builds::JobStates as JS;

        match js {
            JS::PENDING => JobStates::Pending,
            JS::WAITING => JobStates::Waiting,
            JS::WAITING_FAILED => JobStates::WaitingFailed,
            JS::BLOCKED => JobStates::Blocked,
            JS::BLOCKED_FAILED => JobStates::BlockedFailed,
            JS::UNBLOCKED => JobStates::Unblocked,
            JS::UNBLOCKED_FAILED => JobStates::UnblockedFailed,
            JS::LIMITED => JobStates::Limited,
            JS::SCHEDULED => JobStates::Scheduled,
            JS::ASSIGNED => JobStates::Assigned,
            JS::ACCEPTED => JobStates::Accepted,
            JS::RUNNING => JobStates::Running,
            JS::FINISHED => JobStates::Finished,
            JS::CANCELING => JobStates::Canceling,
            JS::CANCELED => JobStates::Canceled,
            JS::TIMING_OUT => JobStates::TimingOut,
            JS::TIMED_OUT => JobStates::TimedOut,
            JS::SKIPPED => JobStates::Skipped,
            JS::BROKEN => JobStates::Broken,
            _ => JobStates::Unknown,
        }
    }
}

#[derive(Debug)]
pub struct Job {
    /// Unique identifier for the job
    pub uuid: Uuid,
    /// The job's friendly identifier
    pub label: String,
    /// The containing build's UUID
    pub build_uuid: Uuid,
    /// The exit code for the job, if it has finished execution
    pub exit_status: Option<i32>,
    /// The current state of the job
    pub state: JobStates,
    /// The agent query rules used to determine which agents
    /// can pick this job up
    query_rules: String,
    /// The name of the agent the job was assigned to
    pub agent: Option<String>,
}

impl Job {
    pub fn iter_query_rules(&self) -> impl Iterator<Item = (&str, Option<&str>)> {
        self.query_rules.split(',').filter_map(|rule| {
            let mut i = rule.splitn(2, '=');

            match (i.next(), i.next()) {
                (Some(k), Some(v)) => {
                    if v.is_empty() || v == "*" {
                        Some((k, None))
                    } else {
                        Some((k, Some(v)))
                    }
                }
                _ => None,
            }
        })
    }
}

#[derive(Debug)]
pub struct Build {
    /// Unique identifier for the build
    pub uuid: Uuid,
    /// Metadata attached to the build
    pub metadata: BTreeMap<String, String>,
    /// The commit message that triggered the build
    pub message: Option<String>,
    /// The commit identifier the build pertains to
    pub commit: String,
    /// The current state of the build
    pub state: BuildStates,
    /// The list of currently known jobs in the build
    pub jobs: Vec<Job>,
}

impl std::cmp::PartialOrd for Build {
    fn partial_cmp(&self, other: &Build) -> Option<std::cmp::Ordering> {
        Some(self.uuid.cmp(&other.uuid))
    }
}

impl std::cmp::Ord for Build {
    fn cmp(&self, other: &Build) -> std::cmp::Ordering {
        self.uuid.cmp(&other.uuid)
    }
}

impl std::cmp::PartialEq for Build {
    fn eq(&self, other: &Build) -> bool {
        self.uuid == other.uuid
    }
}

impl std::cmp::Eq for Build {}

//#[derive(Clone)]
pub struct Builds {
    pub builds: Vec<Build>,
    pub pipeline: String,
    pub org: String,
}

// Monitors Buildkite's GraphQL API for events of interest
pub struct Monitor {
    // The organization identifier, this can be specified exactly or queried
    // from Buildkite during initialization
    org_id: String,
    org_slug: String,
    client: Client,
}

#[derive(Clone)]
struct KnownBuild {
    uuid: Uuid,
    commit: String,
}

impl std::cmp::PartialOrd for KnownBuild {
    fn partial_cmp(&self, other: &KnownBuild) -> Option<std::cmp::Ordering> {
        Some(self.uuid.cmp(&other.uuid))
    }
}

impl std::cmp::Ord for KnownBuild {
    fn cmp(&self, other: &KnownBuild) -> std::cmp::Ordering {
        self.uuid.cmp(&other.uuid)
    }
}

impl std::cmp::PartialEq for KnownBuild {
    fn eq(&self, other: &KnownBuild) -> bool {
        self.uuid == other.uuid
    }
}

impl std::cmp::Eq for KnownBuild {}

impl Monitor {
    pub async fn with_org_id(token: String, id: String) -> Result<Monitor, BkErr> {
        use http::{header::AUTHORIZATION, HeaderMap};
        let mut headers = HeaderMap::new();

        // Every Buildkite request requires auth so just make it a default
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {}", token)
                .parse::<http::header::HeaderValue>()
                .map_err(BkErr::InvalidHttpHeader)?,
        );

        let client = reqwest::r#async::ClientBuilder::new()
            .default_headers(headers)
            .build()?;

        // Verify the id is correct and we have access to it
        let req_body = OrgAccess::build_query(org_access::Variables { id: id.clone() });

        let (res_body, _) = async_wait!(send_request::<_, org_access::ResponseData>(&client, &req_body, None))?;

        // Just verify the ids match as a sanity check, the operation
        // completing without an error is enough to know we can
        // continue making API calls
        let res_id = res_body
            .and_then(|rd| rd.node)
            .ok_or_else(|| BkErr::InvalidOrgId(id.clone()))?;

        if res_id.id != id {
            return Err(BkErr::InvalidOrgId(id));
        }

        let slug = match res_id.on {
            org_access::OrgAccessNodeOn::Organization(org) => {
                info!(
                    "created monitor for organization {}({})",
                    org.slug, res_id.id
                );

                org.slug
            }
            _ => return Err(BkErr::InvalidOrgId(id)),
        };

        Ok(Self {
            org_id: id,
            org_slug: slug,
            client,
        })
    }

    pub async fn with_org_slug(token: String, slug: &str) -> Result<Monitor, BkErr> {
        let req_body = OrgId::build_query(org_id::Variables {
            slug: slug.to_owned(),
        });

        let client = Client::new();
        let mut res = async_wait!(client
            .post(crate::BK_API_URL)
            .bearer_auth(&token)
            .json(&req_body)
            .send())?;

        let res_body: Response<org_id::ResponseData> = async_wait!(res.json())?;

        let id = res_body
            .data
            .and_then(|rd| rd.organization)
            .ok_or_else(|| BkErr::UnknownOrg(slug.to_owned()))?;

        info!("resolved slug '{}' to id '{}'", slug, id.id);

        async_wait!(Self::with_org_id(token, id.id))
    }

    pub fn client(&self) -> Client {
        self.client.clone()
    }

    pub async fn watch<'a>(
        &'a self,
        pipeline: &'a str,
    ) -> Result<futures::channel::mpsc::Receiver<Builds>, BkErr> {
        use futures::future::{FutureExt, TryFutureExt};
        use tokio::prelude::StreamAsyncExt;

        let req_body = FindPipeline::build_query(find_pipeline::Variables {
            org: self.org_id.clone(),
            name: pipeline.to_owned(),
        });

        let (res_body, _) =
            async_wait!(send_request::<_, find_pipeline::ResponseData>(&self.client, &req_body, None))?;

        let pipeline = res_body
            .and_then(|root| {
                if let Some(node) = root.node {
                    match node.on {
                        find_pipeline::FindPipelineNodeOn::Organization(org) => org.pipelines,
                        _ => None,
                    }
                } else {
                    None
                }
            })
            .and_then(|pipelines| {
                if let Some(mut edges) = pipelines.edges {
                    let index = edges.iter().position(|edge| {
                        if let Some(edge) = edge {
                            if let Some(ref node) = edge.node {
                                // Return the first pipeline that exactly
                                // matches the specified name
                                return node.name == pipeline;
                            }
                        }

                        false
                    });

                    index.map(|i| edges.swap_remove(i).unwrap())
                } else {
                    None
                }
            })
            .and_then(|edge| edge.node)
            .ok_or_else(|| BkErr::UnknownPipeline(pipeline.to_owned()))?;

        let crate::monitor::find_pipeline::FindPipelineNodeOnOrganizationPipelinesEdgesNode {
            name: pipeline_name,
            id: pipeline_id,
            description: pipeline_description,
            ..
        } = pipeline;

        let (mut tx, rx) = futures::channel::mpsc::channel(100);

        let client = self.client.clone();
        let org_slug = self.org_slug.clone();

        let poll = async move {
            info!(
                "watching pipeline {}({}) '{}'",
                pipeline_name,
                pipeline_id,
                pipeline_description.unwrap_or_default()
            );

            // Currently we just poll aggressively, a better option would be to add
            // a webhook for the `job.scheduled` event that invokes a cloud function, which
            // then posts a cloud pubsub message that we listen to instead as it should
            // be much lower cost for everyone involved, at the expense of additional
            // complexity
            let mut interval =
                tokio_timer::Interval::new_interval(std::time::Duration::from_secs(1));

            let mut failure_count = 0u8;
            let mut query_hash = None;
            let mut known_builds = Vec::new();
            const MAX_FAILURES: u8 = 100;

            while let Some(_) = async_wait!(interval.next()) {
                let builds = match async_wait!(get_builds(&client, &pipeline_id, &mut known_builds, &mut query_hash)) {
                    Ok(builds) => {
                        if failure_count > 0 {
                            info!("successfully sent query after {} failures", failure_count);
                            failure_count = 0;
                        }

                        match builds {
                            Some(builds) => Builds {
                                builds,
                                pipeline: pipeline_name.clone(),
                                org: org_slug.clone(),
                            },
                            None => continue,
                        }
                    }
                    Err(err) => {
                        failure_count += 1;

                        if failure_count >= MAX_FAILURES {
                            error!(
                                "stopping query for {}, too many failures: {}",
                                pipeline_name, err
                            );
                            break;
                        } else {
                            warn!("failed to send query {}: {}", failure_count, err);
                            continue;
                        }
                    }
                };

                // Try to send to our channel, if it fails it means the receiver
                // was dropped and we can stop polling
                if tx.try_send(builds).is_err() {
                    break;
                }
            }

            info!(
                "stopped watching pipeline {}({})",
                pipeline_name, pipeline_id,
            );
        };

        let poll = poll.unit_error().boxed().compat();

        tokio::spawn(poll);

        Ok(rx)
    }
}

async fn get_builds<'a>(
    client: &'a Client,
    pipeline_id: &'a String,
    known_builds: &'a mut Vec<KnownBuild>,
    query_hash: &'a mut Option<u64>,
) -> Result<Option<Vec<Build>>, BkErr> {
    let req_body = GetRunningBuilds::build_query(get_running_builds::Variables {
        id: pipeline_id.clone(),
    });

    let (res_body, _hash) = async_wait!(send_request::<_, get_running_builds::ResponseData>(&client, &req_body, None))?;

    let builds_to_check = res_body
        .and_then(|root| root.node)
        .and_then(|node| {
            match node.on {
                get_running_builds::GetRunningBuildsNodeOn::Pipeline(pipeline) => pipeline.builds,
                _ => None,
            }
        })
        .and_then(|in_builds| in_builds.edges)
        .and_then(|builds| {
            let builds: Vec<_> = builds
                .into_iter()
                .filter_map(|n| n.and_then(|n| n.node))
                .filter_map(|build| {
                    Some(KnownBuild {
                        uuid: parse_uuid!(&build.uuid),
                        commit: build.commit,
                    })
                }).collect();

            Some(builds)
        });

    let commits: Vec<_> = known_builds.iter().map(|kb| kb.commit.clone())
        .chain(builds_to_check.unwrap_or_else(|| Vec::new()).into_iter().map(|kb| kb.commit)).collect();

    let req_body = GetBuilds::build_query(get_builds::Variables {
        id: pipeline_id.clone(),
        commits: Some(commits)
    });

    let (res_body, hash) = async_wait!(send_request::<_, get_builds::ResponseData>(&client, &req_body, *query_hash))?;

    // Don't bother sending an update to jobifier if the Buildkite state hasn't changed
    if *query_hash == hash {
        return Ok(None);
    }

    *query_hash = hash;

    let builds = res_body
        .and_then(|root| {
            if let Some(node) = root.node {
                match node.on {
                    get_builds::GetBuildsNodeOn::Pipeline(pipeline) => pipeline.builds,
                    _ => None,
                }
            } else {
                None
            }
        })
        .and_then(|in_builds| in_builds.edges)
        .and_then(|builds| {
            let mut builds: Vec<_> = builds
                .into_iter()
                .filter_map(|n| n.and_then(|n| n.node))
                .filter_map(|build| {
                    let uuid = parse_uuid!(&build.uuid);

                    let jobs: Option<Vec<_>> = build.jobs.and_then(|jn| jn.edges)
                        .map(|jobs| {
                            jobs.into_iter()
                                .filter_map(|e| e.and_then(|n| n.node))
                                .filter_map(|job| {
                                    match job {
                                        get_builds::GetBuildsNodeOnPipelineBuildsEdgesNodeJobsEdgesNode::JobTypeCommand(cmd) => {
                                            Some(Job {
                                                uuid: parse_uuid!(&cmd.uuid),
                                                build_uuid: uuid.clone(),
                                                label: cmd.label.unwrap_or_else(|| "<unlabeled>".to_owned()),
                                                state: cmd.state.into(),
                                                query_rules: cmd.agent_query_rules.as_ref().map(|v| v.join(",")).unwrap_or_default(),
                                                exit_status: cmd.exit_status.and_then(|es| es.parse::<i32>().ok()),
                                                agent: cmd.agent.and_then(|a| a.name),
                                            })
                                        }
                                        _ => None
                                    }
                                }).collect()
                        });

                    let jobs = match jobs {
                        Some(ref j) if j.is_empty() => return None,
                        None => return None,
                        Some(j) => j,
                    };

                    // Fill out the metadata for the build, if we've been triggered by another build
                    // add its metadata first and then override them if they're also set on this build
                    let metadata = {
                        let mut meta = BTreeMap::new();

                        if let Some(md) = build.triggered_from.and_then(|tf| tf.build.and_then(|b| b.meta_data.and_then(|md| md.edges))) {
                            meta.extend(md.into_iter().filter_map(|e| {
                                e.and_then(|e| e.node.and_then(|kv| Some((kv.key, kv.value))))
                            }));
                        }

                        if let Some(md) = build.meta_data.and_then(|md| md.edges) {
                            meta.extend(md.into_iter().filter_map(|e| {
                                e.and_then(|e| e.node.and_then(|kv| Some((kv.key, kv.value))))
                            }));
                        }

                        meta
                    };

                    Some(Build {
                        jobs,
                        metadata,
                        message: build.message,
                        commit: build.commit,
                        uuid: uuid,
                        state: build.state.into(),
                    })
                })
                .collect();

            builds.sort();

            if !builds.is_empty() {
                Some(builds)
            } else {
                None
            }
        });

    known_builds.clear();

    if let Some(ref builds) = builds {
        known_builds.extend(builds.iter().filter_map(|b| {
            match b.state {
                BuildStates::Running => Some(KnownBuild {
                    uuid: b.uuid.clone(),
                    commit: b.commit.clone(),
                }),
                _ => None,
            }
        }))
    }

    Ok(builds)
}

#[cfg(test)]
mod test {
    use super::{Job, JobStates};

    #[test]
    fn iters_agent_rules() {
        let job = Job {
            id: "".to_owned(),
            exit_status: None,
            state: JobStates::Running,
            query_rules: "queue=something,os=linux,blah=thing".to_owned(),
        };

        let mut iter = job.iter_query_rules();

        assert_eq!(iter.next(), Some(("queue", "something")));
        assert_eq!(iter.next(), Some(("os", "linux")));
        assert_eq!(iter.next(), Some(("blah", "thing")));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn iters_empty_agent_rules() {
        let job = Job {
            id: "".to_owned(),
            exit_status: None,
            state: JobStates::Running,
            query_rules: String::new(),
        };

        let mut iter = job.iter_query_rules();
        assert_eq!(iter.next(), None);
    }
}
