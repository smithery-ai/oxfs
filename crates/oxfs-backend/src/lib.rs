use std::path::PathBuf;

use anyhow::{bail, Result};
use opendal::services::{Dropbox, Fs, Gdrive, S3};
pub use opendal::Operator;

pub struct BackendConfig {
    pub root: Option<PathBuf>,
    pub bucket: Option<String>,
    pub region: Option<String>,
    pub endpoint: Option<String>,
    pub prefix: Option<String>,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
}

pub fn build_operator(backend: &str, cfg: BackendConfig) -> Result<Operator> {
    match backend {
        "fs" => {
            let root = cfg.root.unwrap_or_else(|| PathBuf::from("/tmp/oxfs-data"));
            std::fs::create_dir_all(&root)?;
            let builder = Fs::default().root(root.to_str().unwrap());
            Ok(Operator::new(builder)?.finish())
        }
        "s3" => {
            let mut builder = S3::default();
            if let Some(ref b) = cfg.bucket { builder = builder.bucket(b); }
            if let Some(ref r) = cfg.region { builder = builder.region(r); }
            if let Some(ref e) = cfg.endpoint { builder = builder.endpoint(e); }
            if let Some(ref p) = cfg.prefix { builder = builder.root(p); }
            Ok(Operator::new(builder)?.finish())
        }
        "gdrive" => {
            let mut builder = Gdrive::default();
            if let Some(ref r) = cfg.prefix { builder = builder.root(r); }
            if let Some(ref t) = cfg.access_token { builder = builder.access_token(t); }
            if let Some(ref t) = cfg.refresh_token { builder = builder.refresh_token(t); }
            if let Some(ref id) = cfg.client_id { builder = builder.client_id(id); }
            if let Some(ref s) = cfg.client_secret { builder = builder.client_secret(s); }
            Ok(Operator::new(builder)?.finish())
        }
        "dropbox" => {
            let mut builder = Dropbox::default();
            if let Some(ref r) = cfg.prefix { builder = builder.root(r); }
            if let Some(ref t) = cfg.access_token { builder = builder.access_token(t); }
            if let Some(ref t) = cfg.refresh_token { builder = builder.refresh_token(t); }
            if let Some(ref id) = cfg.client_id { builder = builder.client_id(id); }
            if let Some(ref s) = cfg.client_secret { builder = builder.client_secret(s); }
            Ok(Operator::new(builder)?.finish())
        }
        other => bail!("unsupported backend: {other} (use fs, s3, gdrive, or dropbox)"),
    }
}
