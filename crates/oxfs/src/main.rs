use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use opendal::services::{Fs, S3};
use opendal::Operator;

use oxfs_data::OpenDalDataEngine;
use oxfs_fuse::OxfsFuse;
use oxfs_meta::{MetaEngine, RedbMetaEngine, SqliteMetaEngine};
use oxfs_vfs::{CacheConfig, TieredCache, Vfs};

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
        #[arg(long)]
        prefix: Option<String>,
        #[arg(long, default_value = "oxfs.db")]
        meta_db: PathBuf,
        #[arg(long, default_value = "redb")]
        meta_backend: String,
        #[arg(long)]
        default_permissions: bool,
        #[arg(long, default_value = "256")]
        cache_mem_mb: u64,
        #[arg(long)]
        cache_disk_path: Option<PathBuf>,
        #[arg(long, default_value = "1024")]
        cache_disk_mb: u64,
        #[arg(long)]
        wal_path: Option<PathBuf>,
        /// Fork into the background before mounting.
        #[arg(short = 'd', long)]
        daemonize: bool,
    },
}

fn mount<M: MetaEngine + 'static>(
    meta: Arc<M>,
    op: Operator,
    mountpoint: &PathBuf,
    default_permissions: bool,
    cache_config: CacheConfig,
) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let _guard = rt.enter();
    let data = OpenDalDataEngine::new(op);
    let cache = Arc::new(TieredCache::new(data, cache_config));
    let vfs = Arc::new(Vfs::new(meta, cache));
    let fs = OxfsFuse::new(vfs, rt.handle().clone());

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

    match cli.command {
        Command::Mount {
            mountpoint,
            backend,
            root,
            bucket,
            region,
            endpoint,
            prefix,
            meta_db,
            meta_backend,
            default_permissions,
            cache_mem_mb,
            cache_disk_path,
            cache_disk_mb,
            wal_path,
            daemonize,
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
                    if let Some(ref p) = prefix { builder = builder.root(p); }
                    Operator::new(builder)?.finish()
                }
                other => anyhow::bail!("unsupported backend: {other}"),
            };

            let cache_config = CacheConfig {
                mem_max_bytes: cache_mem_mb * 1024 * 1024,
                disk_path: cache_disk_path,
                disk_max_bytes: cache_disk_mb * 1024 * 1024,
                wal_path,
            };

            tracing::info!(
                "mounting oxfs at {} (meta: {}, cache: {}MB mem{})",
                mountpoint.display(),
                meta_backend,
                cache_mem_mb,
                cache_config.disk_path.as_ref()
                    .map(|p| format!(", {}MB disk at {}", cache_disk_mb, p.display()))
                    .unwrap_or_default(),
            );

            if daemonize {
                let daemon = daemonize::Daemonize::new()
                    .working_directory("/");
                daemon.start()?;
            }

            match meta_backend.as_str() {
                "redb" => {
                    let meta = Arc::new(RedbMetaEngine::new(&meta_db)?);
                    mount(meta, op, &mountpoint, default_permissions, cache_config)?;
                }
                "sqlite" => {
                    let meta = Arc::new(SqliteMetaEngine::new(&meta_db)?);
                    mount(meta, op, &mountpoint, default_permissions, cache_config)?;
                }
                other => anyhow::bail!("unsupported meta backend: {other} (use redb or sqlite)"),
            }
        }
    }

    Ok(())
}
