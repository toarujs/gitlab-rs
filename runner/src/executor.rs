use anyhow::{bail, Context, Result};
use bollard::container::{
    Config, CreateContainerOptions, KillContainerOptions, LogsOptions, NetworkingConfig,
    RemoveContainerOptions, StartContainerOptions, WaitContainerOptions,
};
use bollard::image::CreateImageOptions;
use bollard::models::{EndpointSettings, HostConfig, Mount, MountTypeEnum};
use bollard::volume::{CreateVolumeOptions, RemoveVolumeOptions};
use bollard::Docker;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::{info, warn};

use crate::api::{job_var, Image, JobResponse, Step};
use crate::config::DockerSection;
use crate::trace::Trace;
use crate::urlutil::{inject_job_token, redact_secrets, rewrite_clone_url};

const GIT_IMAGE: &str = "alpine/git:latest";
const BUILDS_DIR: &str = "/builds";

pub struct JobOutcome {
    pub success: bool,
    pub exit_code: i32,
    pub failure_reason: &'static str,
}

pub async fn connect_docker() -> Result<Docker> {
    let docker = Docker::connect_with_defaults().context("connect docker (DOCKER_HOST)")?;
    docker.ping().await.context("docker ping")?;
    Ok(docker)
}

pub async fn run_job(
    docker: &Docker,
    job: &JobResponse,
    docker_cfg: &DockerSection,
    clone_base: &str,
    extra_env: &[String],
    trace: &mut Trace,
) -> Result<JobOutcome> {
    if let Some(strategy) = job_var(job, "GIT_STRATEGY") {
        let s = strategy.to_ascii_lowercase();
        if s == "fetch" {
            trace
                .append_line("ERROR: GIT_STRATEGY=fetch is not implemented")
                .await?;
            return Ok(fail(1, "runner_system_failure"));
        }
        if s == "none" {
            // skip clone below
        } else if s != "clone" && !s.is_empty() {
            trace
                .append_line(&format!("ERROR: unsupported GIT_STRATEGY={strategy}"))
                .await?;
            return Ok(fail(1, "runner_system_failure"));
        }
    }
    if let Some(sub) = job_var(job, "GIT_SUBMODULE_STRATEGY") {
        let s = sub.to_ascii_lowercase();
        if s != "none" && !s.is_empty() {
            trace
                .append_line("ERROR: git submodules are not implemented")
                .await?;
            return Ok(fail(1, "runner_system_failure"));
        }
    }

    let skip_clone = job_var(job, "GIT_STRATEGY")
        .map(|s| s.eq_ignore_ascii_case("none"))
        .unwrap_or(false);

    let volume = format!("runner-job-{}-{}", job.id, &uuid::Uuid::new_v4().simple().to_string()[..8]);
    docker
        .create_volume(CreateVolumeOptions {
            name: volume.clone(),
            driver: "local".into(),
            ..Default::default()
        })
        .await
        .context("create builds volume")?;

    let result = run_job_inner(
        docker,
        job,
        docker_cfg,
        clone_base,
        skip_clone,
        extra_env,
        &volume,
        trace,
    )
    .await;

    if let Err(err) = docker
        .remove_volume(
            &volume,
            Some(RemoveVolumeOptions { force: true }),
        )
        .await
    {
        warn!(volume = %volume, error = %err, "remove volume failed");
    }

    result
}

async fn run_job_inner(
    docker: &Docker,
    job: &JobResponse,
    docker_cfg: &DockerSection,
    clone_base: &str,
    skip_clone: bool,
    extra_env: &[String],
    volume: &str,
    trace: &mut Trace,
) -> Result<JobOutcome> {
    let network = docker_cfg.network_mode.clone();
    let extra_mounts = bind_mounts(&docker_cfg.volumes);
    let timeout = if job.runner_info.timeout > 0 {
        Duration::from_secs(job.runner_info.timeout as u64)
    } else {
        Duration::from_secs(3600)
    };

    let mut service_names = Vec::new();
    let result = run_job_with_services(
        docker,
        job,
        docker_cfg,
        clone_base,
        skip_clone,
        extra_env,
        volume,
        network.as_deref(),
        &extra_mounts,
        timeout,
        &mut service_names,
        trace,
    )
    .await;
    for name in service_names {
        let _ = docker
            .remove_container(
                &name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_job_with_services(
    docker: &Docker,
    job: &JobResponse,
    docker_cfg: &DockerSection,
    clone_base: &str,
    skip_clone: bool,
    extra_env: &[String],
    volume: &str,
    network: Option<&str>,
    extra_mounts: &[Mount],
    timeout: Duration,
    service_names: &mut Vec<String>,
    trace: &mut Trace,
) -> Result<JobOutcome> {
    let mut wait_targets: Vec<(String, String)> = Vec::new();
    let service_env = {
        let mut env = extra_env.to_vec();
        env.extend(job_env(job, &[]));
        env
    };
    for (idx, svc) in job.services.iter().enumerate() {
        if svc.name.is_empty() {
            continue;
        }
        let name = format!("runner-{}-svc-{}", job.id, idx);
        let aliases = service_aliases(svc, idx);
        trace
            .append_line(&format!(
                "Starting service {} ({})",
                svc.name,
                aliases.join(", ")
            ))
            .await?;
        ensure_image(docker, &svc.name, &docker_cfg.pull_policy, trace).await?;
        let entrypoint = if svc.entrypoint.is_empty() {
            None
        } else {
            Some(svc.entrypoint.clone())
        };
        let cmd = if svc.command.is_empty() {
            None
        } else {
            Some(svc.command.clone())
        };
        spawn_container(
            docker,
            &name,
            &svc.name,
            entrypoint,
            cmd,
            service_env.clone(),
            extra_mounts.to_vec(),
            network,
            aliases,
            None,
            docker_cfg.privileged,
        )
        .await
        .with_context(|| format!("start service {}", svc.name))?;
        service_names.push(name.clone());
        wait_targets.push((name, svc.name.clone()));
    }
    if !wait_targets.is_empty() {
        let wait = service_wait(docker_cfg.wait_for_services_timeout);
        wait_for_services(docker, &wait_targets, wait, trace).await?;
    }

    if !skip_clone {
        trace.append_line("Getting source from Git repository").await?;
        ensure_image(docker, GIT_IMAGE, &docker_cfg.pull_policy, trace).await?;
        let clone_url = {
            let rewritten = rewrite_clone_url(clone_base, &job.git_info.repo_url);
            inject_job_token(&rewritten, &job.token)
        };
        trace
            .append_line(&format!(
                "Fetching {} for {}",
                redact_secrets(&clone_url),
                job.git_info.sha
            ))
            .await?;
        let git_script = git_clone_script();
        let git_env = vec![
            format!("GIT_CLONE_URL={clone_url}"),
            format!("CI_COMMIT_SHA={}", job.git_info.sha),
            format!("CI_COMMIT_REF_NAME={}", job.git_info.refspec()),
            "GIT_TERMINAL_PROMPT=0".into(),
        ];
        let git_name = format!("runner-{}-git", job.id);
        let status = run_container(
            docker,
            &git_name,
            GIT_IMAGE,
            Some(vec!["/bin/sh".into(), "-c".into()]),
            Some(vec![git_script]),
            git_env,
            vec![builds_mount(volume)],
            network,
            "/",
            docker_cfg.privileged,
            timeout,
            trace,
        )
        .await?;
        if status != 0 {
            trace
                .append_line(&format!("ERROR: git clone failed with exit code {status}"))
                .await?;
            return Ok(fail(status, "runner_system_failure"));
        }
    }

    let image = if job.image.name.is_empty() {
        docker_cfg.image.clone()
    } else {
        job.image.name.clone()
    };
    trace
        .append_line(&format!("Using docker image {image}"))
        .await?;
    ensure_image(docker, &image, &docker_cfg.pull_policy, trace).await?;

    let script = render_script(&job.steps);
    if script.trim().is_empty() {
        trace.append_line("ERROR: job has no script steps").await?;
        return Ok(fail(1, "script_failure"));
    }
    let env = job_env(job, extra_env);
    let job_name = format!("runner-{}-job", job.id);
    let entrypoint = if job.image.entrypoint.is_empty() {
        Some(vec!["/bin/sh".into(), "-c".into()])
    } else {
        Some(job.image.entrypoint.clone())
    };
    let cmd = if job.image.command.is_empty() {
        Some(vec![script])
    } else {
        Some(job.image.command.clone())
    };

    let mut mounts = vec![builds_mount(volume)];
    mounts.push(Mount {
        target: Some("/cache".into()),
        source: Some(format!("{volume}-cache")),
        typ: Some(MountTypeEnum::VOLUME),
        ..Default::default()
    });
    let _ = docker
        .create_volume(CreateVolumeOptions {
            name: format!("{volume}-cache"),
            driver: "local".into(),
            ..Default::default()
        })
        .await;
    mounts.extend(extra_mounts.iter().cloned());

    let status = run_container(
        docker,
        &job_name,
        &image,
        entrypoint,
        cmd,
        env,
        mounts,
        network,
        BUILDS_DIR,
        docker_cfg.privileged,
        timeout,
        trace,
    )
    .await?;
    let _ = docker
        .remove_volume(
            &format!("{volume}-cache"),
            Some(RemoveVolumeOptions { force: true }),
        )
        .await;
    if status == 0 {
        Ok(JobOutcome {
            success: true,
            exit_code: 0,
            failure_reason: "script_failure",
        })
    } else {
        Ok(fail(status, "script_failure"))
    }
}

fn fail(exit_code: i32, failure_reason: &'static str) -> JobOutcome {
    JobOutcome {
        success: false,
        exit_code,
        failure_reason,
    }
}

fn git_clone_script() -> String {
    r#"
set -eu
mkdir -p /builds
cd /builds
git init
git remote add origin "$GIT_CLONE_URL"
if ! git fetch --depth 50 origin "$CI_COMMIT_SHA"; then
  git fetch origin "$CI_COMMIT_SHA" || git fetch origin "$CI_COMMIT_REF_NAME"
fi
git checkout --force "$CI_COMMIT_SHA" || git checkout --force FETCH_HEAD
"#
    .to_string()
}

fn render_script(steps: &[Step]) -> String {
    let mut out = String::from("set +e\n__exit=0\n");
    let mut after = Vec::new();
    for step in steps {
        let when = if step.when.is_empty() {
            "on_success"
        } else {
            step.when.as_str()
        };
        if step.name == "after_script" || when == "always" && step.name.contains("after") {
            after.push(step);
            continue;
        }
        if when == "on_failure" {
            out.push_str("if [ \"$__exit\" -ne 0 ]; then\n");
            append_step(&mut out, step);
            out.push_str("fi\n");
            continue;
        }
        out.push_str("if [ \"$__exit\" -eq 0 ]; then\n");
        append_step(&mut out, step);
        out.push_str("fi\n");
    }
    for step in after {
        append_step(&mut out, step);
    }
    out.push_str("exit $__exit\n");
    out
}

fn append_step(out: &mut String, step: &Step) {
    out.push_str(&format!("echo '$ {name}'\n", name = step.name));
    for line in &step.script {
        let escaped = line.replace('\'', "'\"'\"'");
        out.push_str(&format!("echo '$ {escaped}'\n"));
        out.push_str(line);
        out.push('\n');
        if step.allow_failure {
            out.push_str("st=$?\n");
            out.push_str("if [ $st -ne 0 ]; then echo \"WARNING: command failed with $st (allow_failure)\"; fi\n");
        } else {
            out.push_str("st=$?\n");
            out.push_str("if [ $st -ne 0 ]; then __exit=$st; fi\n");
        }
    }
}

fn job_env(job: &JobResponse, extra: &[String]) -> Vec<String> {
    let mut env = extra.to_vec();
    for var in &job.variables {
        if var.file {
            continue;
        }
        env.push(format!("{}={}", var.key, var.value));
    }
    env.push(format!("CI_PROJECT_DIR={BUILDS_DIR}"));
    env
}

fn builds_mount(volume: &str) -> Mount {
    Mount {
        target: Some(BUILDS_DIR.into()),
        source: Some(volume.into()),
        typ: Some(MountTypeEnum::VOLUME),
        ..Default::default()
    }
}

fn bind_mounts(volumes: &[String]) -> Vec<Mount> {
    let mut out = Vec::new();
    for spec in volumes {
        if spec == "/cache" || spec.is_empty() {
            continue;
        }
        let parts: Vec<&str> = spec.split(':').collect();
        if parts.len() < 2 {
            continue;
        }
        out.push(Mount {
            target: Some(parts[1].into()),
            source: Some(parts[0].into()),
            typ: Some(MountTypeEnum::BIND),
            read_only: Some(parts.get(2).is_some_and(|mode| mode.contains("ro"))),
            ..Default::default()
        });
    }
    out
}

fn service_aliases(svc: &Image, index: usize) -> Vec<String> {
    let mut aliases = Vec::new();
    if !svc.alias_name.is_empty() {
        aliases.push(svc.alias_name.clone());
    }
    let name = svc.name.split('/').next_back().unwrap_or(svc.name.as_str());
    let base = name.split(':').next().unwrap_or(name);
    if !base.is_empty() && !aliases.iter().any(|a| a == base) {
        aliases.push(base.to_string());
    }
    aliases.push(format!("svc-{index}"));
    aliases
}

fn service_wait(timeout: i64) -> Duration {
    let secs = if timeout > 0 { timeout as u64 } else { 30 };
    Duration::from_secs(secs)
}

async fn ensure_image(
    docker: &Docker,
    image: &str,
    pull_policy: &[String],
    trace: &mut Trace,
) -> Result<()> {
    let exists = docker.inspect_image(image).await.is_ok();
    let policy = pull_policy
        .first()
        .map(|s| s.as_str())
        .unwrap_or("if-not-present");
    let pull = match policy {
        "never" => false,
        "always" => true,
        _ => !exists,
    };
    if !pull {
        if !exists {
            bail!("image {image} missing and pull_policy={policy}");
        }
        return Ok(());
    }
    trace.append_line(&format!("Pulling image {image}")).await?;
    info!(image, "pull image");
    let mut stream = docker.create_image(
        Some(CreateImageOptions {
            from_image: image,
            ..Default::default()
        }),
        None,
        None,
    );
    while let Some(item) = stream.next().await {
        item.context("pull image")?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn spawn_container(
    docker: &Docker,
    name: &str,
    image: &str,
    entrypoint: Option<Vec<String>>,
    cmd: Option<Vec<String>>,
    env: Vec<String>,
    mounts: Vec<Mount>,
    network: Option<&str>,
    aliases: Vec<String>,
    workdir: Option<&str>,
    privileged: bool,
) -> Result<()> {
    let _ = docker
        .remove_container(
            name,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;

    let mut endpoints = HashMap::new();
    if let Some(net) = network {
        if !aliases.is_empty() {
            endpoints.insert(
                net.to_string(),
                EndpointSettings {
                    aliases: Some(aliases),
                    ..Default::default()
                },
            );
        }
    }
    let networking_config = if endpoints.is_empty() {
        None
    } else {
        Some(NetworkingConfig {
            endpoints_config: endpoints,
        })
    };

    let host_config = HostConfig {
        network_mode: network.map(|s| s.to_string()),
        mounts: Some(mounts),
        privileged: Some(privileged),
        auto_remove: Some(false),
        ..Default::default()
    };

    let cfg = Config {
        image: Some(image.to_string()),
        entrypoint,
        cmd,
        env: Some(env),
        working_dir: workdir.map(|s| s.to_string()),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        tty: Some(false),
        host_config: Some(host_config),
        networking_config,
        ..Default::default()
    };

    docker
        .create_container(
            Some(CreateContainerOptions {
                name,
                platform: None,
            }),
            cfg,
        )
        .await
        .context("create container")?;
    docker
        .start_container(name, None::<StartContainerOptions<String>>)
        .await
        .context("start container")?;
    Ok(())
}

async fn wait_for_services(
    docker: &Docker,
    services: &[(String, String)],
    timeout: Duration,
    trace: &mut Trace,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    for (name, image) in services {
        loop {
            let info = docker
                .inspect_container(name, None)
                .await
                .with_context(|| format!("inspect service {name}"))?;
            let running = info.state.as_ref().and_then(|s| s.running).unwrap_or(false);
            let logs = service_logs(docker, name).await.unwrap_or_default();
            let ready = running
                && (logs.contains("API listen")
                    || logs.contains("ready to accept")
                    || logs.contains("Daemon has completed")
                    || !looks_like_dind(image, &logs));
            if ready {
                break;
            }
            if !running {
                if let Some(err) = info.state.as_ref().and_then(|s| s.error.clone()) {
                    if !err.is_empty() {
                        trace
                            .append_line(&format!("ERROR: service {name} failed: {err}"))
                            .await?;
                    }
                }
            }
            if Instant::now() >= deadline {
                if running {
                    trace
                        .append_line(&format!(
                            "WARNING: service {name} wait timed out; continuing"
                        ))
                        .await?;
                    break;
                }
                bail!("service {name} did not become ready");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    Ok(())
}

fn looks_like_dind(image: &str, logs: &str) -> bool {
    let lower = image.to_ascii_lowercase();
    lower.contains("dind") || lower.contains("docker") || logs.contains("dockerd") || logs.contains("containerd")
}

async fn service_logs(docker: &Docker, name: &str) -> Result<String> {
    let mut stream = docker.logs(
        name,
        Some(LogsOptions::<String> {
            follow: false,
            stdout: true,
            stderr: true,
            tail: "80".into(),
            ..Default::default()
        }),
    );
    let mut out = String::new();
    while let Some(item) = stream.next().await {
        if let Ok(chunk) = item {
            out.push_str(&chunk.to_string());
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
async fn run_container(
    docker: &Docker,
    name: &str,
    image: &str,
    entrypoint: Option<Vec<String>>,
    cmd: Option<Vec<String>>,
    env: Vec<String>,
    mounts: Vec<Mount>,
    network: Option<&str>,
    workdir: &str,
    privileged: bool,
    timeout: Duration,
    trace: &mut Trace,
) -> Result<i32> {
    spawn_container(
        docker,
        name,
        image,
        entrypoint,
        cmd,
        env,
        mounts,
        network,
        Vec::new(),
        Some(workdir),
        privileged,
    )
    .await?;

    let mut logs = docker.logs(
        name,
        Some(LogsOptions::<String> {
            follow: true,
            stdout: true,
            stderr: true,
            ..Default::default()
        }),
    );

    let wait_fut = async {
        let mut wait = docker.wait_container(name, None::<WaitContainerOptions<String>>);
        match wait.next().await {
            Some(Ok(status)) => Ok(status.status_code as i32),
            Some(Err(err)) => Err(anyhow::anyhow!("wait container: {err}")),
            None => Ok(0),
        }
    };

    let logs_fut = async {
        while let Some(item) = logs.next().await {
            match item {
                Ok(chunk) => {
                    let text = chunk.to_string();
                    if let Err(err) = trace.append(&text).await {
                        warn!(error = %err, "trace append failed");
                    }
                }
                Err(err) => {
                    warn!(error = %err, "container logs stream");
                    break;
                }
            }
        }
        let _ = trace.flush().await;
    };

    let timed = tokio::time::timeout(timeout, async {
        tokio::join!(wait_fut, logs_fut)
    })
    .await;

    let status = match timed {
        Ok((wait_res, _)) => wait_res?,
        Err(_) => {
            warn!(name, "job timed out, killing container");
            let _ = docker
                .kill_container(name, None::<KillContainerOptions<String>>)
                .await;
            trace.append_line("ERROR: job timed out").await?;
            124
        }
    };

    let _ = docker
        .remove_container(
            name,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;

    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_script_runs_after_on_success() {
        let steps = vec![
            Step {
                name: "script".into(),
                script: vec!["echo ok".into()],
                when: "on_success".into(),
                ..Default::default()
            },
            Step {
                name: "after_script".into(),
                script: vec!["echo after".into()],
                when: "always".into(),
                ..Default::default()
            },
        ];
        let script = render_script(&steps);
        assert!(script.contains("echo ok"));
        assert!(script.contains("echo after"));
        assert!(script.contains("exit $__exit"));
    }

    #[test]
    fn service_aliases_include_image_and_explicit_alias() {
        let svc = Image {
            name: "docker:24.0.9-dind".into(),
            alias_name: "dind".into(),
            ..Default::default()
        };
        let aliases = service_aliases(&svc, 0);
        assert!(aliases.contains(&"dind".into()));
        assert!(aliases.contains(&"docker".into()));
        assert!(aliases.contains(&"svc-0".into()));
    }

    #[test]
    fn bind_mounts_skip_cache_and_keep_ro() {
        let mounts = bind_mounts(&[
            "/cache".into(),
            "/etc/docker/daemon.json:/etc/docker/daemon.json:ro".into(),
        ]);
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].source.as_deref(), Some("/etc/docker/daemon.json"));
        assert_eq!(mounts[0].read_only, Some(true));
    }
}
