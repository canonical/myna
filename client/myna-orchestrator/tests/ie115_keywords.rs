//! Keywords reach an IE115 server only when its `session.created` greeting
//! lists them: a server that does not take them would refuse the session.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::{SinkExt, StreamExt};
use myna_core::SessionConfig;
use myna_orchestrator::{BackendClient, WsUnixIe115Backend};
use serde_json::{json, Value};
use tokio::net::UnixListener;
use tokio_tungstenite::tungstenite::Message;

struct Socket(PathBuf);

impl Drop for Socket {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

fn socket() -> (Socket, UnixListener) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "myna-ie115-keywords-{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::remove_file(&path).ok();
    let listener = UnixListener::bind(&path).unwrap();
    (Socket(path), listener)
}

fn greeting(transcription: Value) -> Value {
    json!({
        "type": "session.created",
        "session": {"type": "transcription", "audio": {"input": {"transcription": transcription}}},
    })
}

/// The `transcription` object of the `session.update` a client opening a
/// session with `keywords` sends, after the server greets it with `greeting`.
async fn sent_transcription(greeting: Option<Value>, keywords: &[&str]) -> Value {
    let (socket, listener) = socket();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        if let Some(greeting) = greeting {
            ws.send(Message::text(greeting.to_string())).await.unwrap();
        }
        while let Some(Ok(message)) = ws.next().await {
            if let Message::Text(text) = message {
                return serde_json::from_str::<Value>(&text).unwrap();
            }
        }
        panic!("the client sent no session.update");
    });
    let config = SessionConfig {
        keywords: keywords.iter().map(|word| (*word).to_owned()).collect(),
        ..SessionConfig::default()
    };
    let _handle = WsUnixIe115Backend::new(&socket.0)
        .open_session(config)
        .await
        .expect("the session opens");
    let update = server.await.unwrap();
    assert_eq!(update["type"], "session.update");
    update["session"]["audio"]["input"]["transcription"].clone()
}

#[tokio::test]
async fn keywords_go_to_a_server_whose_greeting_lists_them() {
    let sent = sent_transcription(Some(greeting(json!({"keywords": []}))), &["Myna"]).await;
    assert_eq!(sent["keywords"], json!(["Myna"]));
}

#[tokio::test]
async fn keywords_stay_home_when_the_greeting_does_not_list_them() {
    let sent = sent_transcription(Some(greeting(json!({"model": "whisper"}))), &["Myna"]).await;
    assert!(sent.get("keywords").is_none(), "sent {sent}");
}

/// A server that never greets costs one bounded wait, not the session.
#[tokio::test(start_paused = true)]
async fn keywords_stay_home_when_the_server_never_greets() {
    let sent = sent_transcription(None, &["Myna"]).await;
    assert!(sent.get("keywords").is_none(), "sent {sent}");
}
