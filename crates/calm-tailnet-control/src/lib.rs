//! Only the five private-tailnet operations; not a generic neige-app admin bridge.
use calm_types::tailnet::{TailnetAction, TailnetRequest, TailnetResponse};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

pub const VERSION: u32 = 1;
pub const MAX_MESSAGE: u64 = 16_384;

#[derive(Debug, Clone)]
pub struct TailnetClient {
    socket: PathBuf,
}
impl TailnetClient {
    pub fn new(socket: PathBuf) -> Self {
        Self { socket }
    }
    pub fn socket(&self) -> &Path {
        &self.socket
    }
    pub async fn request(&self, action: TailnetAction) -> anyhow::Result<TailnetResponse> {
        tokio::time::timeout(Duration::from_secs(12), async {
            let mut stream = tokio::net::UnixStream::connect(&self.socket).await?;
            let mut bytes = serde_json::to_vec(&TailnetRequest {
                version: VERSION,
                action,
            })?;
            bytes.push(b'\n');
            stream.write_all(&bytes).await?;
            let mut response = Vec::new();
            BufReader::new(stream.take(MAX_MESSAGE + 1))
                .read_until(b'\n', &mut response)
                .await?;
            anyhow::ensure!(
                response.len() <= MAX_MESSAGE as usize && response.last() == Some(&b'\n'),
                "Invalid tailnet control response length"
            );
            let response: TailnetResponse = serde_json::from_slice(&response)?;
            anyhow::ensure!(
                response.version == VERSION,
                "Unsupported tailnet control version"
            );
            if let Some(error) = &response.error {
                anyhow::bail!("{error}")
            }
            anyhow::ensure!(
                action == TailnetAction::Login || response.login_url.is_none(),
                "Unexpected login URL in status"
            );
            Ok(response)
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_types::tailnet::TailnetStatus;
    async fn answer(bytes: Vec<u8>) -> anyhow::Result<TailnetResponse> {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("fixture.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = String::new();
            BufReader::new(&mut stream)
                .read_line(&mut request)
                .await
                .unwrap();
            let _ = stream.write_all(&bytes).await;
        });
        let response = TailnetClient::new(sock)
            .request(TailnetAction::Status)
            .await;
        server.await.unwrap();
        response
    }
    #[tokio::test]
    async fn tailnet_client_rejects_wrong_version_oversize_and_unsolicited_login() {
        let mut response = TailnetResponse {
            version: 2,
            status: TailnetStatus::stopped(false, false),
            login_url: None,
            error: None,
        };
        let wire = |r: &TailnetResponse| {
            let mut bytes = serde_json::to_vec(r).unwrap();
            bytes.push(b'\n');
            bytes
        };
        assert!(answer(wire(&response)).await.is_err());
        response.version = 1;
        response.login_url = Some("https://login.tailscale.com/a/fixture".into());
        assert!(answer(wire(&response)).await.is_err());
        response.login_url = None;
        assert!(answer(wire(&response)).await.is_ok());
        assert!(answer(vec![b' '; MAX_MESSAGE as usize + 2]).await.is_err());
    }
}
