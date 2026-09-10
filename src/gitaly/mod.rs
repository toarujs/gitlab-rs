pub mod sidechannel;

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

pub mod gitaly {
    tonic::include_proto!("gitaly");
}

use gitaly::{
    blob_service_client::BlobServiceClient,
    commit_service_client::CommitServiceClient,
    diff_service_client::DiffServiceClient,
    repository_service_client::RepositoryServiceClient,
    smart_http_service_client::SmartHttpServiceClient,
    FindCommitRequest, GetArchiveRequest, GetBlobRequest, GetSnapshotRequest,
    InfoRefsRequest, LastCommitForPathRequest,
    PostReceivePackRequest, PostUploadPackRequest, PostUploadPackWithSidechannelRequest,
    RawDiffRequest, RawPatchRequest, Repository, TreeEntryRequest,
};
use tonic::transport::Channel;

#[derive(Debug, Clone)]
pub struct GitalyServer {
    pub address: String,
    pub token: String,
    #[allow(dead_code)]
    pub call_metadata: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct RepoInfo {
    pub storage_name: String,
    pub relative_path: String,
    pub gl_project_path: String,
    pub gl_repository: String,
}

impl RepoInfo {
    pub fn new(storage_name: &str, relative_path: &str, gl_project_path: &str, gl_repository: &str) -> Self {
        Self {
            storage_name: storage_name.to_string(),
            relative_path: relative_path.to_string(),
            gl_project_path: gl_project_path.to_string(),
            gl_repository: gl_repository.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BlobMeta {
    pub oid: String,
    pub size: i64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct TreeEntryBlob {
    pub object_type: i32,
    pub oid: String,
    pub size: i64,
    pub mode: i32,
    pub data: Vec<u8>,
}

fn apply_gitaly_auth<T>(req: &mut tonic::Request<T>, token: &str) {
    if token.is_empty() {
        return;
    }
    if let Ok(val) = format!("Bearer {}", token).parse() {
        req.metadata_mut().insert("authorization", val);
    }
}

pub struct GitalyClient {
    smart_http: SmartHttpServiceClient<Channel>,
    repository: RepositoryServiceClient<Channel>,
    blob: BlobServiceClient<Channel>,
    commit: CommitServiceClient<Channel>,
    diff: DiffServiceClient<Channel>,
    server: GitalyServer,
}

impl GitalyClient {
    pub async fn connect(server: &GitalyServer) -> io::Result<Self> {
        let channel = Self::create_grpc_channel(server).await?;

        Ok(Self {
            smart_http: SmartHttpServiceClient::new(channel.clone()),
            repository: RepositoryServiceClient::new(channel.clone()),
            blob: BlobServiceClient::new(channel.clone()),
            commit: CommitServiceClient::new(channel.clone()),
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

    fn apply_auth<T>(&self, req: &mut tonic::Request<T>) {
        apply_gitaly_auth(req, &self.server.token);
    }

    fn build_repo(&self, repo: &RepoInfo) -> Repository {
        Repository {
            storage_name: repo.storage_name.clone(),
            relative_path: repo.relative_path.clone(),
            gl_project_path: repo.gl_project_path.clone(),
            gl_repository: repo.gl_repository.clone(),
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
        self.apply_auth(&mut req);
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
        self.apply_auth(&mut req);
        let mut stream = self.smart_http.info_refs_receive_pack(req).await?.into_inner();
        let mut data = Vec::new();
        while let Some(chunk) = stream.message().await? {
            data.extend_from_slice(&chunk.data);
        }
        Ok(data)
    }

    #[allow(dead_code)]
    pub async fn post_upload_pack(
        &mut self,
        repo: &RepoInfo,
        data: Vec<u8>,
    ) -> Result<Vec<u8>, tonic::Status> {
        let mut req = tonic::Request::new(PostUploadPackRequest {
            repository: Some(self.build_repo(repo)),
            data,
            ..Default::default()
        });
        self.apply_auth(&mut req);
        let mut stream = self.smart_http.post_upload_pack(req).await?.into_inner();
        let mut result = Vec::new();
        while let Some(chunk) = stream.message().await? {
            result.extend_from_slice(&chunk.data);
        }
        Ok(result)
    }

    pub async fn post_upload_pack_with_sidechannel(
        &mut self,
        repo: &RepoInfo,
        request_body: Vec<u8>,
    ) -> Result<Vec<u8>, tonic::Status> {
        // First, establish yamux session for sidechannel data transfer
        tracing::info!("yamux: connecting to Gitaly at {}", self.server.address);
        let session = sidechannel::YamuxSession::connect(&self.server.address).await
            .map_err(|e| tonic::Status::internal(format!("yamux connect: {}", e)))?;
        tracing::info!("yamux: connected");

        tracing::info!("yamux: registering sidechannel");
        let (key_hex, rx) = session.register_sidechannel().await
            .map_err(|e| tonic::Status::internal(format!("sidechannel register: {}", e)))?;
        tracing::info!("yamux: sidechannel registered, key={}", key_hex);

        // Create a gRPC channel that uses the yamux stream as transport
        let yamux_channel = session.create_grpc_channel()
            .await
            .map_err(|e| tonic::Status::internal(format!("create grpc channel: {}", e)))?;
        
        // Create a new SmartHttpServiceClient that uses the yamux channel
        let mut yamux_smart_http = SmartHttpServiceClient::new(yamux_channel);
        
        // Prepare gRPC request
        let mut req = tonic::Request::new(PostUploadPackWithSidechannelRequest {
            repository: Some(self.build_repo(repo)),
        });
        self.apply_auth(&mut req);
        req.metadata_mut().insert(
            "gitaly-sidechannel-id",
            key_hex.parse().unwrap(),
        );

        // Spawn gRPC call in a separate task so it doesn't block sidechannel handling
        let grpc_handle = tokio::spawn(async move {
            tracing::info!("yamux: calling gRPC PostUploadPackWithSidechannel");
            yamux_smart_http.post_upload_pack_with_sidechannel(req).await
        });

        // Wait for sidechannel stream to be ready
        tracing::info!("yamux: waiting for sidechannel stream");
        let mut sidechannel = rx.await
            .map_err(|_| tonic::Status::internal("sidechannel stream not received"))?;

        // Write request body to sidechannel using pktline framing
        // Gitaly's ServerConn.Read() strips pktline framing before passing to git-upload-pack
        tracing::debug!("yamux: writing {} bytes to sidechannel (pktline framed)", request_body.len());
        sidechannel.write_pktline_framed(&request_body).await
            .map_err(|e| tonic::Status::internal(format!("sidechannel write: {}", e)))?;
        tracing::info!("yamux: request body written, flushing");
        
        // Flush to ensure data is sent
        sidechannel.flush().await
            .map_err(|e| tonic::Status::internal(format!("sidechannel flush: {}", e)))?;

        // Close write side to signal EOF to Gitaly
        // ClientConn.CloseWrite() sends a flush packet "0000"
        sidechannel.close_write().await
            .map_err(|e| tonic::Status::internal(format!("sidechannel close_write: {}", e)))?;

        // Read pack data from sidechannel as raw data
        // Gitaly writes raw bytes via ServerConn.Write()
        let mut pack_data = Vec::new();
        sidechannel.read_to_end(&mut pack_data).await
            .map_err(|e| tonic::Status::internal(format!("sidechannel read: {}", e)))?;
        tracing::info!("yamux: received {} bytes from sidechannel", pack_data.len());

        // Wait for gRPC call to complete
        let _grpc_result = grpc_handle.await
            .map_err(|_| tonic::Status::internal("gRPC task failed"))??;
        
        tracing::info!("yamux: gRPC call completed, pack_data={} bytes", pack_data.len());

        Ok(pack_data)
    }

    pub async fn post_receive_pack(
        &mut self,
        repo: &RepoInfo,
        data: Vec<u8>,
        gl_id: &str,
        gl_username: &str,
    ) -> Result<Vec<u8>, tonic::Status> {
        let repo = self.build_repo(repo);
        tracing::info!(
            "post_receive_pack: repo={:?}, gl_id={}, gl_username={}, data_len={}",
            repo,
            gl_id,
            gl_username,
            data.len(),
        );
        let header = PostReceivePackRequest {
            repository: Some(repo.clone()),
            data: vec![],
            gl_id: gl_id.to_string(),
            gl_repository: repo.gl_repository.clone(),
            gl_username: gl_username.to_string(),
            ..Default::default()
        };

        let body = PostReceivePackRequest {
            data: data.clone(),
            ..Default::default()
        };

        let stream = tokio_stream::iter(vec![header, body]);
        let mut req = tonic::Request::new(stream);
        self.apply_auth(&mut req);

        let mut response_stream = self.smart_http.post_receive_pack(req).await?.into_inner();
        let mut response_data = Vec::new();
        while let Some(chunk) = response_stream.message().await? {
            response_data.extend_from_slice(&chunk.data);
        }
        tracing::info!(
            "post_receive_pack response: {} bytes",
            response_data.len()
        );
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
        self.apply_auth(&mut req);
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
        self.apply_auth(&mut req);
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
        self.apply_auth(&mut req);
        let mut stream = self.blob.get_blob(req).await?.into_inner();
        let mut data = Vec::new();
        while let Some(chunk) = stream.message().await? {
            data.extend_from_slice(&chunk.data);
        }
        Ok(data)
    }

    pub async fn get_blob_with_meta(
        &mut self,
        repo: &RepoInfo,
        oid: &str,
        limit: i64,
    ) -> Result<BlobMeta, tonic::Status> {
        let mut req = tonic::Request::new(GetBlobRequest {
            repository: Some(self.build_repo(repo)),
            oid: oid.to_string(),
            limit,
        });
        self.apply_auth(&mut req);
        let mut stream = self.blob.get_blob(req).await?.into_inner();
        let mut meta = BlobMeta {
            oid: String::new(),
            size: 0,
            data: Vec::new(),
        };
        while let Some(chunk) = stream.message().await? {
            if meta.size == 0 && chunk.size != 0 {
                meta.size = chunk.size;
            }
            if meta.oid.is_empty() && !chunk.oid.is_empty() {
                meta.oid = chunk.oid;
            }
            meta.data.extend_from_slice(&chunk.data);
        }
        if meta.size == 0 {
            meta.size = meta.data.len() as i64;
        }
        Ok(meta)
    }

    pub async fn tree_entry(
        &mut self,
        repo: &RepoInfo,
        revision: &str,
        path: &str,
        max_size: i64,
    ) -> Result<TreeEntryBlob, tonic::Status> {
        let mut req = tonic::Request::new(TreeEntryRequest {
            repository: Some(self.build_repo(repo)),
            revision: revision.as_bytes().to_vec(),
            path: path.as_bytes().to_vec(),
            limit: 0,
            max_size,
        });
        self.apply_auth(&mut req);
        let mut stream = self.commit.tree_entry(req).await?.into_inner();
        let mut entry = TreeEntryBlob {
            object_type: 0,
            oid: String::new(),
            size: 0,
            mode: 0,
            data: Vec::new(),
        };
        while let Some(chunk) = stream.message().await? {
            if !chunk.oid.is_empty() {
                entry.oid = chunk.oid;
            }
            if chunk.size != 0 {
                entry.size = chunk.size;
            }
            if chunk.mode != 0 {
                entry.mode = chunk.mode;
            }
            let ty = chunk.r#type as i32;
            if ty != 0 {
                entry.object_type = ty;
            }
            entry.data.extend_from_slice(&chunk.data);
        }
        Ok(entry)
    }

    pub async fn find_commit_id(
        &mut self,
        repo: &RepoInfo,
        revision: &str,
    ) -> Result<Option<String>, tonic::Status> {
        let mut req = tonic::Request::new(FindCommitRequest {
            repository: Some(self.build_repo(repo)),
            revision: revision.as_bytes().to_vec(),
            trailers: false,
        });
        self.apply_auth(&mut req);
        let resp = self.commit.find_commit(req).await?.into_inner();
        Ok(resp.commit.map(|c| c.id).filter(|s| !s.is_empty()))
    }

    pub async fn last_commit_id_for_path(
        &mut self,
        repo: &RepoInfo,
        revision: &str,
        path: &str,
    ) -> Result<Option<String>, tonic::Status> {
        let mut req = tonic::Request::new(LastCommitForPathRequest {
            repository: Some(self.build_repo(repo)),
            revision: revision.as_bytes().to_vec(),
            path: path.as_bytes().to_vec(),
            literal_pathspec: true,
        });
        self.apply_auth(&mut req);
        let resp = self.commit.last_commit_for_path(req).await?.into_inner();
        Ok(resp.commit.map(|c| c.id).filter(|s| !s.is_empty()))
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
        self.apply_auth(&mut req);
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
        self.apply_auth(&mut req);
        let mut stream = self.diff.raw_patch(req).await?.into_inner();
        let mut data = Vec::new();
        while let Some(chunk) = stream.message().await? {
            data.extend_from_slice(&chunk.data);
        }
        Ok(data)
    }
}

#[allow(dead_code)]
pub struct GitalyPool {
    clients: Arc<Mutex<HashMap<String, Arc<Mutex<GitalyClient>>>>>,
    server: GitalyServer,
}

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

    #[test]
    fn test_apply_gitaly_auth_skips_empty_token() {
        let mut req = tonic::Request::new(());
        apply_gitaly_auth(&mut req, "");
        assert!(req.metadata().get("authorization").is_none());
    }

    #[test]
    fn test_apply_gitaly_auth_sets_bearer() {
        let mut req = tonic::Request::new(());
        apply_gitaly_auth(&mut req, "secret");
        assert_eq!(
            req.metadata().get("authorization").unwrap().to_str().unwrap(),
            "Bearer secret"
        );
    }
}
