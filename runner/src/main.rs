use anyhow::{Context, Result};
use gitlab_rs_runner::api::{ApiError, Client};
use gitlab_rs_runner::config::{default_config_path, load_or_create_system_id, RunnerConfig, RunnerSection};
use gitlab_rs_runner::executor::{self, JobOutcome};
use gitlab_rs_runner::mask::collect_secrets;
use gitlab_rs_runner::trace::Trace;
use gitlab_rs_runner::RUNNER_VERSION;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tracing::{error, info, warn};

struct Cli {
    command: Command,
    config: PathBuf,
}

enum Command {
    Run,
    Version,
}

fn parse_cli(args: &[String]) -> Cli {
    let mut config = default_config_path();
    let mut command = Command::Run;
    let mut i = 1;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "run" => command = Command::Run,
            "--version" | "-v" | "version" => command = Command::Version,
            "--config" => {
                if let Some(v) = args.get(i + 1) {
                    config = PathBuf::from(v);
                    i += 1;
                }
            }
            s if s.starts_with("--config=") => {
                config = PathBuf::from(&s[9..]);
            }
            "--user" | "--working-directory" => {
                i += 1;
            }
            s if s.starts_with("--user=") || s.starts_with("--working-directory=") => {}
            _ => {}
        }
        i += 1;
    }
    Cli { command, config }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    let cli = parse_cli(&args);
    match cli.command {
        Command::Version => {
            println!("Version: {RUNNER_VERSION}");
            return Ok(());
        }
        Command::Run => {}
    }

    info!(config = %cli.config.display(), "loading runner config");
    let cfg = match RunnerConfig::load(&cli.config) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("gitlab-runner: {err}");
            std::process::exit(1);
        }
    };
    let system_id = load_or_create_system_id(&cli.config);
    info!(system_id = %system_id, concurrent = cfg.concurrent, "starting gitlab-rs-runner");

    let docker = match executor::connect_docker().await {
        Ok(d) => d,
        Err(err) => {
            eprintln!("gitlab-runner: docker unavailable: {err}");
            std::process::exit(1);
        }
    };

    let shutdown = Arc::new(AtomicBool::new(false));
    let inflight = Arc::new(AtomicUsize::new(0));
    let cap = cfg.concurrent.max(1) as usize;
    let sem = Arc::new(Semaphore::new(cap));
    let interval = Duration::from_secs(cfg.request_interval());
    let shutdown_timeout = if cfg.shutdown_timeout == 0 {
        Duration::from_secs(30)
    } else {
        Duration::from_secs(cfg.shutdown_timeout)
    };

    let flag = shutdown.clone();
    tokio::spawn(async move {
        wait_shutdown().await;
        info!("shutdown requested");
        flag.store(true, Ordering::SeqCst);
    });

    let mut last_updates: Vec<Option<String>> = vec![None; cfg.runners.len()];
    while !shutdown.load(Ordering::SeqCst) {
        let mut got_job = false;
        for (idx, runner) in cfg.runners.iter().enumerate() {
            if shutdown.load(Ordering::SeqCst) {
                break;
            }
            let permit = match sem.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => break,
            };
            match poll_once(
                &docker,
                runner,
                &system_id,
                &mut last_updates[idx],
            )
            .await
            {
                Ok(Some(job)) => {
                    got_job = true;
                    inflight.fetch_add(1, Ordering::SeqCst);
                    let docker = docker.clone();
                    let runner = runner.clone();
                    let inflight = inflight.clone();
                    tokio::spawn(async move {
                        if let Err(err) = execute_job(docker, runner, job).await {
                            error!(error = %err, "job execution failed");
                        }
                        inflight.fetch_sub(1, Ordering::SeqCst);
                        drop(permit);
                    });
                }
                Ok(None) => drop(permit),
                Err(err) => {
                    drop(permit);
                    if err.downcast_ref::<ApiError>() == Some(&ApiError::Unauthorized)
                        || err.to_string().contains("runner token rejected")
                    {
                        error!("runner token rejected, exiting");
                        std::process::exit(1);
                    }
                    warn!(error = ?err, "job request failed");
                }
            }
        }
        if !got_job {
            tokio::time::sleep(interval).await;
        }
    }

    let deadline = tokio::time::Instant::now() + shutdown_timeout;
    while inflight.load(Ordering::SeqCst) > 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    info!("runner stopped");
    Ok(())
}

async fn wait_shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).expect("sigterm handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
}

async fn poll_once(
    _docker: &bollard::Docker,
    runner: &RunnerSection,
    system_id: &str,
    last_update: &mut Option<String>,
) -> Result<Option<gitlab_rs_runner::api::JobResponse>> {
    let client = Client::new(&runner.url)?;
    client.request_job(&runner.token, system_id, last_update).await
}

async fn execute_job(
    docker: bollard::Docker,
    runner: RunnerSection,
    job: gitlab_rs_runner::api::JobResponse,
) -> Result<()> {
    let client = Client::new(&runner.url)?;
    let secrets = collect_secrets(
        job.variables
            .iter()
            .filter(|v| v.masked)
            .map(|v| v.value.clone()),
    );
    let mut trace = Trace::new(client.clone(), job.id, job.token.clone(), secrets);
    info!(job_id = job.id, name = %job.job_info.name, "job received");
    client
        .update_state(job.id, &job.token, "running", None, None)
        .await
        .context("mark running")?;
    trace
        .append_line(&format!(
            "Running with gitlab-rs-runner {RUNNER_VERSION} ({name})",
            name = runner.name
        ))
        .await?;

    let docker_cfg = runner
        .docker
        .clone()
        .context("missing [runners.docker]")?;
    let clone_base = runner
        .clone_url
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(runner.url.as_str());

    let outcome = match executor::run_job(&docker, &job, &docker_cfg, clone_base, &mut trace).await {
        Ok(o) => o,
        Err(err) => {
            let _ = trace.append_line(&format!("ERROR: {err}")).await;
            JobOutcome {
                success: false,
                exit_code: 1,
                failure_reason: "runner_system_failure",
            }
        }
    };
    let _ = trace.flush().await;
    if outcome.success {
        client
            .update_state(job.id, &job.token, "success", Some(0), None)
            .await?;
        info!(job_id = job.id, "job succeeded");
    } else {
        client
            .update_state(
                job.id,
                &job.token,
                "failed",
                Some(outcome.exit_code),
                Some(outcome.failure_reason),
            )
            .await?;
        warn!(
            job_id = job.id,
            exit = outcome.exit_code,
            reason = outcome.failure_reason,
            "job failed"
        );
    }
    Ok(())
}
