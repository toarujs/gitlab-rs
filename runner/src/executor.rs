use anyhow::{bail, Context, Result};
use bollard::container::{
    Config, CreateContainerOptions, KillContainerOptions, LogsOptions, RemoveContainerOptions,
    StartContainerOptions,
    WaitContainerOptions,
};
use bollard::image::CreateImageOptions;
use bollard::models::{HostConfig, Mount, MountTypeEnum};
use bollard::volume::{CreateVolumeOptions, RemoveVolumeOptions};
use bollard::Docker;
use futures_util::StreamExt;
use std::time::Duration;
use tracing::{info, warn};

use crate::api::{job_var, JobResponse, Step};
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
    trace: &mut Trace,
) -> Result<JobOutcome> {
    if !job.services.is_empty() {
        trace
            .append_line("ERROR: service containers are not implemented")
            .await?;
        return Ok(fail(1, "runner_system_failure"));
    }
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

    let result = run_job_inner(docker, job, docker_cfg, clone_base, skip_clone, &volume, trace).await;

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
    volume: &str,
    trace: &mut Trace,
) -> Result<JobOutcome> {
    let network = docker_cfg.network_mode.clone();
    let timeout = if job.runner_info.timeout > 0 {
        Duration::from_secs(job.runner_info.timeout as u64)
    } else {
        Duration::from_secs(3600)
    };

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
            vec![git_script],
            git_env,
            volume,
            network.as_deref(),
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
    let env = job_env(job);
    let job_name = format!("runner-{}-job", job.id);
    let entrypoint = if job.image.entrypoint.is_empty() {
        Some(vec!["/bin/sh".into(), "-c".into()])
    } else {
        Some(job.image.entrypoint.clone())
    };
    let cmd = if job.image.command.is_empty() {
        vec![script]
    } else {
        job.image.command.clone()
    };

    let status = run_container(
        docker,
        &job_name,
        &image,
        entrypoint,
        cmd,
        env,
        volume,
        network.as_deref(),
        BUILDS_DIR,
        docker_cfg.privileged,
        timeout,
        trace,
    )
    .await?;
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

fn job_env(job: &JobResponse) -> Vec<String> {
    let mut env = Vec::new();
    for var in &job.variables {
        if var.file {
            continue;
        }
        env.push(format!("{}={}", var.key, var.value));
    }
    env.push(format!("CI_PROJECT_DIR={BUILDS_DIR}"));
    env
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
async fn run_container(
    docker: &Docker,
    name: &str,
    image: &str,
    entrypoint: Option<Vec<String>>,
    cmd: Vec<String>,
    env: Vec<String>,
    volume: &str,
    network: Option<&str>,
    workdir: &str,
    privileged: bool,
    timeout: Duration,
    trace: &mut Trace,
) -> Result<i32> {
    let _ = docker
        .remove_container(
            name,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;

    let mut mounts = vec![Mount {
        target: Some(BUILDS_DIR.into()),
        source: Some(volume.into()),
        typ: Some(MountTypeEnum::VOLUME),
        ..Default::default()
    }];
    if workdir == BUILDS_DIR {
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
    }

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
        cmd: Some(cmd),
        env: Some(env),
        working_dir: Some(workdir.into()),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        tty: Some(false),
        host_config: Some(host_config),
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

    if workdir == BUILDS_DIR {
        let _ = docker
            .remove_volume(
                &format!("{volume}-cache"),
                Some(RemoveVolumeOptions { force: true }),
            )
            .await;
    }

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
}
