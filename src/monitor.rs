//use failure::{Error, ResultExt};
//use futures::executor::ThreadPool;
use tokio_threadpool::ThreadPool;
use graphql_client::{GraphQLQuery, Response};
use log::{info, warn, error};
use serde::{de::DeserializeOwned, Serialize};

const API_URL: &str = "https://graphql.buildkite.com/v1";

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
    query_path = "buildkite/builds.gql",
    response_derives = "Debug"
)]
struct FindPipeline;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "buildkite/schema.json",
    query_path = "buildkite/builds.gql",
    response_derives = "Debug"
)]
struct GetBuilds;

#[derive(Fail, Debug)]
pub enum BkErr {
    // #[fail(display = "Input was invalid UTF-8 at index {}", _0)]
    // Utf8Error(usize),
    #[fail(display = "Invalid header value specified {}", _0)]
    Http(#[fail(cause)] http::header::InvalidHeaderValue),
    #[fail(display = "Reqwest failure {}", _0)]
    Reqwest(#[fail(cause)] reqwest::Error),
    // #[fail(display = "{}", _0)]
    // Io(#[fail(cause)] io::Error),
    #[fail(display = "Failed to deserialize value {}", _0)]
    Serde(#[fail(cause)] serde::de::value::Error),
    #[fail(display = "Failed to find organization {}", _0)]
    UnknownOrg(String),
    #[fail(display = "Invalid organization id {}", _0)]
    InvalidOrgId(String),
    #[fail(display = "Failed to find pipeline {}", _0)]
    UnknownPipeline(String),
    #[fail(display = "Failed to create threadpool {}", _0)]
    Threadpool(#[fail(cause)] std::io::Error),
}

impl From<reqwest::Error> for BkErr {
    fn from(e: reqwest::Error) -> Self {
        BkErr::Reqwest(e)
    }
}

fn send_request<Req: Serialize, Res: DeserializeOwned>(
    client: &reqwest::Client,
    req: &Req,
) -> Result<Option<Res>, BkErr> {
    let mut res = client.post(API_URL).json(req).send()?;

    let res: Response<Res> = res.json().map_err(|e| BkErr::from(e))?;

    // TODO: Errors!

    Ok(res.data)
}

#[derive(PartialEq)]
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

impl From<&get_builds::BuildStates> for BuildStates {
    fn from(bs: &get_builds::BuildStates) -> Self {
        use get_builds::BuildStates as BS;

        match *bs {
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

#[derive(PartialEq)]
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

impl From<&get_builds::JobStates> for JobStates {
    fn from(js: &get_builds::JobStates) -> Self {
        use get_builds::JobStates as JS;

        match *js {
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

pub struct Job {
    /// UUID
    pub id: String,
    /// The exit code for the job, if it has finished execution
    pub exit_status: Option<i32>,
    /// The current state of the job
    pub state: JobStates,
    /// The agent query rules used to determine which agents
    /// can pick this job up
    query_rules: String,
}

impl Job {
    pub fn iter_query_rules(&self) -> impl Iterator<Item = (&str, &str)> {
        self.query_rules.split(',')
            .filter_map(|rule| {
                let mut i = rule.splitn(2, '=');

                match (i.next(), i.next()) {
                    (Some(k), Some(v)) => Some((k, v)),
                    _ => None,
                }
            })
    }
}

pub struct Build {
    /// UUID
    pub id: String,
    /// Current state of the build
    pub state: BuildStates,
    /// Sequence number
    pub number: i64,
    /// The list of jobs in the build
    pub jobs: Vec<Job>,
}

pub struct Builds(pub Vec<Build>);

// Monitors Buildkite's GraphQL API for events of interest
pub struct Monitor {
    // The organization identifier, this can be specified exactly or queried
    // from Buildkite during initialization
    org_id: String,
    client: reqwest::Client,
    //pool: ThreadPool,
}

impl Monitor {
    pub fn with_org_id(token: String, id: String) -> Result<Monitor, BkErr> {
        use http::{header::AUTHORIZATION, HeaderMap};
        let mut headers = HeaderMap::new();

        // Every Buildkite request requires auth so just make it a default
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {}", token)
                .parse::<http::header::HeaderValue>()
                .map_err(BkErr::Http)?,
        );

        let client = reqwest::ClientBuilder::new()
            .default_headers(headers)
            .build()?;

        // Verify the id is correct and we have access to it
        let req_body = OrgAccess::build_query(org_access::Variables { id: id.clone() });

        let res_body: Option<org_access::ResponseData> = send_request(&client, &req_body)?;

        // Just verify the ids match as a sanity check, the operation
        // completing without an error is enough to know we can
        // continue making API calls
        let res_id = res_body
            .and_then(|rd| rd.node)
            .ok_or_else(|| BkErr::InvalidOrgId(id.clone()))?;

        if res_id.id != id {
            return Err(BkErr::InvalidOrgId(id));
        }

        match res_id.on {
            org_access::OrgAccessNodeOn::Organization(org) => {
                info!(
                    "created monitor for organization {}({})",
                    org.slug, res_id.id
                );
            }
            _ => return Err(BkErr::InvalidOrgId(id)),
        }

        Ok(Self {
            org_id: id,
            client,
            //pool: ThreadPool::new(),
        })
    }

    pub fn with_org_slug(token: String, slug: &str) -> Result<Monitor, BkErr> {
        let req_body = OrgId::build_query(org_id::Variables {
            slug: slug.to_owned(),
        });

        let client = reqwest::Client::new();
        let mut res = client
            .post(API_URL)
            .bearer_auth(&token)
            .json(&req_body)
            .send()?;

        let res_body: Response<org_id::ResponseData> = res.json()?;

        let id = res_body
            .data
            .and_then(|rd| rd.organization)
            .ok_or_else(|| BkErr::UnknownOrg(slug.to_owned()))?;

        info!("resolved slug '{}' to id '{}'", slug, id.id);

        Self::with_org_id(token, id.id)
    }

    pub fn watch(&self, pipeline: &str) -> Result<futures::channel::mpsc::Receiver<Builds>, BkErr> {
        use futures::future::{FutureExt, TryFutureExt};
        use tokio::prelude::StreamAsyncExt;

        let req_body = FindPipeline::build_query(find_pipeline::Variables {
            org: self.org_id.clone(),
            name: pipeline.to_owned(),
        });

        let res_body: Option<find_pipeline::ResponseData> = send_request(&self.client, &req_body)?;

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

        info!(
            "watching pipeline {}({}) '{}'",
            pipeline_name,
            pipeline_id,
            pipeline_description.unwrap_or_default()
        );

        let (mut tx, rx) = futures::channel::mpsc::channel(100);

        let client = self.client.clone();

        let future03 = async move {
            // Currently we just poll aggressively, a better option would be to add
            // a webhook for the `job.scheduled` event that invokes a cloud function, which
            // then posts a cloud pubsub message that we listen to instead as it should
            // be much lower cost for everyone involved, at the expense of additional
            // complexity
            let mut interval =
                tokio_timer::Interval::new_interval(std::time::Duration::from_secs(1));

            let mut failure_count = 0u8;

            while let Some(_) = await!(interval.next()) {
                let req_body = GetBuilds::build_query(get_builds::Variables {
                    id: pipeline_id.clone(),
                });

                let res_body: Option<get_builds::ResponseData> = match send_request(&client, &req_body) {
                    Ok(rb) => {
                        failure_count = 0;
                        rb
                    }
                    Err(err) => {
                        failure_count += 1;

                        if failure_count >= 5 {
                            error!("stopping GetBuilds query for {}, too many failures: {}", pipeline_name, err);
                            break;
                        } else {
                            warn!("failed to send GetBuilds query {}: {}", failure_count, err);
                            continue;
                        }
                    }
                };

                let builds = match res_body.and_then(|root|
                    if let Some(node) = root.node {
                        match node.on {
                            get_builds::GetBuildsNodeOn::Pipeline(pipeline) => pipeline.builds,
                            _ => None,
                        }
                    } else {
                        None
                    }
                ).and_then(|builds| {
                    if let Some(mut edges) = builds.edges {
                        let mut builds = Vec::with_capacity(builds.count as usize);

                        for build in edges.iter().filter_map(|n| n.as_ref().and_then(|n| n.node.as_ref())) {
                            
                            let jobs = match build.jobs {
                                Some(ref node) => {
                                    match node.edges {
                                        Some(ref job_edges) => {
                                            let mut jobs = Vec::with_capacity(node.count as usize);

                                            for job in job_edges.iter().filter_map(|n| n.as_ref().and_then(|n| n.node.as_ref())) {
                                                match job {
                                                    get_builds::GetBuildsNodeOnPipelineBuildsEdgesNodeJobsEdgesNode::JobTypeCommand(job) => {
                                                        jobs.push(Job {
                                                            id: job.id.clone(),
                                                            state: (&job.state).into(),
                                                            query_rules: job.agent_query_rules.as_ref().map(|v| v.join(",")).unwrap_or_default(),
                                                            exit_status: job.exit_status.as_ref().and_then(|s| s.parse::<i32>().ok()),
                                                        });
                                                    }
                                                    get_builds::GetBuildsNodeOnPipelineBuildsEdgesNodeJobsEdgesNode::JobTypeWait(job) => {
                                                        jobs.push(Job {
                                                            id: job.id.clone(),
                                                            state: (&job.state).into(),
                                                            query_rules: String::new(),
                                                            exit_status: None,
                                                        });
                                                    }
                                                    _ => {}
                                                }
                                                
                                            }

                                            jobs
                                        },
                                        None => continue,
                                    }
                                }
                                None => continue,
                            };

                            builds.push(Build {
                                id: build.id.clone(),
                                state: (&build.state).into(), // The GraphQL state enum is not Copy :(
                                number: build.number,
                                jobs,
                            });
                        }

                        if !builds.is_empty() {
                            Some(builds)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }) {
                    Some(builds) => Builds(builds),
                    None => continue,
                };

                // Try to send to our channel, if it fails it means the receiver
                // was dropped and we can stop polling
                if tx.try_send(builds).is_err() {
                    info!(
                        "stopped watching pipeline {}({})",
                        pipeline_name,
                        pipeline_id,
                    );
                    break;
                }
            }
        };

        // let future01 = future03
        //     .unit_error()
        //     .boxed()
        //     .compat();

        tokio::spawn_async(future03);
        //self.pool.spawn(future01);

        Ok(rx)
    }
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