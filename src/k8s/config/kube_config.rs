use std::path::Path;

use anyhow::{Context as _, Error};
use openssl::{pkcs12::Pkcs12, pkey::PKey, x509::X509};

use super::apis::{AuthInfo, Cluster, Config, Context};

/// [`KubeConfigLoader`] loads current context, cluster, and authentication information.
#[derive(Debug)]
pub struct KubeConfigLoader {
    pub current_context: Context,
    pub cluster: Cluster,
    pub user: AuthInfo,
}

impl KubeConfigLoader {
    pub fn load<P: AsRef<Path>>(path: P) -> Result<KubeConfigLoader, Error> {
        let config = Config::load_config(path)?;
        let current_context = config
            .contexts
            .iter()
            .find(|named_context| named_context.name == config.current_context)
            .map(|named_context| &named_context.context)
            .context("current context")?;
        let cluster = config
            .clusters
            .iter()
            .find(|named_cluster| named_cluster.name == current_context.cluster)
            .map(|named_cluster| &named_cluster.cluster)
            .context("load cluster")?;
        let user = match config
            .auth_infos
            .iter()
            .find(|named_user| named_user.name == current_context.user)
        {
            Some(named_user) => {
                let mut user = named_user.auth_info.clone();
                user.load_gcp()?;
                user
            }
            None => anyhow::bail!("failed to find user {}", current_context.user),
        };
        Ok(KubeConfigLoader {
            current_context: current_context.clone(),
            cluster: cluster.clone(),
            user,
        })
    }

    pub fn p12(&self, password: &str) -> Result<Pkcs12, Error> {
        let client_cert = &self.user.load_client_certificate()?;
        let client_key = &self.user.load_client_key()?;

        let x509 = X509::from_pem(client_cert)?;
        let pkey = PKey::private_key_from_pem(client_key)?;

        Pkcs12::builder()
            .name("kubeconfig")
            .pkey(&pkey)
            .cert(&x509)
            .build2(password)
            .map_err(Error::from)
    }

    pub fn ca(&self) -> Option<Result<X509, Error>> {
        let ca = self.cluster.load_certificate_authority()?;
        Some(ca.and_then(|ca| X509::from_pem(&ca).map_err(Error::from)))
    }
}
