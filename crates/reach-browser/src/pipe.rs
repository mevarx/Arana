use reach_core::{ReachError, Result};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
pub const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
pub struct PipeConnection {
    writer: tokio::sync::Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
    replies: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Result<Value>>>,
    serial: tokio::sync::Mutex<()>,
    reader_task: tokio::task::JoinHandle<()>,
}
impl PipeConnection {
    pub fn from_io<T: AsyncRead + AsyncWrite + Send + Unpin + 'static>(io: T) -> Self {
        let (read, write) = tokio::io::split(io);
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let reader_task = tokio::spawn(async move {
            reader(read, tx).await;
        });
        Self {
            writer: tokio::sync::Mutex::new(Box::new(write)),
            replies: tokio::sync::Mutex::new(rx),
            serial: tokio::sync::Mutex::new(()),
            reader_task,
        }
    }
    pub async fn close(&self) {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            self.writer.lock().await.shutdown().await
        })
        .await;
        self.reader_task.abort();
    }
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        tokio::time::timeout(CALL_TIMEOUT, self.call_inner(method, params))
            .await
            .map_err(|_| ReachError::new("TIMEOUT", "CDP pipe command response timed out"))?
    }
    async fn call_inner(&self, method: &str, params: Value) -> Result<Value> {
        let _serial = self.serial.lock().await;
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let payload = serde_json::to_vec(&json!({"id":id,"method":method,"params":params}))
            .map_err(|e| ReachError::new("UPSTREAM_BROKEN", e.to_string()))?;
        if payload.len() > MAX_FRAME_BYTES {
            return Err(ReachError::new(
                "BAD_INPUT",
                "CDP command exceeds frame limit",
            ));
        }
        let mut writer = self.writer.lock().await;
        writer.write_all(&payload).await.map_err(ReachError::from)?;
        writer.write_all(&[0]).await.map_err(ReachError::from)?;
        writer.flush().await.map_err(ReachError::from)?;
        drop(writer);
        let mut rx = self.replies.lock().await;
        loop {
            let msg = rx.recv().await.ok_or_else(|| {
                ReachError::new("UPSTREAM_BROKEN", "CDP pipe closed without a reply")
            })??;
            if msg["id"].as_u64() != Some(id) {
                continue;
            }
            if !msg["error"].is_null() {
                return Err(ReachError::new(
                    "UPSTREAM_BROKEN",
                    msg["error"]["message"]
                        .as_str()
                        .unwrap_or("CDP command failed"),
                ));
            }
            return Ok(msg.get("result").cloned().unwrap_or(json!({})));
        }
    }
}
impl Drop for PipeConnection {
    fn drop(&mut self) {
        self.reader_task.abort();
    }
}
async fn reader<R: AsyncRead + Unpin>(mut read: R, tx: tokio::sync::mpsc::Sender<Result<Value>>) {
    loop {
        match read_frame(&mut read).await {
            Ok(Some(frame)) => {
                match serde_json::from_slice::<Value>(&frame) {
                    Ok(msg) if msg.get("id").is_some() => {
                        if tx.send(Ok(msg)).await.is_err() {
                            return;
                        }
                    }
                    Ok(_) => {} // CDP events are intentionally drained; this transport is command-only.
                    Err(e) => {
                        let _ = tx
                            .send(Err(ReachError::new(
                                "UPSTREAM_BROKEN",
                                format!("Invalid CDP pipe JSON: {e}"),
                            )))
                            .await;
                        return;
                    }
                }
            }
            Ok(None) => {
                let _ = tx
                    .send(Err(ReachError::new("UPSTREAM_BROKEN", "CDP pipe closed")))
                    .await;
                return;
            }
            Err(e) => {
                let _ = tx.send(Err(e)).await;
                return;
            }
        }
    }
}
async fn read_frame<R: AsyncRead + Unpin>(read: &mut R) -> Result<Option<Vec<u8>>> {
    let mut frame = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        match read.read(&mut byte).await {
            Ok(0) if frame.is_empty() => return Ok(None),
            Ok(0) => {
                return Err(ReachError::new(
                    "UPSTREAM_BROKEN",
                    "Truncated CDP pipe frame",
                ));
            }
            Ok(_) if byte[0] == 0 => return Ok(Some(frame)),
            Ok(_) => {
                if frame.len() >= MAX_FRAME_BYTES {
                    return Err(ReachError::new(
                        "UPSTREAM_BROKEN",
                        "CDP pipe frame exceeds limit",
                    ));
                }
                frame.push(byte[0]);
            }
            Err(e) => return Err(ReachError::from(e)),
        }
    }
}
#[cfg(test)]
fn decode_frame(frame: &[u8]) -> Result<Value> {
    if frame.len() > MAX_FRAME_BYTES {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            "CDP pipe frame exceeds limit",
        ));
    }
    serde_json::from_slice(frame).map_err(|e| ReachError::new("UPSTREAM_BROKEN", e.to_string()))
}
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test(start_paused = true)]
    async fn unresponsive_pipe_has_one_bounded_command_deadline() {
        let (client, peer) = tokio::io::duplex(4096);
        let connection = PipeConnection::from_io(client);
        assert_eq!(
            connection
                .call("Browser.getVersion", json!({}))
                .await
                .unwrap_err()
                .code,
            "TIMEOUT"
        );
        connection.close().await;
        drop(peer);
    }
    #[tokio::test]
    async fn closing_connection_closes_the_browser_write_end() {
        let (client, mut peer) = tokio::io::duplex(4096);
        let connection = PipeConnection::from_io(client);
        connection.close().await;
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), peer.read_u8())
                .await
                .unwrap()
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }
    #[tokio::test]
    async fn nul_framed_command_receives_matching_reply_while_reader_drains() {
        let (client, mut browser) = tokio::io::duplex(4096);
        let connection = PipeConnection::from_io(client);
        let responder = tokio::spawn(async move {
            let mut bytes = Vec::new();
            loop {
                let b = browser.read_u8().await.unwrap();
                if b == 0 {
                    break;
                }
                bytes.push(b);
            }
            let command: Value = serde_json::from_slice(&bytes).unwrap();
            let id = command["id"].as_u64().unwrap();
            browser.write_all(format!("{{\"id\":999,\"method\":\"event\"}}\\0{{\"id\":{id},\"result\":{{\"ok\":true}}}}\\0").replace("\\0", "\0").as_bytes()).await.unwrap();
        });
        let result = connection
            .call("Runtime.evaluate", json!({"expression":"1+1"}))
            .await
            .unwrap();
        assert_eq!(result["ok"], true);
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn pipe_closes_when_peer_closes_and_rejects_oversized_frames() {
        assert!(decode_frame(&vec![b'x'; MAX_FRAME_BYTES + 1]).is_err());
        let (client, peer) = tokio::io::duplex(64);
        let connection = PipeConnection::from_io(client);
        drop(peer);
        assert!(connection
            .call("Browser.getVersion", json!({}))
            .await
            .is_err());
    }
}
