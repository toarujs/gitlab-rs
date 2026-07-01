#![allow(dead_code)]
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub listen_addr: String,
    pub listen_network: String,
    pub listen_umask: Option<i32>,
    pub secret_path: PathBuf,
    pub document_root: PathBuf,
    pub development_mode: bool,

    // Backend configuration
    pub auth_backend: Url,
    pub auth_socket: Option<String>,
    pub cable_backend: Option<Url>,
    pub cable_socket: Option<String>,

    // Timeouts
    pub proxy_headers_timeout: Duration,
    pub shutdown_timeout: Duration,

    // Rate limiting
    pub api_limit: Option<u32>,
    pub api_queue_limit: Option<u32>,
    pub api_queue_timeout: Duration,
    pub api_ci_long_polling_duration: Duration,

    // Logging
    pub log_file: Option<PathBuf>,
    pub log_format: LogFormat,

    // Monitoring
    pub pprof_listen_addr: Option<String>,
    pub prometheus_listen_addr: Option<String>,

    // Redis configuration
    pub redis: Option<RedisConfig>,

    // Object storage
    pub object_storage: Option<ObjectStorageConfig>,

    // Image resizer
    pub image_resizer: Option<ImageResizerConfig>,

    // Circuit breaker
    pub circuit_breaker: Option<CircuitBreakerConfig>,

    // Health check
    pub health_check: Option<HealthCheckConfig>,

    // Load shedding
    pub load_shedding: Option<LoadSheddingConfig>,

    // Listeners
    pub listeners: Vec<ListenerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LogFormat {
    Text,
    Json,
    Structured,
    None,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RedisConfig {
    pub url: String,
    pub sentinel: Option<Vec<String>>,
    pub sentinel_master: Option<String>,
    pub sentinel_username: Option<String>,
    pub sentinel_password: Option<String>,
    pub tls: Option<TlsConfig>,
}

impl std::fmt::Debug for RedisConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisConfig")
            .field("url", &self.url)
            .field("sentinel", &self.sentinel)
            .field("sentinel_master", &self.sentinel_master)
            .field("sentinel_username", &self.sentinel_username)
            .field("sentinel_password", &self.sentinel_password.as_ref().map(|_| "***"))
            .field("tls", &self.tls)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsConfig {
    pub certificate: PathBuf,
    pub key: PathBuf,
    pub ca_certificate: Option<PathBuf>,
    pub min_version: Option<String>,
    pub max_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectStorageConfig {
    pub provider: StorageProvider,
    pub s3: Option<S3Config>,
    pub azure: Option<AzureConfig>,
    pub google: Option<GoogleConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StorageProvider {
    AWS,
    AzureRM,
    Google,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct S3Config {
    pub aws_access_key_id: String,
    pub aws_secret_access_key: String,
    pub region: Option<String>,
    pub endpoint: Option<String>,
}

impl fmt::Debug for S3Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3Config")
            .field("aws_access_key_id", &self.aws_access_key_id)
            .field("aws_secret_access_key", &"***")
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AzureConfig {
    pub storage_account_name: String,
    pub storage_access_key: String,
}

impl fmt::Debug for AzureConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AzureConfig")
            .field("storage_account_name", &self.storage_account_name)
            .field("storage_access_key", &"***")
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct GoogleConfig {
    pub application_default: bool,
    pub json_key_string: Option<String>,
    pub json_key_location: Option<PathBuf>,
}

impl fmt::Debug for GoogleConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GoogleConfig")
            .field("application_default", &self.application_default)
            .field("json_key_string", &self.json_key_string.as_ref().map(|_| "***"))
            .field("json_key_location", &self.json_key_location)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageResizerConfig {
    pub max_scaler_procs: u32,
    pub max_filesize: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerConfig {
    pub enabled: bool,
    pub timeout: Duration,
    pub interval: Duration,
    pub max_requests: u32,
    pub consecutive_failures: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckConfig {
    pub network: String,
    pub addr: String,
    pub readiness_probe_url: Option<String>,
    pub graceful_shutdown_delay: Option<Duration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadSheddingConfig {
    pub enabled: bool,
    pub max_connections: Option<u32>,
    pub max_requests_per_second: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListenerConfig {
    pub network: String,
    pub addr: String,
    pub tls: Option<TlsConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_addr: "localhost:8181".to_string(),
            listen_network: "tcp".to_string(),
            listen_umask: Some(0),
            secret_path: PathBuf::from("./.gitlab_workhorse_secret"),
            document_root: PathBuf::from("public"),
            development_mode: false,
            auth_backend: Url::parse("http://localhost:8080").unwrap(),
            auth_socket: None,
            cable_backend: None,
            cable_socket: None,
            proxy_headers_timeout: Duration::from_secs(300),
            shutdown_timeout: Duration::from_secs(60),
            api_limit: None,
            api_queue_limit: None,
            api_queue_timeout: Duration::from_secs(30),
            api_ci_long_polling_duration: Duration::from_millis(50),
            log_file: None,
            log_format: LogFormat::Text,
            pprof_listen_addr: None,
            prometheus_listen_addr: None,
            redis: None,
            object_storage: None,
            image_resizer: None,
            circuit_breaker: None,
            health_check: None,
            load_shedding: None,
            listeners: Vec::new(),
        }
    }
}

impl Config {
    pub fn load_from_file(path: &PathBuf) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn merge_from_cli_args(&mut self, args: &CliArgs) -> anyhow::Result<()> {
        if let Some(addr) = &args.listen_addr {
            self.listen_addr = addr.clone();
        }
        if let Some(network) = &args.listen_network {
            self.listen_network = network.clone();
        }
        if let Some(umask) = args.listen_umask {
            self.listen_umask = Some(umask);
        }
        if let Some(secret) = &args.secret_path {
            self.secret_path = secret.clone();
        }
        if let Some(root) = &args.document_root {
            self.document_root = root.clone();
        }
        if let Some(backend) = &args.auth_backend {
            self.auth_backend = Url::parse(backend)?;
        }
        if let Some(socket) = &args.auth_socket {
            self.auth_socket = Some(socket.clone());
        }
        if let Some(backend) = &args.cable_backend {
            self.cable_backend = Some(Url::parse(backend)?);
        }
        if let Some(socket) = &args.cable_socket {
            self.cable_socket = Some(socket.clone());
        }
        if let Some(timeout) = args.proxy_headers_timeout {
            self.proxy_headers_timeout = timeout;
        }
        if let Some(timeout) = args.shutdown_timeout {
            self.shutdown_timeout = timeout;
        }
        if let Some(limit) = args.api_limit {
            self.api_limit = Some(limit);
        }
        if let Some(limit) = args.api_queue_limit {
            self.api_queue_limit = Some(limit);
        }
        if let Some(timeout) = args.api_queue_timeout {
            self.api_queue_timeout = timeout;
        }
        if let Some(duration) = args.api_ci_long_polling_duration {
            self.api_ci_long_polling_duration = duration;
        }
        if let Some(file) = &args.log_file {
            self.log_file = Some(file.clone());
        }
        if let Some(format) = &args.log_format {
            self.log_format = format.clone();
        }
        if let Some(addr) = &args.pprof_listen_addr {
            self.pprof_listen_addr = Some(addr.clone());
        }
        if let Some(addr) = &args.prometheus_listen_addr {
            self.prometheus_listen_addr = Some(addr.clone());
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CliArgs {
    pub config_file: Option<PathBuf>,
    pub listen_addr: Option<String>,
    pub listen_network: Option<String>,
    pub listen_umask: Option<i32>,
    pub secret_path: Option<PathBuf>,
    pub document_root: Option<PathBuf>,
    pub development_mode: bool,
    pub auth_backend: Option<String>,
    pub auth_socket: Option<String>,
    pub cable_backend: Option<String>,
    pub cable_socket: Option<String>,
    pub proxy_headers_timeout: Option<Duration>,
    pub shutdown_timeout: Option<Duration>,
    pub api_limit: Option<u32>,
    pub api_queue_limit: Option<u32>,
    pub api_queue_timeout: Option<Duration>,
    pub api_ci_long_polling_duration: Option<Duration>,
    pub log_file: Option<PathBuf>,
    pub log_format: Option<LogFormat>,
    pub pprof_listen_addr: Option<String>,
    pub prometheus_listen_addr: Option<String>,
    pub print_version: bool,
}

impl Default for CliArgs {
    fn default() -> Self {
        Self {
            config_file: None,
            listen_addr: None,
            listen_network: None,
            listen_umask: None,
            secret_path: None,
            document_root: None,
            development_mode: false,
            auth_backend: None,
            auth_socket: None,
            cable_backend: None,
            cable_socket: None,
            proxy_headers_timeout: None,
            shutdown_timeout: None,
            api_limit: None,
            api_queue_limit: None,
            api_queue_timeout: None,
            api_ci_long_polling_duration: None,
            log_file: None,
            log_format: None,
            pprof_listen_addr: None,
            prometheus_listen_addr: None,
            print_version: false,
        }
    }
}
