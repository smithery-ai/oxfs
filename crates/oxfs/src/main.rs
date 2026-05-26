use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};

use oxfs_backend::{BackendConfig, Operator, build_operator};
use oxfs_flat::{FlatConfig, FlatFuse};
use oxfs_posix::{
    CacheConfig, MetaEngine, OxfsFuse, OpenDalDataEngine,
    RedbMetaEngine, SqliteMetaEngine, TieredCache, Vfs,
};

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
        #[arg(long, env = "OXFS_ACCESS_TOKEN")]
        access_token: Option<String>,
        #[arg(long, env = "OXFS_REFRESH_TOKEN")]
        refresh_token: Option<String>,
        #[arg(long, env = "OXFS_CLIENT_ID")]
        client_id: Option<String>,
        #[arg(long, env = "OXFS_CLIENT_SECRET")]
        client_secret: Option<String>,
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
        #[arg(short = 'd', long)]
        daemonize: bool,
        #[arg(long, default_value = "flat")]
        mode: String,
        /// Buffer writes and flush on close (faster, requires readers use the mount)
        #[arg(long)]
        writeback: bool,
    },
}

fn mount_posix<M: MetaEngine + 'static>(
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
            access_token,
            refresh_token,
            client_id,
            client_secret,
            meta_db,
            meta_backend,
            default_permissions,
            cache_mem_mb,
            cache_disk_path,
            cache_disk_mb,
            wal_path,
            daemonize,
            mode,
            writeback,
        } => {
            let op = build_operator(&backend, BackendConfig {
                root,
                bucket,
                region,
                endpoint,
                prefix,
                access_token,
                refresh_token,
                client_id,
                client_secret,
            })?;

            if daemonize {
                let daemon = daemonize::Daemonize::new().working_directory("/");
                daemon.start()?;
            }

            if mode == "flat" {
                tracing::info!("mounting oxfs (flat mode) at {}", mountpoint.display());

                let rt = tokio::runtime::Runtime::new()?;
                let _guard = rt.enter();

                let flat_fuse = FlatFuse::new(
                    op,
                    rt.handle().clone(),
                    FlatConfig { dir_ttl: Duration::from_secs(1), writeback },
                );

                let mut config = fuser::Config::default();
                config.mount_options.push(fuser::MountOption::FSName("oxfs".into()));
                if default_permissions {
                    config.mount_options.push(fuser::MountOption::DefaultPermissions);
                }
                config.acl = fuser::SessionACL::All;
                fuser::mount2(flat_fuse, &mountpoint, &config)?;
            } else {
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

                match meta_backend.as_str() {
                    "redb" => {
                        let meta = Arc::new(RedbMetaEngine::new(&meta_db)?);
                        mount_posix(meta, op, &mountpoint, default_permissions, cache_config)?;
                    }
                    "sqlite" => {
                        let meta = Arc::new(SqliteMetaEngine::new(&meta_db)?);
                        mount_posix(meta, op, &mountpoint, default_permissions, cache_config)?;
                    }
                    other => anyhow::bail!("unsupported meta backend: {other} (use redb or sqlite)"),
                }
            }
        }
    }

    Ok(())
}
