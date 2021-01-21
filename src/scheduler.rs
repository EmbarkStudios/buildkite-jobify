use crate::{
    jobifier::Jobifier,
    monitor::{BkErr, Monitor},
};
use futures::{channel::mpsc, sink::SinkExt, stream::StreamExt};
use std::fmt;

pub struct Scheduler {
    monitor: Monitor,
    jobifier: Jobifier,
    waiter: (mpsc::UnboundedSender<()>, mpsc::UnboundedReceiver<()>),
}

#[derive(Debug)]
pub enum Error {
    Buildkite(BkErr),
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Buildkite(s) => Some(s),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Buildkite(bk) => write!(f, "{}", bk),
        }
    }
}

impl Scheduler {
    pub fn new(monitor: Monitor, jobifier: Jobifier) -> Self {
        Self {
            monitor,
            jobifier,
            waiter: mpsc::unbounded(),
        }
    }

    pub async fn watch<'a>(&'a self, pipeline_slug: &'a str) -> Result<(), Error> {
        let mut channel = self
            .monitor
            .watch(pipeline_slug)
            .await
            .map_err(Error::Buildkite)?;
        let mut enqueue = self.jobifier.queue();
        let mut waiter = self.waiter.0.clone();

        let schedule = async move {
            while let Some(builds) = channel.next().await {
                if enqueue.send(builds).await.is_err() {
                    break;
                }
            }

            let _ = waiter.send(()).await;
        };

        tokio::spawn(schedule);

        Ok(())
    }

    pub async fn wait(mut self) {
        while self.waiter.1.next().await.is_some() {}
    }
}
