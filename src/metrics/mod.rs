use axum::{Json, extract::State, http::StatusCode};
use prometheus::{Gauge, Histogram, IntCounter, Registry, TextEncoder};
use serde::{Deserialize, Serialize};

use super::state::AppState;

#[derive(Debug, Clone)]
pub struct MetricsState {
    pub registry: Registry,
    pub http_requests_total: IntCounter,
    pub http_request_duration_seconds: Histogram,
    pub http_requests_in_flight: Gauge,
    pub upload_bytes_total: IntCounter,
    pub download_bytes_total: IntCounter,
    pub git_operations_total: IntCounter,
}

impl MetricsState {
    pub fn new() -> Self {
        let registry = Registry::new();

        let http_requests_total =
            IntCounter::new("http_requests_total", "Total number of HTTP requests").unwrap();

        let http_request_duration_seconds = Histogram::with_opts(
            prometheus::HistogramOpts::new(
                "http_request_duration_seconds",
                "HTTP request duration in seconds",
            )
            .buckets(vec![0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 10.0]),
        )
        .unwrap();

        let http_requests_in_flight = Gauge::new(
            "http_requests_in_flight",
            "Number of HTTP requests currently in flight",
        )
        .unwrap();

        let upload_bytes_total =
            IntCounter::new("upload_bytes_total", "Total bytes uploaded").unwrap();

        let download_bytes_total =
            IntCounter::new("download_bytes_total", "Total bytes downloaded").unwrap();

        let git_operations_total =
            IntCounter::new("git_operations_total", "Total number of Git operations").unwrap();

        registry
            .register(Box::new(http_requests_total.clone()))
            .unwrap();
        registry
            .register(Box::new(http_request_duration_seconds.clone()))
            .unwrap();
        registry
            .register(Box::new(http_requests_in_flight.clone()))
            .unwrap();
        registry
            .register(Box::new(upload_bytes_total.clone()))
            .unwrap();
        registry
            .register(Box::new(download_bytes_total.clone()))
            .unwrap();
        registry
            .register(Box::new(git_operations_total.clone()))
            .unwrap();

        Self {
            registry,
            http_requests_total,
            http_request_duration_seconds,
            http_requests_in_flight,
            upload_bytes_total,
            download_bytes_total,
            git_operations_total,
        }
    }

    pub fn record_request(&self) {
        self.http_requests_total.inc();
        self.http_requests_in_flight.inc();
    }

    pub fn record_request_duration(&self, duration: f64) {
        self.http_request_duration_seconds.observe(duration);
        self.http_requests_in_flight.dec();
    }

    pub fn record_upload(&self, bytes: u64) {
        self.upload_bytes_total.inc_by(bytes);
    }

    pub fn record_download(&self, bytes: u64) {
        self.download_bytes_total.inc_by(bytes);
    }

    pub fn record_git_operation(&self) {
        self.git_operations_total.inc();
    }
}

pub async fn metrics_endpoint(State(state): State<AppState>) -> Result<String, StatusCode> {
    let encoder = TextEncoder::new();
    let metric_families = state.metrics.registry.gather();

    encoder.encode_to_string(&metric_families).map_err(|e| {
        tracing::error!("Failed to encode metrics: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

pub async fn metrics_json(
    State(state): State<AppState>,
) -> Result<Json<MetricsResponse>, StatusCode> {
    let response = MetricsResponse {
        http_requests_total: state.metrics.http_requests_total.get(),
        http_requests_in_flight: state.metrics.http_requests_in_flight.get() as u64,
        upload_bytes_total: state.metrics.upload_bytes_total.get(),
        download_bytes_total: state.metrics.download_bytes_total.get(),
        git_operations_total: state.metrics.git_operations_total.get(),
    };

    Ok(Json(response))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MetricsResponse {
    pub http_requests_total: u64,
    pub http_requests_in_flight: u64,
    pub upload_bytes_total: u64,
    pub download_bytes_total: u64,
    pub git_operations_total: u64,
}
