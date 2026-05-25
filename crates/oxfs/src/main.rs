use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use opendal::services::{Fs, S3};
use opendal::Operator;

use oxfs_data::OpenDalDataEngine;
use oxfs_fuse::OxfsFuse;
use oxfs_meta::{MetaEngine, RedbMetaEngine, SqliteMetaEngine};
use oxfs_vfs::{PassthroughCache, Vfs};

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
        #[arg(long, default_value = "redb")]
        meta_backend: String,
        #[arg(long)]
        default_permissions: bool,
    },
}

fn mount<M: MetaEngine + 'static>(
    meta: Arc<M>,
    op: Operator,
    mountpoint: &PathBuf,
    rt: &tokio::runtime::Handle,
    default_permissions: bool,
) -> Result<()> {
    let data = OpenDalDataEngine::new(op);
    let cache = Arc::new(PassthroughCache::new(data));
    let vfs = Arc::new(Vfs::new(meta, cache));
    let fs = OxfsFuse::new(vfs, rt.clone());

    let mut config = fuser::Config::default();
    config.mount_options.push(fuser::MountOption::FSName("oxfs".into()));
    if default_permissions {
        config.mount_options.push(fuser::MountOption::DefaultPermissions);
    }
    config.acl = fuser::SessionACL::All;
    fuser::mount2(fs, mountpoint, &config)?;
    Ok(())
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
            meta_backend,
            default_permissions,
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
                    if let Some(ref b) = bucket { builder = builder.bucket(b); }
                    if let Some(ref r) = region { builder = builder.region(r); }
                    if let Some(ref e) = endpoint { builder = builder.endpoint(e); }
                    Operator::new(builder)?.finish()
                }
                other => anyhow::bail!("unsupported backend: {other}"),
            };

            tracing::info!("mounting oxfs at {} (meta: {})", mountpoint.display(), meta_backend);

            match meta_backend.as_str() {
                "redb" => {
                    let meta = Arc::new(RedbMetaEngine::new(&meta_db)?);
                    mount(meta, op, &mountpoint, rt.handle(), default_permissions)?;
                }
                "sqlite" => {
                    let meta = Arc::new(SqliteMetaEngine::new(&meta_db)?);
                    mount(meta, op, &mountpoint, rt.handle(), default_permissions)?;
                }
                other => anyhow::bail!("unsupported meta backend: {other} (use redb or sqlite)"),
            }
        }
    }

    Ok(())
}
