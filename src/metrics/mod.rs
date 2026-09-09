use axum::{Json, extract::State, http::StatusCode};
use prometheus::{Gauge, Histogram, HistogramVec, IntCounter, IntCounterVec, Registry, TextEncoder};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

use super::state::AppState;
use super::hotpath::{HotPathWindows, HotWindowSnapshot};

#[derive(Debug, Clone)]
pub struct MetricsState {
    pub registry: Registry,
    pub http_requests_total: IntCounter,
    pub http_request_duration_seconds: Histogram,
    pub http_requests_in_flight: Gauge,
    pub upload_bytes_total: IntCounter,
    pub download_bytes_total: IntCounter,
    pub git_operations_total: IntCounter,
    pub hotpath_duration_seconds: HistogramVec,
    pub hotpath_result_total: IntCounterVec,
    hotpath_windows: Arc<Mutex<HotPathWindows>>,
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

        let hotpath_duration_seconds = HistogramVec::new(
            prometheus::HistogramOpts::new(
                "gitlab_rs_hotpath_duration_seconds",
                "Puma wait time for hot path templates (observed after 50 hits / 5 min)",
            )
            .buckets(vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]),
            &["path", "result"],
        )
        .unwrap();

        let hotpath_result_total = IntCounterVec::new(
            prometheus::Opts::new(
                "gitlab_rs_hotpath_result_total",
                "Hot path results: hit, fallback, error",
            ),
            &["result"],
        )
        .unwrap();

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
        registry
            .register(Box::new(hotpath_duration_seconds.clone()))
            .unwrap();
        registry
            .register(Box::new(hotpath_result_total.clone()))
            .unwrap();

        Self {
            registry,
            http_requests_total,
            http_request_duration_seconds,
            http_requests_in_flight,
            upload_bytes_total,
            download_bytes_total,
            git_operations_total,
            hotpath_duration_seconds,
            hotpath_result_total,
            hotpath_windows: Arc::new(Mutex::new(HotPathWindows::default())),
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

    /// Record a Puma-proxied request. Histogram is only observed for hot templates.
    pub fn record_hotpath(
        &self,
        path_template: &str,
        result: &str,
        puma_secs: f64,
    ) -> HotWindowSnapshot {
        self.hotpath_result_total
            .with_label_values(&[result])
            .inc();

        let puma_ms = (puma_secs * 1000.0).round() as u64;
        let snap = match self.hotpath_windows.lock() {
            Ok(mut windows) => windows.record(path_template, puma_ms),
            Err(_) => HotWindowSnapshot {
                count: 0,
                is_hot: false,
                p50_ms: None,
                p95_ms: None,
                log_aggregate: false,
            },
        };

        if snap.is_hot {
            self.hotpath_duration_seconds
                .with_label_values(&[path_template, result])
                .observe(puma_secs);
        }
        snap
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
