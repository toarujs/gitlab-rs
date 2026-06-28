pub mod sidechannel;

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use sidechannel::GitalyConnection;

pub mod gitaly {
    tonic::include_proto!("gitaly");
}

use gitaly::{
    blob_service_client::BlobServiceClient,
    diff_service_client::DiffServiceClient,
    repository_service_client::RepositoryServiceClient,
    smart_http_service_client::SmartHttpServiceClient,
    GetArchiveRequest, GetBlobRequest, GetSnapshotRequest,
    InfoRefsRequest, InfoRefsResponse,
    PostReceivePackRequest, PostReceivePackResponse,
    PostUploadPackWithSidechannelRequest,
    RawDiffRequest, RawPatchRequest,
    Repository,
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

type Channel = tonic::transport::Channel;

pub struct GitalyClient {
    sidechannel_conn: GitalyConnection,
    smart_http: SmartHttpServiceClient<Channel>,
    repository: RepositoryServiceClient<Channel>,
    blob: BlobServiceClient<Channel>,
    diff: DiffServiceClient<Channel>,
    server: GitalyServer,
}

impl GitalyClient {
    pub async fn connect(server: &GitalyServer) -> io::Result<Self> {
        let channel = Self::create_grpc_channel(server).await?;
        let sidechannel_conn = Self::create_sidechannel_conn(server).await?;

        Ok(Self {
            sidechannel_conn,
            smart_http: SmartHttpServiceClient::new(channel.clone()),
            repository: RepositoryServiceClient::new(channel.clone()),
            blob: BlobServiceClient::new(channel.clone()),
            diff: DiffServiceClient::new(channel),
            server: server.clone(),
        })
    }

    async fn create_grpc_channel(server: &GitalyServer) -> io::Result<Channel> {
        if server.address.starts_with("unix:") {
            let path = server.address.trim_start_matches("unix:").to_string();
            tonic::transport::Endpoint::try_from("http://[::1]:1")
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
                .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
                    let path = path.clone();
                    async move {
                        let stream = tokio::net::UnixStream::connect(path).await
                            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
                        Ok::<_, io::Error>(hyper_util::rt::TokioIo::new(stream))
                    }
                }))
                .await
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
        } else {
            tonic::transport::Endpoint::try_from(format!("http://{}", server.address))
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
                .connect()
                .await
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
        }
    }

    async fn create_sidechannel_conn(server: &GitalyServer) -> io::Result<GitalyConnection> {
        if server.address.starts_with("unix:") {
            let path = server.address.trim_start_matches("unix:");
            GitalyConnection::connect_unix(path).await
        } else {
            GitalyConnection::connect_tcp(&server.address).await
        }
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

    pub async fn info_refs_upload_pack(&mut self, repo: &RepoInfo) -> Result<Vec<u8>, tonic::Status> {
        let repository = self.build_repo(repo);
        tracing::info!(
            "Gitaly info_refs_upload_pack: storage={:?}, relative={:?}",
            repository.storage_name,
            repository.relative_path,
        );
        let mut req = tonic::Request::new(InfoRefsRequest {
            repository: Some(repository),
        });
        req.metadata_mut().insert("authorization", self.auth_token());
        let mut stream = self.smart_http.info_refs_upload_pack(req).await?.into_inner();
        let mut data = Vec::new();
        while let Some(chunk) = stream.message().await? {
            data.extend_from_slice(&chunk.data);
        }
        Ok(data)
    }

    pub async fn info_refs_receive_pack(&mut self, repo: &RepoInfo) -> Result<Vec<u8>, tonic::Status> {
        let mut req = tonic::Request::new(InfoRefsRequest {
            repository: Some(self.build_repo(repo)),
        });
        req.metadata_mut().insert("authorization", self.auth_token());
        let mut stream = self.smart_http.info_refs_receive_pack(req).await?.into_inner();
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
        let (reg_key, rx) = self.sidechannel_conn.register_sidechannel_waiter().await;
        let mut req = tonic::Request::new(PostUploadPackWithSidechannelRequest {
            repository: Some(self.build_repo(repo)),
        });
        req.metadata_mut().insert("authorization", self.auth_token());
        req.metadata_mut().insert(
            "gitaly-sidechannel",
            reg_key.parse().unwrap(),
        );
        self.smart_http.post_upload_pack_with_sidechannel(req).await?;
        rx.await.map_err(|_| tonic::Status::internal("sidechannel stream not received"))
    }

    pub async fn post_receive_pack(
        &mut self,
        repo: &RepoInfo,
        data: Vec<u8>,
        gl_id: &str,
        gl_username: &str,
    ) -> Result<Vec<u8>, tonic::Status> {
        let repo = self.build_repo(repo);
        let header = PostReceivePackRequest {
            repository: Some(repo),
            data: vec![],
            gl_id: gl_id.to_string(),
            gl_repository: "project-1".to_string(),
            gl_username: gl_username.to_string(),
            ..Default::default()
        };
        let body = PostReceivePackRequest {
            repository: None,
            data,
            ..Default::default()
        };

        let stream = tokio_stream::iter(vec![header, body]);
        let mut req = tonic::Request::new(stream);
        req.metadata_mut().insert("authorization", self.auth_token());

        let response = self.smart_http.post_receive_pack(req).await?;
        let response_data = response.into_inner().data;
        Ok(response_data)
    }

    pub async fn get_archive(
        &mut self,
        repo: &RepoInfo,
        commit_id: &str,
        format: &str,
        prefix: &str,
        path: &str,
    ) -> Result<Vec<u8>, tonic::Status> {
        let mut req = tonic::Request::new(GetArchiveRequest {
            repository: Some(self.build_repo(repo)),
            commit_id: commit_id.to_string(),
            format: format.to_string(),
            prefix: prefix.to_string(),
            path: path.to_string(),
            ..Default::default()
        });
        req.metadata_mut().insert("authorization", self.auth_token());
        let mut stream = self.repository.get_archive(req).await?.into_inner();
        let mut data = Vec::new();
        while let Some(chunk) = stream.message().await? {
            data.extend_from_slice(&chunk.data);
        }
        Ok(data)
    }

    pub async fn get_snapshot(
        &mut self,
        repo: &RepoInfo,
        commit_id: &str,
    ) -> Result<Vec<u8>, tonic::Status> {
        let mut req = tonic::Request::new(GetSnapshotRequest {
            repository: Some(self.build_repo(repo)),
            commit_id: commit_id.to_string(),
        });
        req.metadata_mut().insert("authorization", self.auth_token());
        let mut stream = self.repository.get_snapshot(req).await?.into_inner();
        let mut data = Vec::new();
        while let Some(chunk) = stream.message().await? {
            data.extend_from_slice(&chunk.data);
        }
        Ok(data)
    }

    pub async fn get_blob(
        &mut self,
        repo: &RepoInfo,
        oid: &str,
        limit: i64,
    ) -> Result<Vec<u8>, tonic::Status> {
        let mut req = tonic::Request::new(GetBlobRequest {
            repository: Some(self.build_repo(repo)),
            oid: oid.to_string(),
            limit,
        });
        req.metadata_mut().insert("authorization", self.auth_token());
        let mut stream = self.blob.get_blob(req).await?.into_inner();
        let mut data = Vec::new();
        while let Some(chunk) = stream.message().await? {
            data.extend_from_slice(&chunk.data);
        }
        Ok(data)
    }

    pub async fn raw_diff(
        &mut self,
        repo: &RepoInfo,
        left_commit_id: &str,
        right_commit_id: &str,
    ) -> Result<Vec<u8>, tonic::Status> {
        let mut req = tonic::Request::new(RawDiffRequest {
            repository: Some(self.build_repo(repo)),
            left_commit_id: left_commit_id.to_string(),
            right_commit_id: right_commit_id.to_string(),
        });
        req.metadata_mut().insert("authorization", self.auth_token());
        let mut stream = self.diff.raw_diff(req).await?.into_inner();
        let mut data = Vec::new();
        while let Some(chunk) = stream.message().await? {
            data.extend_from_slice(&chunk.data);
        }
        Ok(data)
    }

    pub async fn raw_patch(
        &mut self,
        repo: &RepoInfo,
        left_commit_id: &str,
        right_commit_id: &str,
    ) -> Result<Vec<u8>, tonic::Status> {
        let mut req = tonic::Request::new(RawPatchRequest {
            repository: Some(self.build_repo(repo)),
            left_commit_id: left_commit_id.to_string(),
            right_commit_id: right_commit_id.to_string(),
        });
        req.metadata_mut().insert("authorization", self.auth_token());
        let mut stream = self.diff.raw_patch(req).await?.into_inner();
        let mut data = Vec::new();
        while let Some(chunk) = stream.message().await? {
            data.extend_from_slice(&chunk.data);
        }
        Ok(data)
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
