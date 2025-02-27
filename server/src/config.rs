//! Server configuration.

use std::env;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use async_compression::Level as CompressionLevel;
use derivative::Derivative;
use serde::{de, Deserialize};
use xdg::BaseDirectories;

use crate::access::{decode_token_hs256_secret_base64, HS256Key};
use crate::narinfo::Compression as NixCompression;
use crate::storage::{LocalStorageConfig, S3StorageConfig};

/// Application prefix in XDG base directories.
///
/// This will be concatenated into `$XDG_CONFIG_HOME/attic`.
const XDG_PREFIX: &str = "attic";

/// Environment variable storing the Base64-encoded TOML configuration.
///
/// This is useful for deploying to certain application platforms like Fly.io
const ENV_CONFIG_BASE64: &str = "ATTIC_SERVER_CONFIG_BASE64";

/// Environment variable storing the Base64-encoded HS256 JWT secret.
const ENV_TOKEN_HS256_SECRET_BASE64: &str = "ATTIC_SERVER_TOKEN_HS256_SECRET_BASE64";

/// Configuration for the Attic Server.
#[derive(Clone, Derivative, Deserialize)]
#[derivative(Debug)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Socket address to listen on.
    #[serde(default = "default_listen_address")]
    pub listen: SocketAddr,

    /// Allowed `Host` headers.
    ///
    /// This _must_ be configured for production use. If unconfigured or the
    /// list is empty, all `Host` headers are allowed.
    #[serde(rename = "allowed-hosts")]
    #[serde(default = "Vec::new")]
    pub allowed_hosts: Vec<String>,

    /// The canonical API endpoint of this server.
    ///
    /// This is the endpoint exposed to clients in `cache-config` responses.
    ///
    /// This _must_ be configured for production use. If not configured, the
    /// API endpoint is synthesized from the client's `Host` header which may
    /// be insecure.
    ///
    /// The API endpoint _must_ end with a slash (e.g., `https://domain.tld/attic/`
    /// not `https://domain.tld/attic`).
    #[serde(rename = "api-endpoint")]
    pub api_endpoint: Option<String>,

    /// The canonical Nix Binary Cache endpoint of this server.
    ///
    /// This is usually the same as `api_endpoint` but can be configured
    /// to a different value.
    ///
    /// If unconfigured, it's assumed to be the same as `api_endpoint`.
    #[serde(rename = "substituter-endpoint")]
    pub substituter_endpoint: Option<String>,

    /// Whether to soft-delete caches.
    ///
    /// If this is enabled, caches are soft-deleted instead of actually
    /// removed from the database. Note that soft-deleted caches cannot
    /// have their names reused as long as the original database records
    /// are there.
    #[serde(rename = "soft-delete-caches")]
    #[serde(default = "default_soft_delete_caches")]
    pub soft_delete_caches: bool,

    /// Whether to require fully uploading a NAR if it exists in the global cache.
    ///
    /// If set to false, simply knowing the NAR hash is enough for
    /// an uploader to gain access to an existing NAR in the global
    /// cache.
    #[serde(rename = "require-proof-of-possession")]
    #[serde(default = "default_require_proof_of_possession")]
    pub require_proof_of_possession: bool,

    /// Database connection.
    pub database: DatabaseConfig,

    /// Storage.
    pub storage: StorageConfig,

    /// Compression.
    #[serde(default = "Default::default")]
    pub compression: CompressionConfig,

    /// Garbage collection.
    #[serde(rename = "garbage-collection")]
    #[serde(default = "Default::default")]
    pub garbage_collection: GarbageCollectionConfig,

    /// JSON Web Token HMAC secret.
    ///
    /// Set this to the base64 encoding of a randomly generated secret.
    #[serde(rename = "token-hs256-secret-base64")]
    #[serde(deserialize_with = "deserialize_token_hs256_secret_base64")]
    #[serde(default = "load_token_hs256_secret_from_env")]
    #[derivative(Debug = "ignore")]
    pub token_hs256_secret: HS256Key,
}

/// Database connection configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    /// Connection URL.
    pub url: String,

    /// Whether to enable sending of periodic heartbeat queries.
    ///
    /// If enabled, a heartbeat query will be sent every minute.
    #[serde(default = "default_db_heartbeat")]
    pub heartbeat: bool,
}

/// File storage configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum StorageConfig {
    /// Local file storage.
    #[serde(rename = "local")]
    Local(LocalStorageConfig),

    /// S3 storage.
    #[serde(rename = "s3")]
    S3(S3StorageConfig),
}

/// Compression configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct CompressionConfig {
    /// Compression type.
    pub r#type: CompressionType,

    /// Compression level.
    ///
    /// If unspecified, Attic will choose a default one.
    pub level: Option<u32>,
}

/// Compression type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum CompressionType {
    /// No compression.
    #[serde(rename = "none")]
    None,

    /// Brotli.
    #[serde(rename = "brotli")]
    Brotli,

    /// ZSTD.
    #[serde(rename = "zstd")]
    Zstd,

    /// XZ.
    #[serde(rename = "xz")]
    Xz,
}

/// Garbage collection config.
#[derive(Debug, Clone, Deserialize)]
pub struct GarbageCollectionConfig {
    /// The frequency to run garbage collection at.
    ///
    /// If zero, automatic garbage collection is disabled, but
    /// it can still be run manually with `atticd --mode garbage-collector-once`.
    #[serde(with = "humantime_serde", default = "default_gc_interval")]
    pub interval: Duration,

    /// The default retention period of unaccessed objects.
    ///
    /// Objects are subject to garbage collection if both the
    /// `created_at` and `last_accessed_at` timestamps are older
    /// than the retention period.
    ///
    /// Zero (default) means time-based garbage-collection is
    /// disabled by default. You can enable it on a per-cache basis.
    #[serde(rename = "default-retention-period")]
    #[serde(with = "humantime_serde", default = "default_default_retention_period")]
    pub default_retention_period: Duration,
}

fn load_token_hs256_secret_from_env() -> HS256Key {
    let s = env::var(ENV_TOKEN_HS256_SECRET_BASE64)
        .expect("The HS256 secret must be specified in either token_hs256_secret or the ATTIC_SERVER_TOKEN_HS256_SECRET_BASE64 environment.");

    decode_token_hs256_secret_base64(&s).expect("Failed to load as decoding key")
}

impl CompressionConfig {
    pub fn level(&self) -> CompressionLevel {
        if let Some(level) = self.level {
            return CompressionLevel::Precise(level);
        }

        match self.r#type {
            CompressionType::Brotli => CompressionLevel::Precise(5),
            CompressionType::Zstd => CompressionLevel::Precise(8),
            CompressionType::Xz => CompressionLevel::Precise(2),
            _ => CompressionLevel::Default,
        }
    }
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            r#type: CompressionType::Zstd,
            level: None,
        }
    }
}

impl From<CompressionType> for NixCompression {
    fn from(t: CompressionType) -> Self {
        match t {
            CompressionType::None => NixCompression::None,
            CompressionType::Brotli => NixCompression::Brotli,
            CompressionType::Zstd => NixCompression::Zstd,
            CompressionType::Xz => NixCompression::Xz,
        }
    }
}

impl Default for GarbageCollectionConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(43200),
            default_retention_period: Duration::ZERO,
        }
    }
}

fn deserialize_token_hs256_secret_base64<'de, D>(deserializer: D) -> Result<HS256Key, D::Error>
where
    D: de::Deserializer<'de>,
{
    use de::Error;

    let s = String::deserialize(deserializer)?;
    let key = decode_token_hs256_secret_base64(&s).map_err(Error::custom)?;

    Ok(key)
}

fn default_listen_address() -> SocketAddr {
    "[::]:8080".parse().unwrap()
}

fn default_db_heartbeat() -> bool {
    false
}

fn default_soft_delete_caches() -> bool {
    false
}

fn default_require_proof_of_possession() -> bool {
    true
}

fn default_gc_interval() -> Duration {
    Duration::from_secs(43200)
}

fn default_default_retention_period() -> Duration {
    Duration::ZERO
}

fn load_config_from_path(path: &Path) -> Result<Config> {
    tracing::info!("Using configurations: {:?}", path);

    let config = std::fs::read_to_string(path)?;
    Ok(toml::from_str(&config)?)
}

fn load_config_from_str(s: &str) -> Result<Config> {
    tracing::info!("Using configurations from environment variable");
    Ok(toml::from_str(s)?)
}

/// Loads the configuration in the standard order.
pub async fn load_config(config_path: Option<&Path>, allow_oobe: bool) -> Result<Config> {
    if let Some(config_path) = config_path {
        load_config_from_path(&config_path)
    } else if let Ok(config_env) = env::var(ENV_CONFIG_BASE64) {
        let decoded = String::from_utf8(base64::decode(config_env.as_bytes())?)?;
        load_config_from_str(&decoded)
    } else {
        // Config from XDG
        let config_path = get_xdg_config_path()?;

        if allow_oobe {
            // Special OOBE sequence
            crate::oobe::run_oobe().await?;
        }

        load_config_from_path(&config_path)
    }
}

pub fn get_xdg_config_path() -> anyhow::Result<PathBuf> {
    let xdg_dirs = BaseDirectories::with_prefix(XDG_PREFIX)?;
    let config_path = xdg_dirs.place_config_file("server.toml")?;

    Ok(config_path)
}

pub fn get_xdg_data_path() -> anyhow::Result<PathBuf> {
    let xdg_dirs = BaseDirectories::with_prefix(XDG_PREFIX)?;
    let data_path = xdg_dirs.create_data_directory("")?;

    Ok(data_path)
}
