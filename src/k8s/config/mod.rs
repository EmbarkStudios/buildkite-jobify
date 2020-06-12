mod apis;
mod incluster;
mod kube_config;
mod utils;

use anyhow::{Context, Error};
use reqwest::{header, Certificate, Client, Identity, Response};
use std::sync::Arc;
use tame_oauth::gcp::{ServiceAccountAccess, ServiceAccountInfo, TokenOrRequest};

use self::kube_config::KubeConfigLoader;

/// Configuration stores kubernetes path and client for requests.
pub struct Configuration {
    base_path: String,
    client: Client,
    auth_provider: Option<AuthProvider>,
}

impl Configuration {
    pub(crate) fn new(
        base_path: String,
        client: Client,
        auth_provider: Option<AuthProvider>,
    ) -> Self {
        Configuration {
            base_path,
            client,
            auth_provider,
        }
    }

    pub(crate) async fn client(
        &self,
        mut request: http::Request<Vec<u8>>,
    ) -> Result<Response, Error> {
        let client = self.client.clone();

        if let Some(ref auth_provider) = self.auth_provider {
            let auth_value = auth_provider.get_auth_header(&client).await?;

            request
                .headers_mut()
                .insert(header::AUTHORIZATION, auth_value);
        }

        let (parts, body) = request.into_parts();
        let uri_str = format!("{}{}", self.base_path, parts.uri);

        let send = async move {
            let req_builder = match parts.method {
                http::Method::GET => client.get(&uri_str),
                http::Method::POST => client.post(&uri_str),
                http::Method::DELETE => client.delete(&uri_str),
                http::Method::PUT => client.put(&uri_str),
                _other => {
                    unreachable!();
                }
            };

            let req = req_builder.headers(parts.headers).body(body);

            Ok(req.send().await?)
        };

        send.await
    }
}

pub(crate) enum AuthProvider {
    //Basic(header::HeaderValue),
    Oauth2(Arc<ServiceAccountAccess>),
}

impl AuthProvider {
    // fn with_username_password(username: &str, password: &str) -> Result<AuthProvider, Error> {
    //     let encoded = base64::encode(&format!("{}:{}", username, password));
    //     let hv = header::HeaderValue::from_str(&format!("Basic {}", encoded))?;

    //     Ok(AuthProvider::Basic(hv))
    // }

    fn with_service_key(key: ServiceAccountInfo) -> Result<AuthProvider, Error> {
        let access = ServiceAccountAccess::new(key)?;
        Ok(AuthProvider::Oauth2(Arc::new(access)))
    }

    async fn get_auth_header<'a>(
        &'a self,
        client: &'a Client,
    ) -> Result<header::HeaderValue, Error> {
        match self {
            //AuthProvider::Basic(hv) => Ok(hv.clone()),
            AuthProvider::Oauth2(access) => {
                let token = match access
                    .get_token(&["https://www.googleapis.com/auth/cloud-platform"])?
                {
                    TokenOrRequest::Request {
                        request,
                        scope_hash,
                        ..
                    } => {
                        let (parts, body) = request.into_parts();

                        let uri = parts.uri.to_string();

                        let builder = match parts.method {
                            http::Method::GET => client.get(&uri),
                            http::Method::POST => client.post(&uri),
                            http::Method::DELETE => client.delete(&uri),
                            http::Method::PUT => client.put(&uri),
                            method => unimplemented!("{} not implemented", method),
                        };

                        let req = builder.headers(parts.headers).body(body).build()?;

                        let res = client.execute(req).await?;

                        let mut builder = http::Response::builder()
                            .status(res.status())
                            .version(res.version());

                        let headers = builder.headers_mut().context("invalid response headers")?;

                        headers.extend(
                            res.headers()
                                .into_iter()
                                .map(|(k, v)| (k.clone(), v.clone())),
                        );

                        let body = res.bytes().await?;

                        let response = builder.body(body)?;

                        access.parse_token_response(scope_hash, response)?
                    }
                    _ => unreachable!(),
                };

                use std::convert::TryInto;
                Ok(token.try_into()?)
            }
        }
    }
}

/// Returns a config includes authentication and cluster information from kubeconfig file.
pub fn load_kube_config() -> Result<Configuration, Error> {
    let kubeconfig = utils::kubeconfig_path()
        .or_else(utils::default_kube_path)
        .context("kubeconfig")?;

    let loader = KubeConfigLoader::load(kubeconfig)?;
    let mut client_builder = Client::builder();

    if let Some(ca) = loader.ca() {
        let req_ca = Certificate::from_der(&ca?.to_der()?)?;
        client_builder = client_builder.add_root_certificate(req_ca);
    }
    match loader.p12(" ") {
        Ok(p12) => {
            let req_p12 = Identity::from_pkcs12_der(&p12.to_der()?, " ")?;
            client_builder = client_builder.identity(req_p12);
        }
        Err(_) => {
            // last resort only if configs ask for it, and no client certs
            if let Some(true) = loader.cluster.insecure_skip_tls_verify {
                client_builder = client_builder.danger_accept_invalid_certs(true);
            }
        }
    }

    let auth_provider = match (
        utils::data_or_file(&loader.user.token, &loader.user.token_file),
        (loader.user.username, loader.user.password),
    ) {
        (Ok(_), _) => {
            let path = std::env::var_os("GOOGLE_APPLICATION_CREDENTIALS")
                .map(std::path::PathBuf::from)
                .context("missing GOOGLE_APPLICATION_CREDENTIALS")?;

            let svc_acct_info = std::fs::read_to_string(path)?;

            Some(AuthProvider::with_service_key(
                ServiceAccountInfo::deserialize(svc_acct_info)?,
            )?)
        }
        (_, (Some(u), Some(p))) => {
            let mut headers = header::HeaderMap::new();

            let encoded = base64::encode(&format!("{}:{}", u, p));
            let hv = header::HeaderValue::from_str(&format!("Basic {}", encoded))?;

            headers.insert(header::AUTHORIZATION, hv);

            client_builder = client_builder.default_headers(headers);

            None
        }
        _ => anyhow::bail!("unable to find an auth-provider"),
    };

    Ok(Configuration::new(
        loader.cluster.server,
        client_builder.build()?,
        auth_provider,
    ))
}

/// Returns a config which is used by clients within pods on kubernetes.
/// It will return an error if called from out of kubernetes cluster.
pub fn incluster_config() -> Result<Configuration, Error> {
    let server = incluster::kube_server().with_context(|| {
        format!(
            "Unable to load incluster config, {} and {} must be defined",
            incluster::SERVICE_HOSTENV,
            incluster::SERVICE_PORTENV,
        )
    })?;

    let ca = incluster::load_cert()?;
    let req_ca = Certificate::from_der(&ca.to_der()?)?;

    let token = incluster::load_token()?;
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        header::HeaderValue::from_str(&format!("Bearer {}", token))?,
    );

    let client_builder = Client::builder()
        .add_root_certificate(req_ca)
        .default_headers(headers);

    Ok(Configuration::new(server, client_builder.build()?, None))
}
