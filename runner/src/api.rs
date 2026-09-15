use anyhow::{bail, Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_RANGE, CONTENT_TYPE};
use reqwest::StatusCode;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{RUNNER_REVISION, RUNNER_VERSION};

/// Must exceed workhorse `api_ci_long_polling_duration` (50s) plus margin.
pub const JOB_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(70);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ApiError {
    #[error("runner token rejected (HTTP 401)")]
    Unauthorized,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobRequest {
    pub info: VersionInfo,
    pub token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_update: Option<String>,
    pub system_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct VersionInfo {
    pub name: String,
    pub version: String,
    pub revision: String,
    pub platform: String,
    pub architecture: String,
    pub executor: String,
    pub shell: String,
    pub features: RunnerFeatures,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerFeatures {
    pub variables: bool,
    pub artifacts: bool,
    pub cache: bool,
    pub shared: bool,
    pub masking: bool,
    pub raw_variables: bool,
    pub refspecs: bool,
    pub multi_build_steps: bool,
    pub cancelable: bool,
}

impl Default for RunnerFeatures {
    fn default() -> Self {
        Self {
            variables: true,
            artifacts: false,
            cache: false,
            shared: false,
            masking: true,
            raw_variables: true,
            refspecs: true,
            multi_build_steps: true,
            cancelable: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct JobResponse {
    pub id: i64,
    #[serde(default, deserialize_with = "null_to_default")]
    pub token: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub job_info: JobInfo,
    #[serde(default, deserialize_with = "null_to_default")]
    pub git_info: GitInfo,
    #[serde(default, deserialize_with = "null_to_default")]
    pub runner_info: RunnerInfo,
    #[serde(default, deserialize_with = "null_to_default")]
    pub variables: Vec<JobVariable>,
    #[serde(default, deserialize_with = "null_to_default")]
    pub steps: Vec<Step>,
    #[serde(default, deserialize_with = "null_to_default")]
    pub image: Image,
    #[serde(default, deserialize_with = "null_to_default")]
    pub services: Vec<Image>,
}

/// Treat an explicit JSON `null` the same as a missing field, falling back to
/// `T::default()`.
///
/// GitLab's runner register payload sends `null` (not a missing key) for
/// optional sub-objects such as `image` and scalars such as
/// `runner_info.timeout`, which plain `#[serde(default)]` cannot handle.
pub(crate) fn null_to_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct JobInfo {
    #[serde(default, deserialize_with = "null_to_default")]
    pub name: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub stage: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub project_id: i64,
    #[serde(default, deserialize_with = "null_to_default")]
    pub project_name: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct GitInfo {
    #[serde(default, deserialize_with = "null_to_default")]
    pub repo_url: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub ref_name: String,
    #[serde(default, rename = "ref", deserialize_with = "null_to_default")]
    pub git_ref: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub sha: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub before_sha: String,
}

impl GitInfo {
    pub fn refspec(&self) -> &str {
        if !self.git_ref.is_empty() {
            &self.git_ref
        } else {
            &self.ref_name
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RunnerInfo {
    #[serde(default, deserialize_with = "null_to_default")]
    pub timeout: i64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct JobVariable {
    #[serde(default, deserialize_with = "null_to_default")]
    pub key: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub value: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub public: bool,
    #[serde(default, deserialize_with = "null_to_default")]
    pub internal: bool,
    #[serde(default, deserialize_with = "null_to_default")]
    pub file: bool,
    #[serde(default, deserialize_with = "null_to_default")]
    pub masked: bool,
    #[serde(default, deserialize_with = "null_to_default")]
    pub raw: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Step {
    #[serde(default, deserialize_with = "null_to_default")]
    pub name: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub script: Vec<String>,
    #[serde(default, deserialize_with = "null_to_default")]
    pub timeout: i64,
    #[serde(default, deserialize_with = "null_to_default")]
    pub when: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub allow_failure: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Image {
    #[serde(default, deserialize_with = "null_to_default")]
    pub name: String,
    #[serde(default, rename = "alias", deserialize_with = "null_to_default")]
    pub alias_name: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub command: Vec<String>,
    #[serde(default, deserialize_with = "null_to_default")]
    pub entrypoint: Vec<String>,
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
}

impl Client {
    pub fn new(url: &str) -> Result<Self> {
        let base = url.trim_end_matches('/').to_string();
        let http = reqwest::Client::builder()
            .timeout(JOB_REQUEST_TIMEOUT)
            .build()
            .context("build http client")?;
        Ok(Self { http, base })
    }

    pub fn version_info(executor: &str) -> VersionInfo {
        VersionInfo {
            name: "gitlab-runner".into(),
            version: RUNNER_VERSION.into(),
            revision: RUNNER_REVISION.into(),
            platform: std::env::consts::OS.into(),
            architecture: std::env::consts::ARCH.into(),
            executor: executor.into(),
            shell: "sh".into(),
            features: RunnerFeatures::default(),
        }
    }

    pub async fn request_job(
        &self,
        token: &str,
        system_id: &str,
        last_update: &mut Option<String>,
    ) -> Result<Option<JobResponse>> {
        let body = JobRequest {
            info: Self::version_info("docker"),
            token: token.to_string(),
            last_update: last_update.clone(),
            system_id: system_id.to_string(),
        };
        let resp = self
            .http
            .post(format!("{}/api/v4/jobs/request", self.base))
            .json(&body)
            .send()
            .await
            .context("jobs/request")?;
        if let Some(hdr) = resp.headers().get("X-GitLab-Last-Update") {
            if let Ok(v) = hdr.to_str() {
                *last_update = Some(v.to_string());
            }
        }
        match resp.status() {
            StatusCode::NO_CONTENT => Ok(None),
            StatusCode::UNAUTHORIZED => Err(ApiError::Unauthorized.into()),
            StatusCode::CREATED | StatusCode::OK => {
                let job = resp.json::<JobResponse>().await.context("decode job")?;
                Ok(Some(job))
            }
            other => {
                let text = resp.text().await.unwrap_or_default();
                bail!("jobs/request HTTP {other}: {text}");
            }
        }
    }

    pub async fn update_state(
        &self,
        job_id: i64,
        job_token: &str,
        state: &str,
        exit_code: Option<i32>,
        failure_reason: Option<&str>,
    ) -> Result<()> {
        let mut map = serde_json::Map::new();
        map.insert("token".into(), serde_json::Value::String(job_token.into()));
        map.insert("state".into(), serde_json::Value::String(state.into()));
        if let Some(code) = exit_code {
            map.insert("exit_code".into(), serde_json::Value::from(code));
        }
        if let Some(reason) = failure_reason {
            map.insert(
                "failure_reason".into(),
                serde_json::Value::String(reason.into()),
            );
        }
        let resp = self
            .http
            .put(format!("{}/api/v4/jobs/{job_id}", self.base))
            .json(&map)
            .send()
            .await
            .context("jobs update")?;
        if resp.status().is_success() || resp.status() == StatusCode::ACCEPTED {
            return Ok(());
        }
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        bail!("jobs update HTTP {status}: {text}");
    }

    pub async fn patch_trace(
        &self,
        job_id: i64,
        job_token: &str,
        offset: u64,
        body: &[u8],
    ) -> Result<u64> {
        if body.is_empty() {
            return Ok(offset);
        }
        let range = content_range(offset, body.len() as u64);
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        headers.insert(
            CONTENT_RANGE,
            HeaderValue::from_str(&range).context("content-range header")?,
        );
        headers.insert(
            "JOB-TOKEN",
            HeaderValue::from_str(job_token).context("job-token header")?,
        );
        let resp = self
            .http
            .patch(format!("{}/api/v4/jobs/{job_id}/trace", self.base))
            .headers(headers)
            .body(body.to_vec())
            .send()
            .await
            .context("patch trace")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("trace HTTP {status}: {text}");
        }
        Ok(offset + body.len() as u64)
    }
}

/// Inclusive Content-Range used by GitLab: `start-end/total`.
pub fn content_range(offset: u64, body_len: u64) -> String {
    if body_len == 0 {
        return format!("{offset}-{offset}/{offset}");
    }
    let start = offset;
    let end = offset + body_len - 1;
    let total = offset + body_len;
    format!("{start}-{end}/{total}")
}

pub fn job_var<'a>(job: &'a JobResponse, key: &str) -> Option<&'a str> {
    job.variables
        .iter()
        .find(|v| v.key == key)
        .map(|v| v.value.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_range_first_chunk() {
        assert_eq!(content_range(0, 11), "0-10/11");
    }

    #[test]
    fn content_range_next_chunk() {
        assert_eq!(content_range(11, 5), "11-15/16");
    }

    #[test]
    fn job_request_timeout_exceeds_50s_poll() {
        assert!(JOB_REQUEST_TIMEOUT > std::time::Duration::from_secs(50));
        assert_eq!(JOB_REQUEST_TIMEOUT, std::time::Duration::from_secs(70));
    }

    #[test]
    fn decodes_null_optional_fields() {
        // GitLab sends explicit nulls for absent optional values.
        let payload = r#"{
            "id": 7,
            "token": "jobtoken",
            "image": null,
            "services": null,
            "variables": null,
            "steps": null,
            "job_info": null,
            "git_info": {"repo_url": "http://web/g/p.git", "ref": "main", "sha": null},
            "runner_info": {"timeout": null},
            "entrypoint": null
        }"#;
        let job: JobResponse = serde_json::from_str(payload).expect("decode null-tolerant job");
        assert_eq!(job.id, 7);
        assert_eq!(job.token, "jobtoken");
        assert!(job.services.is_empty());
        assert!(job.image.entrypoint.is_empty());
        assert_eq!(job.runner_info.timeout, 0);
        assert_eq!(job.git_info.sha, "");
        assert_eq!(job.git_info.refspec(), "main");
    }
}
