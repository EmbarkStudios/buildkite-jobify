use crate::{
    jobifier::Jobifier,
    monitor::{BkErr, Monitor},
};
use futures::{
    channel::mpsc,
    future::{FutureExt, TryFutureExt},
    sink::SinkExt,
    stream::StreamExt,
};
use tokio::await;

pub struct Scheduler {
    monitor: Monitor,
    jobifier: Jobifier,
    waiter: (mpsc::UnboundedSender<()>, mpsc::UnboundedReceiver<()>),
}

#[derive(Fail, Debug)]
pub enum Error {
    #[fail(display = "Buildkite error {}", _0)]
    Buildkite(#[fail(cause)] BkErr),
}

impl Scheduler {
    pub fn new(monitor: Monitor, jobifier: Jobifier) -> Self {
        Self {
            monitor,
            jobifier,
            waiter: mpsc::unbounded(),
        }
    }

    pub async fn watch<'a>(&'a self, pipeline: &'a str) -> Result<(), Error> {
        let mut channel = await!(self.monitor.watch(pipeline)).map_err(Error::Buildkite)?;
        let mut enqueue = self.jobifier.queue();
        let mut waiter = self.waiter.0.clone();

        let schedule = async move {
            while let Some(builds) = await!(channel.next()) {
                if await!(enqueue.send(builds)).is_err() {
                    break;
                }
            }

            let _ = await!(waiter.send(()));
        };

        let schedule = schedule.unit_error().boxed().compat();

        tokio::spawn(schedule);

        Ok(())
    }

    pub async fn wait(mut self) {
        while let Some(_) = await!(self.waiter.1.next()) {}
    }
}
