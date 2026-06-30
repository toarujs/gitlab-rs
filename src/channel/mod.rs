#![allow(dead_code, unused_imports)]
use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::HeaderMap,
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite;
use tracing::{error, info};
use url::Url;

pub struct ChannelHandler {
    pub subprotocols: Vec<String>,
    pub url: String,
    pub headers: HeaderMap,
    pub ca_pem: String,
    pub max_session_time: u64,
}

impl ChannelHandler {
    pub fn new(url: String, subprotocols: Vec<String>) -> Self {
        Self {
            subprotocols,
            url,
            headers: HeaderMap::new(),
            ca_pem: String::new(),
            max_session_time: 0,
        }
    }

    pub fn with_headers(mut self, headers: HeaderMap) -> Self {
        self.headers = headers;
        self
    }

    pub fn with_ca_pem(mut self, pem: String) -> Self {
        self.ca_pem = pem;
        self
    }

    pub fn with_max_session_time(mut self, seconds: u64) -> Self {
        self.max_session_time = seconds;
        self
    }

    pub async fn handle_upgrade(self, ws: WebSocketUpgrade) -> Response {
        let protocols = if self.subprotocols.is_empty() {
            vec!["terminal.gitlab.com".to_string()]
        } else {
            self.subprotocols.clone()
        };

        ws.protocols(protocols)
            .on_upgrade(move |socket| self.proxy_websocket(socket))
    }

    async fn proxy_websocket(self, client_socket: WebSocket) {
        let target_url = match Url::parse(&self.url) {
            Ok(url) => url,
            Err(e) => {
                error!("Invalid channel URL: {}", e);
                return;
            }
        };

        let ws_url = format!(
            "{}://{}{}",
            if target_url.scheme() == "wss" { "wss" } else { "ws" },
            target_url.host_str().unwrap_or("localhost"),
            target_url.path()
        );

        let mut request = tungstenite::http::Request::builder()
            .uri(&ws_url)
            .header("Host", target_url.host_str().unwrap_or("localhost"));

        for (key, value) in self.headers.iter() {
            if let Ok(v) = value.to_str() {
                request = request.header(key.as_str(), v);
            }
        }

        let request = match request.body(()) {
            Ok(req) => req,
            Err(e) => {
                error!("Failed to build WebSocket request: {}", e);
                return;
            }
        };

        let (backend_ws, _response) = match connect_async(request).await {
            Ok(conn) => conn,
            Err(e) => {
                error!("Failed to connect to backend WebSocket: {}", e);
                return;
            }
        };

        let (mut backend_sink, mut backend_stream) = backend_ws.split();
        let (mut client_sink, mut client_stream) = client_socket.split();

        let client_to_backend = {
            tokio::spawn(async move {
                while let Some(msg) = client_stream.next().await {
                    match msg {
                        Ok(Message::Text(text)) => {
                            if backend_sink
                                .send(tokio_tungstenite::tungstenite::Message::Text(
                                    text.to_string(),
                                ))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Ok(Message::Binary(data)) => {
                            if backend_sink
                                .send(tokio_tungstenite::tungstenite::Message::Binary(data.to_vec()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Ok(Message::Ping(_)) => {}
                        Ok(Message::Pong(_)) => {}
                        Ok(Message::Close(_)) => break,
                        Err(_) => break,
                    }
                }
            })
        };

        let backend_to_client = {
            tokio::spawn(async move {
                while let Some(msg) = backend_stream.next().await {
                    match msg {
                        Ok(tungstenite::Message::Text(text)) => {
                            if client_sink.send(Message::Text(text.into())).await.is_err() {
                                break;
                            }
                        }
                        Ok(tungstenite::Message::Binary(data)) => {
                            if client_sink.send(Message::Binary(data.into())).await.is_err() {
                                break;
                            }
                        }
                        Ok(tungstenite::Message::Ping(_)) => {}
                        Ok(tungstenite::Message::Pong(_)) => {}
                        Ok(tungstenite::Message::Close(_)) => {
                            let _ = client_sink.send(Message::Close(None)).await;
                            break;
                        }
                        Ok(tungstenite::Message::Frame(_)) => {}
                        Err(_) => break,
                    }
                }
            })
        };

        tokio::select! {
            _ = client_to_backend => {},
            _ = backend_to_client => {},
        }

        info!("WebSocket channel connection closed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_handler_new() {
        let handler = ChannelHandler::new(
            "wss://terminal.gitlab.com".to_string(),
            vec!["terminal.gitlab.com".to_string()],
        );
        assert_eq!(handler.url, "wss://terminal.gitlab.com");
        assert_eq!(handler.subprotocols.len(), 1);
    }

    #[test]
    fn test_channel_handler_with_options() {
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "Bearer token".parse().unwrap());

        let handler = ChannelHandler::new(
            "wss://terminal.gitlab.com".to_string(),
            vec!["terminal.gitlab.com".to_string()],
        )
        .with_headers(headers)
        .with_max_session_time(600);

        assert_eq!(handler.max_session_time, 600);
        assert!(handler.headers.contains_key("Authorization"));
    }
}
