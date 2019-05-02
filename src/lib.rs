#![feature(await_macro, async_await)]

#[macro_use]
extern crate failure;
#[macro_use]
extern crate serde_derive;

pub mod jobifier;
pub mod monitor;
pub mod scheduler;

const BK_API_URL: &str = "https://graphql.buildkite.com/v1";
