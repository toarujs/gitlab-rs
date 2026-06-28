use std::collections::HashMap;
use std::path::PathBuf;
use tonic::transport::{Channel, Endpoint, Uri};

pub mod gitaly {
    tonic::include_proto!("gitaly");
}

use gitaly::smart_http_service_client::SmartHttpServiceClient;
use gitaly::{PostUploadPackWithSidechannelRequest, PostReceivePackRequest, Repository};

#[derive(Debug, Clone)]
pub struct GitalyServer {
    pub address: String,
    pub token: String,
    pub call_metadata: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct GitalyClient {
    client: SmartHttpServiceClient<Channel>,
    token: String,
}

impl GitalyClient {
    pub async fn connect(server: &GitalyServer) -> Result<Self, tonic::Status> {
        let channel = if server.address.starts_with("unix:") {
            let path = server.address.trim_start_matches("unix:");
            Endpoint::try_from("http://[::1]:0")
                .map_err(|e| tonic::Status::internal(format!("endpoint error: {}", e)))?
                .connect_with_connector(tower::service_fn(move |_: Uri| {
                    let path = path.to_string();
                    async move {
                        tokio::net::UnixStream::connect(path).await
                            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
                    }
                }))
                .await
                .map_err(|e| tonic::Status::internal(format!("unix connect error: {}", e)))?
        } else {
            let uri = if server.address.contains("://") {
                server.address.clone()
            } else {
                format!("http://{}", server.address)
            };
            Channel::from_shared(uri)
                .map_err(|e| tonic::Status::internal(format!("channel error: {}", e)))?
                .connect()
                .await
                .map_err(|e| tonic::Status::internal(format!("tcp connect error: {}", e)))?
        };

        let client = SmartHttpServiceClient::new(channel);

        Ok(Self {
            client,
            token: server.token.clone(),
        })
    }

    fn build_request<T>(&self, mut msg: T) -> tonic::Request<T> {
        let mut req = tonic::Request::new(msg);
        req.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", self.token).parse().unwrap(),
        );
        req
    }

    pub async fn post_upload_pack(
        &mut self,
        storage_name: &str,
        relative_path: &str,
    ) -> Result<Vec<u8>, tonic::Status> {
        let repo = Repository {
            storage_name: storage_name.to_string(),
            relative_path: relative_path.to_string(),
            ..Default::default()
        };
        let request = PostUploadPackWithSidechannelRequest {
            repository: Some(repo),
        };
        let response = self.client
            .post_upload_pack_with_sidechannel(self.build_request(request))
            .await?;
        Ok(response.into_inner().data)
    }

    pub async fn post_receive_pack(
        &mut self,
        storage_name: &str,
        relative_path: &str,
        data: Vec<u8>,
    ) -> Result<Vec<u8>, tonic::Status> {
        let repo = Repository {
            storage_name: storage_name.to_string(),
            relative_path: relative_path.to_string(),
            ..Default::default()
        };
        let request = PostReceivePackRequest {
            repository: Some(repo),
            data,
        };
        let response = self.client
            .post_receive_pack(self.build_request(request))
            .await?;
        Ok(response.into_inner().data)
    }
}

pub fn parse_gitaly_address(address: &str) -> Option<(String, u16)> {
    if address.starts_with("unix:") {
        return Some((address.to_string(), 0));
    }
    let parts: Vec<&str> = address.rsplitn(2, ':').collect();
    if parts.len() == 2 {
        let host = parts[1].to_string();
        if let Ok(port) = parts[0].parse::<u16>() {
            return Some((host, port));
        }
    }
    Some((address.to_string(), 8075))
}

pub fn resolve_repo_path(gitaly_repo: &Repository) -> Result<PathBuf, std::io::Error> {
    let relative = &gitaly_repo.relative_path;
    let default_path = format!("/var/opt/gitlab/git-data/repositories/{}", relative);
    let repo_path = PathBuf::from(&default_path);
    if repo_path.exists() {
        return Ok(repo_path);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("repository not found: {}", default_path),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_gitaly_address() {
        let result = parse_gitaly_address("localhost:8075");
        assert!(result.is_some());
        let (host, port) = result.unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 8075);
    }

    #[test]
    fn test_parse_gitaly_address_unix() {
        let result = parse_gitaly_address("unix:/var/opt/gitlab/gitaly/gitaly.socket");
        assert!(result.is_some());
        let (path, port) = result.unwrap();
        assert_eq!(path, "unix:/var/opt/gitlab/gitaly/gitaly.socket");
        assert_eq!(port, 0);
    }

    #[test]
    fn test_parse_gitaly_address_default_port() {
        let result = parse_gitaly_address("gitaly.internal");
        assert!(result.is_some());
        let (host, port) = result.unwrap();
        assert_eq!(host, "gitaly.internal");
        assert_eq!(port, 8075);
    }
}
