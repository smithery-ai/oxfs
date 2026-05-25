use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use opendal::services::{Fs, S3};
use opendal::Operator;

use oxfs_vfs::PassthroughCache;
use oxfs_data::OpenDalDataEngine;
use oxfs_fuse::OxfsFuse;
use oxfs_meta::SqliteMetaEngine;
use oxfs_vfs::Vfs;

#[derive(Parser)]
#[command(name = "oxfs", about = "FUSE filesystem backed by any object store")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Mount {
        mountpoint: PathBuf,
        #[arg(long, default_value = "fs")]
        backend: String,
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long)]
        bucket: Option<String>,
        #[arg(long)]
        region: Option<String>,
        #[arg(long)]
        endpoint: Option<String>,
        #[arg(long, default_value = "oxfs.db")]
        meta_db: PathBuf,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("oxfs=debug".parse()?),
        )
        .init();

    let cli = Cli::parse();

    let rt = tokio::runtime::Runtime::new()?;

    match cli.command {
        Command::Mount {
            mountpoint,
            backend,
            root,
            bucket,
            region,
            endpoint,
            meta_db,
        } => {
            let op = match backend.as_str() {
                "fs" => {
                    let root = root.unwrap_or_else(|| PathBuf::from("/tmp/oxfs-data"));
                    std::fs::create_dir_all(&root)?;
                    let builder = Fs::default().root(root.to_str().unwrap());
                    Operator::new(builder)?.finish()
                }
                "s3" => {
                    let mut builder = S3::default();
                    if let Some(ref b) = bucket {
                        builder = builder.bucket(b);
                    }
                    if let Some(ref r) = region {
                        builder = builder.region(r);
                    }
                    if let Some(ref e) = endpoint {
                        builder = builder.endpoint(e);
                    }
                    Operator::new(builder)?.finish()
                }
                other => anyhow::bail!("unsupported backend: {other}"),
            };

            let meta = Arc::new(SqliteMetaEngine::new(&meta_db)?);
            let data = OpenDalDataEngine::new(op);
            let cache = Arc::new(PassthroughCache::new(data));
            let vfs = Arc::new(Vfs::new(meta, cache));
            let fs = OxfsFuse::new(vfs, rt.handle().clone());

            tracing::info!("mounting oxfs at {}", mountpoint.display());

            let mut config = fuser::Config::default();
            config.mount_options.push(fuser::MountOption::FSName("oxfs".into()));
            fuser::mount2(fs, &mountpoint, &config)?;
        }
    }

    Ok(())
}
