use std::{
    env,
    path::{Path, PathBuf},
};

use anyhow::Error;

const KUBECONFIG: &str = "KUBECONFIG";

/// Returns kubeconfig path from specified environment variable.
pub fn kubeconfig_path() -> Option<PathBuf> {
    env::var_os(KUBECONFIG).map(PathBuf::from)
}

/// Returns kubeconfig path from `$HOME/.kube/config`.
pub fn default_kube_path() -> Option<PathBuf> {
    app_dirs2::data_root(app_dirs2::AppDataType::UserConfig)
        .map(|h| h.join(".kube").join("config"))
        .ok()
}

pub fn data_or_file_with_base64<P: AsRef<Path>>(
    data: &Option<String>,
    file: &Option<P>,
) -> Result<Vec<u8>, Error> {
    match (data, file) {
        (Some(d), _) => base64::decode(&d).map_err(Error::from),
        (_, Some(f)) => Ok(std::fs::read(f)?),
        _ => anyhow::bail!("Failed to get data/file with base64 format"),
    }
}

pub fn data_or_file<P: AsRef<Path>>(
    data: &Option<String>,
    file: &Option<P>,
) -> Result<String, Error> {
    match (data, file) {
        (Some(d), _) => Ok(d.to_string()),
        (_, Some(f)) => Ok(std::fs::read_to_string(f)?),
        _ => anyhow::bail!("Failed to get data/file"),
    }
}
