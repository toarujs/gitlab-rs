use anyhow::Result;

use crate::api::Client;
use crate::mask;

pub struct Trace {
    client: Client,
    job_id: i64,
    job_token: String,
    offset: u64,
    buf: Vec<u8>,
    secrets: Vec<String>,
}

impl Trace {
    pub fn new(client: Client, job_id: i64, job_token: String, secrets: Vec<String>) -> Self {
        Self {
            client,
            job_id,
            job_token,
            offset: 0,
            buf: Vec::new(),
            secrets,
        }
    }

    pub async fn append(&mut self, text: &str) -> Result<()> {
        let masked = mask::mask_text(text, &self.secrets);
        self.buf.extend_from_slice(masked.as_bytes());
        if self.buf.len() >= 8 * 1024 {
            self.flush().await?;
        }
        Ok(())
    }

    pub async fn append_line(&mut self, text: &str) -> Result<()> {
        if text.ends_with('\n') {
            self.append(text).await
        } else {
            self.append(&format!("{text}\n")).await
        }
    }

    pub async fn flush(&mut self) -> Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::take(&mut self.buf);
        self.offset = self
            .client
            .patch_trace(self.job_id, &self.job_token, self.offset, &chunk)
            .await?;
        Ok(())
    }
}
