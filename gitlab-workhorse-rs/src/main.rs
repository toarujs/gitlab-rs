use axum::{
    Router,
    middleware,
    routing::{get, post, put},
};
use axum::http::HeaderMap;
use clap::Parser;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_http::{compression::CompressionLayer, cors::CorsLayer, trace::TraceLayer};

mod api;
mod artifacts;
mod badgateway;
mod bodylimit;
mod builds;
mod cache;
mod channel;
mod compression;
mod config;
mod correlation;
mod dependencyproxy;
mod device_detection;
mod download;
mod error;
mod forwardheaders;
mod git;
mod gitaly;
mod gob;
mod headers;
mod health;
mod html_injection;
mod imageresizer;
mod loadshedding;
mod logging;
mod lsif_transformer;
mod memory;
mod metrics;
mod oauthproxy;
mod orbit;
mod proxy;
mod queueing;
mod ratelimit;
mod redis;
mod rejectmethods;
mod routes;
mod secret;
mod senddata;
mod state;
mod staticpages;
mod transport;
mod upstream;
mod upload;
mod zipartifacts;

use state::AppState;

#[derive(Parser, Debug)]
#[command(
    name = "gitlab-workhorse",
    version,
    disable_version_flag = true,
    about = "GitLab Workhorse - smart HTTP proxy"
)]
struct Cli {
    /// Path to secret file for auth backend
    #[arg(long, default_value = "./.gitlab_workhorse_secret")]
    secret_path: String,

    /// Listen address
    #[arg(long, default_value = "localhost:8181")]
    listen_addr: String,

    /// Listen network type: tcp, tcp4, tcp6, unix
    #[arg(long, default_value = "tcp")]
    listen_network: String,

    /// Umask for unix socket
    #[arg(long, default_value = "0")]
    listen_umask: Option<i32>,

    /// Pprof listen address
    #[arg(long, default_value = "")]
    pprof_listen_addr: Option<String>,

    /// Prometheus listen address
    #[arg(long, default_value = "")]
    prometheus_listen_addr: Option<String>,

    /// Log file path
    #[arg(long, default_value = "")]
    log_file: Option<String>,

    /// Log format: text, json, structured, none
    #[arg(long, default_value = "text")]
    log_format: String,

    /// Auth backend URL
    #[arg(long, default_value = "http://localhost:8080")]
    auth_backend: String,

    /// Auth backend unix socket
    #[arg(long, default_value = "")]
    auth_socket: Option<String>,

    /// ActionCable backend URL
    #[arg(long, default_value = "")]
    cable_backend: Option<String>,

    /// ActionCable backend unix socket
    #[arg(long, default_value = "")]
    cable_socket: Option<String>,

    /// Document root for static files
    #[arg(long, default_value = "public")]
    document_root: String,

    /// Proxy headers timeout
    #[arg(long, default_value = "300s")]
    proxy_headers_timeout: String,

    /// Development mode
    #[arg(long, default_value_t = false)]
    development_mode: bool,

    /// API request limit
    #[arg(long, default_value = "0")]
    api_limit: u32,

    /// API queue limit
    #[arg(long, default_value = "0")]
    api_queue_limit: u32,

    /// API queue timeout
    #[arg(long, default_value = "30s")]
    api_queue_timeout: String,

    /// CI long polling duration
    #[arg(long, default_value = "50ms")]
    api_ci_long_polling_duration: String,

    /// TOML config file path
    #[arg(long, default_value = "")]
    config: Option<String>,

    /// Print version and exit
    #[arg(long, default_value_t = false)]
    version: bool,

    /// Shutdown timeout
    #[arg(long, default_value = "60s")]
    shutdown_timeout: String,
}

#[allow(dead_code)]
fn parse_duration(s: &str) -> anyhow::Result<std::time::Duration> {
    let s = s.trim();
    if s.ends_with("ms") {
        let ms: f64 = s[..s.len() - 2].parse()?;
        Ok(std::time::Duration::from_secs_f64(ms / 1000.0))
    } else if s.ends_with('s') {
        let secs: f64 = s[..s.len() - 1].parse()?;
        Ok(std::time::Duration::from_secs_f64(secs))
    } else if s.ends_with('m') {
        let mins: f64 = s[..s.len() - 1].parse()?;
        Ok(std::time::Duration::from_secs_f64(mins * 60.0))
    } else if s.ends_with('h') {
        let hours: f64 = s[..s.len() - 1].parse()?;
        Ok(std::time::Duration::from_secs_f64(hours * 3600.0))
    } else {
        let secs: f64 = s.parse()?;
        Ok(std::time::Duration::from_secs_f64(secs))
    }
}

fn resolve_auth_backend(auth_backend: &str, auth_socket: &Option<String>) -> String {
    if let Some(socket) = auth_socket {
        if !socket.is_empty() {
            return format!("http+unix://{}", socket);
        }
    }
    auth_backend.to_string()
}

fn register_injecters(registry: &senddata::InjecterRegistry) {
    tokio::task::block_in_place(|| {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async {
            registry.register(senddata::Injecter {
                name: "send-file".to_string(),
                prefix: "send-data:send-file:".to_string(),
                inject: Arc::new(|json_data: String, _headers: HeaderMap| {
                    Box::pin(senddata::sendfile::send_file_inject(json_data, _headers))
                }),
            }).await;

            registry.register(senddata::Injecter {
                name: "send-url".to_string(),
                prefix: "send-data:send-url:".to_string(),
                inject: Arc::new(|json_data: String, _headers: HeaderMap| {
                    Box::pin(senddata::sendurl::send_url_inject(json_data, _headers))
                }),
            }).await;

            registry.register(senddata::Injecter {
                name: "git-archive".to_string(),
                prefix: "send-data:git-archive:".to_string(),
                inject: Arc::new(|json_data: String, _headers: HeaderMap| {
                    Box::pin(senddata::git_injectors::git_archive_inject(json_data, _headers))
                }),
            }).await;

            registry.register(senddata::Injecter {
                name: "git-blob".to_string(),
                prefix: "send-data:git-blob:".to_string(),
                inject: Arc::new(|json_data: String, _headers: HeaderMap| {
                    Box::pin(senddata::git_injectors::git_blob_inject(json_data, _headers))
                }),
            }).await;

            registry.register(senddata::Injecter {
                name: "git-diff".to_string(),
                prefix: "send-data:git-diff:".to_string(),
                inject: Arc::new(|json_data: String, _headers: HeaderMap| {
                    Box::pin(senddata::git_injectors::git_diff_inject(json_data, _headers))
                }),
            }).await;

            registry.register(senddata::Injecter {
                name: "git-snapshot".to_string(),
                prefix: "send-data:git-snapshot:".to_string(),
                inject: Arc::new(|json_data: String, _headers: HeaderMap| {
                    Box::pin(senddata::git_injectors::git_snapshot_inject(json_data, _headers))
                }),
            }).await;
        });
    });
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if cli.version {
        println!("gitlab-workhorse version {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let log_level = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into());
    let json_format = cli.log_format == "json";
    logging::init_logging(&log_level, json_format);

    let resolved_backend = resolve_auth_backend(&cli.auth_backend, &cli.auth_socket);
    let _resolved_cable = cli
        .cable_backend
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| resolved_backend.clone());
    let document_root =
        if cli.document_root == "public" && std::env::var("GITLAB_DOCUMENT_ROOT").is_ok() {
            std::env::var("GITLAB_DOCUMENT_ROOT").unwrap()
        } else {
            cli.document_root.clone()
        };

    // Load TOML config if available
    let toml_config = if let Some(ref config_path) = cli.config {
        if !config_path.is_empty() && std::path::Path::new(config_path).exists() {
            tracing::info!("Loading config from {}", config_path);
            match config::Config::load_from_file(&std::path::PathBuf::from(config_path)) {
                Ok(cfg) => Some(cfg),
                Err(e) => {
                    tracing::warn!("Failed to load config file: {}", e);
                    None
                }
            }
        } else {
            None
        }
    } else {
        let config_path =
            std::env::var("CONFIG_FILE").unwrap_or_else(|_| "config.toml".to_string());
        if std::path::Path::new(&config_path).exists() {
            tracing::info!("Loading config from {}", config_path);
            match config::Config::load_from_file(&std::path::PathBuf::from(&config_path)) {
                Ok(cfg) => Some(cfg),
                Err(e) => {
                    tracing::warn!("Failed to load config file: {}", e);
                    None
                }
            }
        } else {
            None
        }
    };

    tracing::info!(
        "Starting GitLab Workhorse RS v{}",
        env!("CARGO_PKG_VERSION")
    );
    tracing::info!("Listen address: {}/{}", cli.listen_network, cli.listen_addr);
    tracing::info!("Auth backend: {}", resolved_backend);
    tracing::info!("Document root: {}", document_root);

    let backend_url = url::Url::parse(&resolved_backend)
        .unwrap_or_else(|_| url::Url::parse("http://localhost:8080").unwrap());

    let connection_pool = proxy::ConnectionPool::default();
    let proxy_client = connection_pool
        .build_client()
        .map_err(|e| anyhow::anyhow!("Failed to create HTTP client: {}", e))?;

    let injecters = Arc::new(senddata::InjecterRegistry::new());
    register_injecters(&injecters);

    let secret = secret::Secret::from_path(&cli.secret_path).ok();

    let api_limit = cli.api_limit;
    let _queue_limit = if cli.api_queue_limit > 0 {
        cli.api_queue_limit
    } else {
        api_limit * 10
    };

    let app_state = AppState {
        proxy: proxy::ProxyState {
            backend_url,
            client: proxy_client,
            auth_socket: cli.auth_socket.filter(|s| !s.is_empty()),
            circuit_breaker: toml_config.as_ref().and_then(|c| c.circuit_breaker.as_ref()).map(|_| {
                proxy::CircuitBreaker::new(5, std::time::Duration::from_secs(60))
            }),
            connection_pool,
        },
        upload: upload::UploadState {
            upload_dir: std::path::PathBuf::from("/tmp/uploads"),
            max_file_size: 100 * 1024 * 1024,
        },
        download: download::DownloadState {
            document_root: std::path::PathBuf::from(&document_root),
            max_file_size: 100 * 1024 * 1024,
        },
        git: git::GitState {
            repository_root: std::path::PathBuf::from("/var/opt/gitlab/git-data/repositories"),
            gitaly_address: None,
        },
        health: health::HealthState::new(
            toml_config
                .as_ref()
                .and_then(|c| c.health_check.as_ref())
                .and_then(|h| h.readiness_probe_url.clone()),
        ),
        metrics: metrics::MetricsState::new(),
        rate_limit: if api_limit > 0 {
            Some(ratelimit::RateLimitState::new(
                api_limit,
                std::time::Duration::from_secs(60),
            ))
        } else {
            None
        },
        cache: Some(cache::CacheState::new(
            1000,
            std::time::Duration::from_secs(300),
        )),
        memory_pool: memory::MemoryPool::default(),
        injecters,
        secret,
        webp_converter: Arc::new(imageresizer::WebPConverter::new(
            500 * 1024 * 1024,
            75.0,
        )),
    };

    let app = Router::new()
        // Health & version endpoints
        // Root
        .route("/", get(proxy::proxy_handler))
        .route("/health", get(state::health_check))
        .route("/version", get(state::version))
        .route("/-/readiness", get(health::readiness_probe))
        .route("/-/liveness", get(health::liveness_probe))
        .route("/-/metrics", get(metrics::metrics_endpoint))
        .route("/-/metrics/json", get(metrics::metrics_json))
        .route("/metrics", get(metrics::metrics_endpoint))
        .route("/metrics/json", get(metrics::metrics_json))
        // Mobile CSS
        .route("/-/mobile.css", get(mobile_css))
        // OAuth routes (must be before catch-all /api/)
        .route("/oauth/authorize", get(routes::oauth::handle_oauth_authorize).post(routes::oauth::handle_oauth_authorize))
        .route("/oauth/authorize_device", get(routes::oauth::handle_oauth_authorize_device).post(routes::oauth::handle_oauth_authorize_device))
        .route("/oauth/token", get(routes::oauth::handle_oauth_token).post(routes::oauth::handle_oauth_token))
        .route("/oauth/revoke", get(routes::oauth::handle_oauth_revoke).post(routes::oauth::handle_oauth_revoke))
        .route("/oauth/introspect", get(routes::oauth::handle_oauth_introspect).post(routes::oauth::handle_oauth_introspect))
        // CI artifacts
        .route(
            "/api/v4/jobs/:job_id/artifacts",
            post(routes::artifacts::handle_artifacts_upload),
        )
        .route(
            "/api/v4/jobs/:job_id/artifacts",
            get(routes::artifacts::handle_artifacts_download),
        )
        // CI long polling
        .route(
            "/api/v4/jobs/request",
            get(routes::ci_long_polling::handle_ci_long_polling).post(routes::ci_long_polling::handle_ci_long_polling),
        )
        // CI SBOM
        .route(
            "/api/v4/jobs/:job_id/sbom_scans",
            post(routes::uploads::handle_sbom_scan),
        )
        // Terraform state
        .route(
            "/api/v4/projects/:project_id/terraform/state/:state_name/lock",
            post(routes::terraform::handle_terraform_state_lock).delete(routes::terraform::handle_terraform_state_unlock),
        )
        // Package repositories
        .route(
            "/api/v4/projects/:project_id/packages/maven/*path",
            put(routes::packages::handle_maven_upload),
        )
        .route(
            "/api/v4/projects/:project_id/packages/npm/-/package/:package/dist-tags/:tag",
            put(routes::packages::handle_npm_upload),
        )
        .route(
            "/api/v4/projects/:project_id/packages/npm/*path",
            put(routes::packages::handle_npm_upload),
        )
        .route(
            "/api/v4/packages/conan/*path",
            put(routes::packages::handle_conan_upload),
        )
        .route(
            "/api/v4/projects/:project_id/packages/conan/*path",
            put(routes::packages::handle_conan_upload),
        )
        .route(
            "/api/v4/projects/:project_id/packages/generic/*path",
            put(routes::packages::handle_generic_upload),
        )
        .route(
            "/api/v4/projects/:project_id/packages/ml_models/*path",
            put(routes::packages::handle_ml_models_upload),
        )
        .route(
            "/api/v4/projects/:project_id/packages/nuget/*path",
            put(routes::packages::handle_nuget_upload).post(routes::packages::handle_nuget_upload),
        )
        .route(
            "/api/v4/projects/:project_id/packages/pypi",
            post(routes::packages::handle_pypi_upload),
        )
        .route(
            "/api/v4/projects/:project_id/packages/debian/*path",
            put(routes::packages::handle_debian_upload),
        )
        .route(
            "/api/v4/projects/:project_id/packages/rpm/*path",
            post(routes::packages::handle_rpm_upload),
        )
        .route(
            "/api/v4/projects/:project_id/packages/rubygems/*path",
            post(routes::packages::handle_rubygems_upload),
        )
        .route(
            "/api/v4/projects/:project_id/packages/terraform/modules/*path",
            put(routes::packages::handle_terraform_upload),
        )
        .route(
            "/api/v4/projects/:project_id/packages/helm/api/:channel/charts",
            post(routes::packages::handle_helm_upload),
        )
        // Observability Backend
        .route(
            "/api/v4/projects/:project_id/observability/v1/traces",
            get(routes::observability::handle_observability_traces).post(routes::observability::handle_observability_traces),
        )
        .route(
            "/api/v4/projects/:project_id/observability/v1/logs",
            get(routes::observability::handle_observability_logs).post(routes::observability::handle_observability_logs),
        )
        .route(
            "/api/v4/projects/:project_id/observability/v1/metrics",
            get(routes::observability::handle_observability_metrics).post(routes::observability::handle_observability_metrics),
        )
        .route(
            "/api/v4/projects/:project_id/observability/v1/analytics",
            get(routes::observability::handle_observability_analytics),
        )
        .route(
            "/api/v4/projects/:project_id/observability/v1/services",
            get(routes::observability::handle_observability_services),
        )
        // Repository API
        .route(
            "/api/v4/projects/:project_id/repository/commits",
            post(routes::uploads::handle_repository_commits),
        )
        .route(
            "/api/v4/projects/:project_id/repository/files/*path",
            post(routes::uploads::handle_repository_files).put(routes::uploads::handle_repository_files),
        )
        // Wiki attachments
        .route(
            "/api/v4/projects/:project_id/wikis/attachments",
            post(routes::wiki::handle_wiki_attachment),
        )
        .route(
            "/api/v4/groups/:group_id/wikis/attachments",
            post(routes::wiki::handle_wiki_attachment),
        )
        // GraphQL
        .route(
            "/api/graphql",
            post(proxy::proxy_handler),
        )
        // Project/Group/User avatars and uploads
        .route(
            "/api/v4/projects/:project_id/uploads",
            post(routes::uploads::handle_project_upload),
        )
        .route(
            "/api/v4/projects",
            post(proxy::proxy_handler).put(proxy::proxy_handler),
        )
        .route(
            "/api/v4/projects/:project_id",
            put(proxy::proxy_handler),
        )
        .route(
            "/api/v4/groups",
            post(proxy::proxy_handler).put(proxy::proxy_handler),
        )
        .route(
            "/api/v4/groups/:group_id",
            put(proxy::proxy_handler),
        )
        .route(
            "/api/v4/organizations",
            post(proxy::proxy_handler).put(proxy::proxy_handler),
        )
        .route(
            "/api/v4/organizations/:org_id",
            put(proxy::proxy_handler),
        )
        .route(
            "/api/v4/user/avatar",
            put(routes::uploads::handle_avatar_upload),
        )
        .route(
            "/api/v4/users",
            post(proxy::proxy_handler),
        )
        .route(
            "/api/v4/users/:user_id",
            put(proxy::proxy_handler),
        )
        // Remote mirrors
        .route(
            "/api/v4/projects/:project_id/remote_mirrors",
            post(proxy::proxy_handler),
        )
        // Topics
        .route(
            "/api/v4/topics",
            post(proxy::proxy_handler).put(proxy::proxy_handler),
        )
        // Import
        .route(
            "/api/v4/groups/import",
            post(routes::import::handle_import_gitlab_group),
        )
        .route(
            "/api/v4/projects/import",
            post(routes::import::handle_import_gitlab_project),
        )
        .route(
            "/api/v4/projects/import-relation",
            post(routes::import::handle_import_relation),
        )
        .route(
            "/import/gitlab_project",
            post(routes::import::handle_import_gitlab_project),
        )
        .route(
            "/import/gitlab_group",
            post(routes::import::handle_import_gitlab_group),
        )
        // Metric images
        .route(
            "/api/v4/projects/:project_id/issues/:issue_id/metric_images",
            post(routes::uploads::handle_project_upload),
        )
        .route(
            "/api/v4/projects/:project_id/alert_management_alerts/:alert_id/metric_images",
            post(routes::uploads::handle_project_upload),
        )
        // Internal API (allowed endpoint for git SSH)
        .route(
            "/api/v4/internal/allowed",
            post(proxy::proxy_handler),
        )
        // WebSocket routes
        .route("/-/cable", get(proxy::proxy_websocket))
        .route("/-/cable/*path", get(proxy::proxy_websocket))
        // Assets (static files with long cache)
        .route("/assets/*path", get(staticpages::serve_static_file))
        // Generic uploads (file upload endpoints)
        .route("/uploads/*path", post(upload::handle_upload).get(proxy::proxy_handler))
        // Upload status/progress (specific patterns)
        .route("/uploads/progress/*path", get(upload::handle_upload_progress))
        // Download and stream
        .route("/download/*path", get(download::handle_download))
        .route("/stream/*path", get(download::handle_download_stream))
        // Git operations (generic)
        .route(
            "/git/*path",
            get(git::handle_git_request).post(git::handle_git_request),
        )
        .route("/git/clone/*path", get(git::handle_git_clone))
        // Catch-all API proxy (must be after all specific API routes)
        .route(
            "/api/*path",
            get(proxy::proxy_handler)
                .post(proxy::proxy_handler)
                .put(proxy::proxy_handler)
                .patch(proxy::proxy_handler)
                .delete(proxy::proxy_handler),
        )
        // Catch-all: proxy all other requests to backend
        .route(
            "/*path",
            get(proxy::proxy_handler)
                .post(proxy::proxy_handler)
                .put(proxy::proxy_handler)
                .patch(proxy::proxy_handler)
                .delete(proxy::proxy_handler),
        )
        .fallback(staticpages::error_page_fallback)
        .layer(
            ServiceBuilder::new()
                .layer(TraceLayer::new_for_http())
                .layer(CorsLayer::permissive())
                .layer(CompressionLayer::new()),
        )
        .layer(middleware::from_fn(correlation::correlation_id_middleware))
        .layer(middleware::from_fn(rejectmethods::reject_methods_middleware))
        .layer(middleware::from_fn(bodylimit::body_limit_middleware))
        .layer(middleware::from_fn(loadshedding::load_shedding_middleware))
        .layer(middleware::from_fn_with_state(app_state.clone(), ratelimit::rate_limit_middleware))
        .layer(middleware::from_fn(device_detection::device_detection_middleware))
        .with_state(app_state.clone());

    let addr: SocketAddr = cli
        .listen_addr
        .parse()
        .unwrap_or_else(|_| "127.0.0.1:8181".parse().unwrap());

    let tls_config = toml_config
        .as_ref()
        .and_then(|c| c.listeners.first())
        .and_then(|l| l.tls.clone());

    let health_state = app_state.health.clone();
    let backend_health_url = app_state.proxy.backend_url.join("/health").ok();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        if let Some(url) = backend_health_url {
            if health::check_backend_health(url.as_ref()).await {
                health_state.set_ready(true).await;
                tracing::info!("Backend health check passed, ready to serve");
            } else {
                tracing::warn!("Backend health check failed, but proceeding anyway");
                health_state.set_ready(true).await;
            }
        } else {
            health_state.set_ready(true).await;
        }
    });

    if let Some(tls) = tls_config {
        tracing::info!("TLS enabled, starting HTTP/2 server on {}", addr);
        serve_with_tls(addr, app, &tls).await?;
    } else {
        if cli.listen_network == "unix" {
            let _ = std::fs::remove_file(&cli.listen_addr);
            let uds = tokio::net::UnixListener::bind(&cli.listen_addr)?;
            tracing::info!("Listening on unix socket {}", cli.listen_addr);
            serve_unix(uds, app).await?;
            return Ok(());
        } else {
            let listener = TcpListener::bind(addr).await?;
            tracing::info!("Listening on {}", addr);
            axum::serve(listener, app).await?;
        }
    }

    Ok(())
}

async fn serve_with_tls(
    addr: SocketAddr,
    app: Router,
    tls_config: &config::TlsConfig,
) -> anyhow::Result<()> {
    use rustls::ServerConfig;
    use std::sync::Arc;
    use tokio_rustls::TlsAcceptor;

    let cert_file = std::fs::File::open(&tls_config.certificate)
        .map_err(|e| anyhow::anyhow!("Failed to open certificate file: {}", e))?;
    let mut cert_reader = std::io::BufReader::new(cert_file);
    let certs: Vec<rustls::pki_types::CertificateDer<'static>> =
        rustls_pemfile::certs(&mut cert_reader).collect::<Result<Vec<_>, _>>()?;

    let key_file = std::fs::File::open(&tls_config.key)
        .map_err(|e| anyhow::anyhow!("Failed to open key file: {}", e))?;
    let mut key_reader = std::io::BufReader::new(key_file);
    let key = rustls_pemfile::private_key(&mut key_reader)?
        .ok_or_else(|| anyhow::anyhow!("No private key found in key file"))?;

    let mut server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;

    server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    let tls_acceptor = TlsAcceptor::from(Arc::new(server_config));
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("TLS server listening on {}", addr);

    loop {
        let (tcp_stream, remote_addr) = listener.accept().await?;
        let tls_acceptor = tls_acceptor.clone();
        let app = app.clone();

        tokio::spawn(async move {
            let tls_stream = match tls_acceptor.accept(tcp_stream).await {
                Ok(stream) => stream,
                Err(e) => {
                    tracing::error!("TLS handshake failed from {}: {}", remote_addr, e);
                    return;
                }
            };

            let protocol = tls_stream.get_ref().1.alpn_protocol();
            if protocol == Some(b"h2") {
                tracing::debug!("HTTP/2 connection from {}", remote_addr);
            } else {
                tracing::debug!("HTTP/1.1 connection from {}", remote_addr);
            }

            let service = tower::ServiceBuilder::new().service(app);

            if let Err(e) =
                hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                    .serve_connection(
                        hyper_util::rt::TokioIo::new(tls_stream),
                        hyper_util::service::TowerToHyperService::new(service),
                    )
                    .await
            {
                tracing::error!("Error serving connection from {}: {}", remote_addr, e);
            }
        });
    }
}

async fn serve_unix(listener: tokio::net::UnixListener, app: Router) -> anyhow::Result<()> {
    loop {
        let (stream, _addr) = listener.accept().await?;
        let app = app.clone();

        tokio::spawn(async move {
            let service = tower::ServiceBuilder::new().service(app);

            if let Err(e) =
                hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                    .serve_connection(
                        hyper_util::rt::TokioIo::new(stream),
                        hyper_util::service::TowerToHyperService::new(service),
                    )
                    .await
            {
                tracing::error!("Error serving unix connection: {}", e);
            }
        });
    }
}

async fn mobile_css() -> impl axum::response::IntoResponse {
    let css = include_str!("../assets/mobile-full.css");
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("content-type", "text/css; charset=utf-8".parse().unwrap());
    headers.insert("cache-control", "public, max-age=86400".parse().unwrap());
    (axum::http::StatusCode::OK, headers, css)
}
