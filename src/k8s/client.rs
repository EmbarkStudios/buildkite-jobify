use anyhow::Error;
use serde::{de::DeserializeOwned, Deserialize};
use std::{fmt, sync::Arc};

use super::config::Configuration;

#[derive(Deserialize, Debug)]
pub struct ApiError {
    status: String,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    code: u16,
}

impl std::error::Error for ApiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#?}", self)
    }
}

/// [`APIClient`] requires [`config::Configuration`] includes client to connect
/// with kubernetes cluster.
#[derive(Clone)]
pub struct APIClient {
    configuration: Arc<Configuration>,
}

impl APIClient {
    pub fn new(configuration: Configuration) -> Self {
        APIClient {
            configuration: Arc::new(configuration),
        }
    }

    /// Returns kubernetes resources binded `Arnavion/k8s-openapi-codegen` APIs.
    pub async fn request<T>(&self, request: http::Request<Vec<u8>>) -> Result<T, Error>
    where
        T: DeserializeOwned,
    {
        let res = self.configuration.client(request).await?;

        // If an API error can't be deserialized from the error response's JSON body, use
        // the HTTP error as a fallback
        let fallback_err = res.error_for_status_ref().map(|_| ());
        let json_body = res.bytes().await?;

        match fallback_err {
            Ok(_) => serde_json::from_slice(&json_body).map_err(Error::from),
            Err(e) => match serde_json::from_slice::<ApiError>(&json_body) {
                Ok(api_err) => Err(api_err.into()),
                Err(_) => Err(e.into()),
            },
        }
    }
}
