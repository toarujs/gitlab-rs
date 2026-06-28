pub mod sidechannel;

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::compat::FuturesAsyncReadCompatExt;

use sidechannel::GitalyConnection;

pub mod gitaly {
    tonic::include_proto!("gitaly");
}

use gitaly::smart_http_service_client::SmartHttpServiceClient;
use gitaly::{
    InfoRefsRequest, PostReceivePackRequest,
    PostUploadPackWithSidechannelRequest, Repository,
};

#[derive(Debug, Clone)]
pub struct GitalyServer {
    pub address: String,
    pub token: String,
    pub call_metadata: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct RepoInfo {
    pub storage_name: String,
    pub relative_path: String,
}

pub struct GitalyClient {
    conn: GitalyConnection,
    grpc: SmartHttpServiceClient<tonic::transport::Channel>,
    server: GitalyServer,
}

impl GitalyClient {
    pub async fn connect(server: &GitalyServer) -> io::Result<Self> {
        let conn = if server.address.starts_with("unix:") {
            let path = server.address.trim_start_matches("unix:");
            GitalyConnection::connect_unix(path).await?
        } else {
            GitalyConnection::connect_tcp(&server.address).await?
        };

        let control = conn.control();
        let channel = tonic::transport::Endpoint::try_from("http://gitaly.internal")
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
            .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
                let c = control.clone();
                async move {
                    let mut guard = c.lock().await;
                    let stream = guard.open_stream().await.map_err(|e| {
                        io::Error::new(io::ErrorKind::Other, e.to_string())
                    })?;
                    Ok::<_, io::Error>(stream.compat())
                }
            }))
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        let grpc = SmartHttpServiceClient::new(channel);

        Ok(Self {
            conn,
            grpc,
            server: server.clone(),
        })
    }

    fn auth_token(&self) -> tonic::metadata::MetadataValue<tonic::metadata::Ascii> {
        format!("Bearer {}", self.server.token)
            .parse()
            .unwrap()
    }

    fn build_repo(&self, repo: &RepoInfo) -> Repository {
        Repository {
            storage_name: repo.storage_name.clone(),
            relative_path: repo.relative_path.clone(),
            ..Default::default()
        }
    }

    pub async fn info_refs_upload_pack(
        &mut self,
        repo: &RepoInfo,
    ) -> Result<Vec<u8>, tonic::Status> {
        let mut req = tonic::Request::new(InfoRefsRequest {
            repository: Some(self.build_repo(repo)),
        });
        req.metadata_mut().insert("authorization", self.auth_token());

        let mut stream = self.grpc.info_refs_upload_pack(req).await?.into_inner();
        let mut data = Vec::new();
        while let Some(chunk) = stream.message().await? {
            data.extend_from_slice(&chunk.data);
        }
        Ok(data)
    }

    pub async fn info_refs_receive_pack(
        &mut self,
        repo: &RepoInfo,
    ) -> Result<Vec<u8>, tonic::Status> {
        let mut req = tonic::Request::new(InfoRefsRequest {
            repository: Some(self.build_repo(repo)),
        });
        req.metadata_mut().insert("authorization", self.auth_token());

        let mut stream = self.grpc.info_refs_receive_pack(req).await?.into_inner();
        let mut data = Vec::new();
        while let Some(chunk) = stream.message().await? {
            data.extend_from_slice(&chunk.data);
        }
        Ok(data)
    }

    pub async fn post_upload_pack_with_sidechannel(
        &mut self,
        repo: &RepoInfo,
    ) -> Result<sidechannel::Sidechannel, tonic::Status> {
        let (reg_key, rx) = self.conn.register_sidechannel_waiter().await;

        let mut req = tonic::Request::new(PostUploadPackWithSidechannelRequest {
            repository: Some(self.build_repo(repo)),
        });
        req.metadata_mut().insert("authorization", self.auth_token());
        req.metadata_mut().insert(
            "gitaly-sidechannel",
            reg_key.parse().unwrap(),
        );

        self.grpc.post_upload_pack_with_sidechannel(req).await?;

        rx.await.map_err(|_| {
            tonic::Status::internal("sidechannel stream was not received from Gitaly")
        })
    }

    pub async fn post_receive_pack(
        &mut self,
        repo: &RepoInfo,
        data: Vec<u8>,
    ) -> Result<Vec<u8>, tonic::Status> {
        let mut req = tonic::Request::new(PostReceivePackRequest {
            repository: Some(self.build_repo(repo)),
            data,
        });
        req.metadata_mut().insert("authorization", self.auth_token());

        let response = self.grpc.post_receive_pack(req).await?;
        Ok(response.into_inner().data)
    }
}

pub struct GitalyPool {
    clients: Arc<Mutex<HashMap<String, Arc<Mutex<GitalyClient>>>>>,
    server: GitalyServer,
}

impl GitalyPool {
    pub fn new(server: GitalyServer) -> Self {
        Self {
            clients: Arc::new(Mutex::new(HashMap::new())),
            server,
        }
    }

    pub async fn get(&self) -> io::Result<Arc<Mutex<GitalyClient>>> {
        let key = self.server.address.clone();
        let mut map = self.clients.lock().await;
        if let Some(client) = map.get(&key) {
            return Ok(client.clone());
        }
        let client = GitalyClient::connect(&self.server).await?;
        let client = Arc::new(Mutex::new(client));
        map.insert(key, client.clone());
        Ok(client)
    }

    pub async fn remove(&self) {
        let key = self.server.address.clone();
        self.clients.lock().await.remove(&key);
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
        let (host, port) = parse_gitaly_address("localhost:8075").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 8075);
    }

    #[test]
    fn test_parse_gitaly_address_unix() {
        let (path, port) = parse_gitaly_address("unix:/var/opt/gitlab/gitaly/gitaly.socket").unwrap();
        assert_eq!(path, "unix:/var/opt/gitlab/gitaly/gitaly.socket");
        assert_eq!(port, 0);
    }

    #[test]
    fn test_parse_gitaly_address_default_port() {
        let (host, port) = parse_gitaly_address("gitaly.internal").unwrap();
        assert_eq!(host, "gitaly.internal");
        assert_eq!(port, 8075);
    }
}
