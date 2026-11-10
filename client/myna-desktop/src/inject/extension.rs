//! Text-input backends supplied by out-of-tree IM extensions.
//!
//! Each utterance gets one private Unix-domain socket connection. Myna and the
//! provider exchange one JSON object per line, beginning with a versioned
//! hello. An explicitly selected provider is never replaced by IBus: a
//! missing, stale, incompatible, or malformed provider is this utterance's
//! error.

use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;

use super::lazy::Connect;
use super::{FocusEvent, InjectError, Injector, Target, TextInputCapabilities};

/// The only IM extension protocol version this client implements.
pub const PROTOCOL_VERSION: u32 = 1;

const IO_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_LINE_BYTES: usize = 1024 * 1024;
const SOCKET_DIRECTORY: &str = "myna-input-methods";

/// The protocol requires only a literal commit today. Later tickets add
/// preedit and secure-field capabilities to the negotiated protocol.
pub const CAPABILITIES: TextInputCapabilities = TextInputCapabilities::COMMIT_ONLY;

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage<'a> {
    Hello { version: u32 },
    Acquire,
    Commit { request: u64, text: &'a str },
    Release,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ProviderMessage {
    Ready { version: u32 },
    Acquired,
    Ack { request: u64 },
    Error { code: String, message: String },
}

/// Opens extension injectors for one configured provider.
pub struct ExtensionConnect {
    socket: Result<PathBuf, String>,
}

impl ExtensionConnect {
    /// Use the fixed socket named by an active provider id.
    pub fn for_provider(provider_id: &str) -> Self {
        Self {
            socket: default_socket_path(provider_id),
        }
    }

    /// Use an exact socket path in tests; the daemon derives its path from the
    /// provider id.
    #[cfg(test)]
    fn at(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: Ok(socket.into()),
        }
    }
}

#[async_trait]
impl Connect for ExtensionConnect {
    async fn connect(&mut self) -> Result<Box<dyn Injector>, InjectError> {
        let socket = self.socket.clone().map_err(InjectError::Unavailable)?;
        Ok(Box::new(ExtensionInjector::at(socket)))
    }

    fn capabilities(&self) -> TextInputCapabilities {
        CAPABILITIES
    }
}

/// An injector that opens one provider connection per utterance.
pub struct ExtensionInjector {
    socket: PathBuf,
}

impl ExtensionInjector {
    fn at(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }
}

#[async_trait]
impl Injector for ExtensionInjector {
    async fn acquire(&mut self) -> Result<Box<dyn Target>, InjectError> {
        let (mut reader, mut writer) = connect_negotiated(&self.socket).await?;
        write_message(&mut writer, &ClientMessage::Acquire).await?;
        match read_message(&mut reader, "acquire").await? {
            ProviderMessage::Acquired => Ok(Box::new(ExtensionTarget {
                reader,
                writer,
                next_request: 1,
            })),
            ProviderMessage::Error { code, message } => Err(provider_error(&code, &message)),
            other => Err(InjectError::Backend(format!(
                "unexpected IM extension acquire reply: {other:?}"
            ))),
        }
    }

    fn capabilities(&self) -> TextInputCapabilities {
        CAPABILITIES
    }
}

/// One acquired extension target. The provider connection owns its lease.
pub struct ExtensionTarget {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    next_request: u64,
}

impl std::fmt::Debug for ExtensionTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtensionTarget")
            .field("next_request", &self.next_request)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Target for ExtensionTarget {
    async fn commit(&mut self, text: &str) -> Result<(), InjectError> {
        let request = self.next_request;
        self.next_request = self.next_request.wrapping_add(1);
        write_message(&mut self.writer, &ClientMessage::Commit { request, text }).await?;
        match read_message(&mut self.reader, "commit").await? {
            ProviderMessage::Ack {
                request: acknowledged,
            } if acknowledged == request => Ok(()),
            ProviderMessage::Ack { .. } => Err(InjectError::Backend(
                "IM extension acknowledged the wrong request".to_string(),
            )),
            ProviderMessage::Error { code, message } => Err(provider_error(&code, &message)),
            other => Err(InjectError::Backend(format!(
                "unexpected IM extension commit reply: {other:?}"
            ))),
        }
    }

    fn focus_events(&self) -> BoxStream<'static, FocusEvent> {
        // Ticket 02 adds the complete asynchronous target lifecycle.
        Box::pin(stream::pending())
    }

    async fn release(mut self: Box<Self>) {
        let _ = write_message(&mut self.writer, &ClientMessage::Release).await;
        let _ = self.writer.shutdown().await;
    }
}

async fn connect_negotiated(
    path: &Path,
) -> Result<(BufReader<OwnedReadHalf>, OwnedWriteHalf), InjectError> {
    validate_socket(path)?;
    let stream = tokio::time::timeout(IO_TIMEOUT, UnixStream::connect(path))
        .await
        .map_err(|_| {
            InjectError::Unavailable(format!("IM extension socket {} timed out", path.display()))
        })?
        .map_err(|error| {
            InjectError::Unavailable(format!("IM extension socket {}: {error}", path.display()))
        })?;
    require_same_user(stream.peer_cred().map_err(|error| {
        InjectError::Unavailable(format!("cannot inspect IM extension credentials: {error}"))
    })?)?;

    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    write_message(
        &mut write,
        &ClientMessage::Hello {
            version: PROTOCOL_VERSION,
        },
    )
    .await?;
    match read_message(&mut reader, "hello").await? {
        ProviderMessage::Ready { version } if version == PROTOCOL_VERSION => Ok((reader, write)),
        ProviderMessage::Ready { version } => Err(InjectError::Unavailable(format!(
            "IM extension protocol {version} is unsupported (need {PROTOCOL_VERSION})"
        ))),
        ProviderMessage::Error { code, message } => Err(provider_error(&code, &message)),
        other => Err(InjectError::Backend(format!(
            "unexpected IM extension hello reply: {other:?}"
        ))),
    }
}

fn validate_socket(path: &Path) -> Result<(), InjectError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        InjectError::Unavailable(format!("IM extension socket {}: {error}", path.display()))
    })?;
    if !metadata.file_type().is_socket() {
        return Err(InjectError::Unavailable(format!(
            "IM extension path {} is not a socket",
            path.display()
        )));
    }
    if metadata.uid() != current_uid() {
        return Err(InjectError::Unavailable(format!(
            "IM extension socket {} is owned by another user",
            path.display()
        )));
    }
    Ok(())
}

fn require_same_user(credentials: tokio::net::unix::UCred) -> Result<(), InjectError> {
    if credentials.uid() == current_uid() {
        Ok(())
    } else {
        Err(InjectError::Unavailable(format!(
            "IM extension peer uid {} is not the current user",
            credentials.uid()
        )))
    }
}

fn current_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and does not modify memory.
    unsafe { libc::geteuid() }
}

fn default_socket_path(provider_id: &str) -> Result<PathBuf, String> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| "XDG_RUNTIME_DIR is not set".to_string())?;
    socket_path(&runtime.join(SOCKET_DIRECTORY), provider_id)
}

fn socket_path(directory: &Path, provider_id: &str) -> Result<PathBuf, String> {
    if provider_id.is_empty()
        || provider_id.len() > 64
        || !provider_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        || !provider_id
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        || !provider_id
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
    {
        return Err(format!("invalid IM extension provider id {provider_id:?}"));
    }
    Ok(directory.join(format!("{provider_id}.sock")))
}

async fn write_message(
    writer: &mut (impl AsyncWrite + Unpin),
    message: &ClientMessage<'_>,
) -> Result<(), InjectError> {
    let mut line = serde_json::to_vec(message)
        .map_err(|error| InjectError::Backend(format!("encode IM extension message: {error}")))?;
    if line.len() >= MAX_LINE_BYTES {
        return Err(InjectError::Backend(
            "IM extension request exceeds the maximum size".to_string(),
        ));
    }
    line.push(b'\n');
    tokio::time::timeout(IO_TIMEOUT, writer.write_all(&line))
        .await
        .map_err(|_| InjectError::Unavailable("IM extension write timed out".to_string()))?
        .map_err(|error| InjectError::Unavailable(format!("write to IM extension: {error}")))
}

async fn read_message(
    reader: &mut (impl AsyncBufRead + Unpin),
    operation: &str,
) -> Result<ProviderMessage, InjectError> {
    let line = tokio::time::timeout(IO_TIMEOUT, read_line_bounded(reader))
        .await
        .map_err(|_| {
            InjectError::Backend(format!(
                "timed out waiting for the IM extension {operation} reply"
            ))
        })??;
    serde_json::from_slice(&line).map_err(|error| {
        InjectError::Backend(format!("invalid IM extension {operation} reply: {error}"))
    })
}

async fn read_line_bounded(
    reader: &mut (impl AsyncBufRead + Unpin),
) -> Result<Vec<u8>, InjectError> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await.map_err(|error| {
            InjectError::Unavailable(format!("read from IM extension: {error}"))
        })?;
        if available.is_empty() {
            return Err(if line.is_empty() {
                InjectError::Unavailable("IM extension disconnected".to_string())
            } else {
                InjectError::Backend("IM extension disconnected mid-message".to_string())
            });
        }

        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.unwrap_or(available.len());
        if line.len() + take > MAX_LINE_BYTES {
            return Err(InjectError::Backend(
                "IM extension reply exceeds the maximum size".to_string(),
            ));
        }
        line.extend_from_slice(&available[..take]);
        let consumed = take + usize::from(newline.is_some());
        reader.consume(consumed);
        if newline.is_some() {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                return Err(InjectError::Backend(
                    "IM extension sent an empty message".to_string(),
                ));
            }
            return Ok(line);
        }
    }
}

fn provider_error(code: &str, message: &str) -> InjectError {
    match code {
        "no_target" => InjectError::NoTarget,
        "secure_target" => InjectError::SecureField,
        "focus_lost" => InjectError::FocusLost,
        "unavailable" => InjectError::Unavailable(message.to_owned()),
        _ => InjectError::Backend(format!("{code}: {message}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
    use tokio::net::UnixListener;

    fn scratch(tag: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("myna-im-extension-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    async fn receive(reader: &mut BufReader<OwnedReadHalf>) -> Value {
        let line = read_line_bounded(reader).await.unwrap();
        serde_json::from_slice(&line).unwrap()
    }

    async fn send(writer: &mut OwnedWriteHalf, value: Value) {
        let mut line = serde_json::to_vec(&value).unwrap();
        line.push(b'\n');
        writer.write_all(&line).await.unwrap();
    }

    async fn hello(reader: &mut BufReader<OwnedReadHalf>, writer: &mut OwnedWriteHalf) {
        assert_eq!(
            receive(reader).await,
            json!({"type": "hello", "version": PROTOCOL_VERSION})
        );
        send(
            writer,
            json!({"type": "ready", "version": PROTOCOL_VERSION}),
        )
        .await;
    }

    #[tokio::test]
    async fn an_extension_commits_literal_text() {
        let directory = scratch("commit");
        let socket = directory.join("provider.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let committed = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&committed);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            hello(&mut reader, &mut write).await;
            assert_eq!(receive(&mut reader).await, json!({"type": "acquire"}));
            send(&mut write, json!({"type": "acquired"})).await;
            let commit = receive(&mut reader).await;
            assert_eq!(commit["type"], "commit");
            assert_eq!(commit["text"], "你好");
            observed
                .lock()
                .unwrap()
                .push(commit["text"].as_str().unwrap().to_owned());
            send(
                &mut write,
                json!({"type": "ack", "request": commit["request"]}),
            )
            .await;
            assert_eq!(receive(&mut reader).await, json!({"type": "release"}));
        });

        let mut injector = ExtensionInjector::at(&socket);
        let mut target = injector.acquire().await.unwrap();
        target.commit("你好").await.unwrap();
        target.release().await;
        server.await.unwrap();
        assert_eq!(&*committed.lock().unwrap(), &["你好"]);
        std::fs::remove_dir_all(directory).ok();
    }

    #[tokio::test]
    async fn an_unset_provider_is_not_fabricated_by_the_extension_factory() {
        let socket = scratch("missing").join("provider.sock");
        let mut injector = ExtensionConnect::at(socket).connect().await.unwrap();
        assert!(matches!(
            injector.acquire().await,
            Err(InjectError::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn an_incompatible_protocol_is_rejected() {
        let directory = scratch("version");
        let socket = directory.join("provider.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            let _ = receive(&mut reader).await;
            send(
                &mut write,
                json!({"type": "ready", "version": PROTOCOL_VERSION + 1}),
            )
            .await;
        });

        let mut injector = ExtensionInjector::at(&socket);
        assert!(matches!(
            injector.acquire().await,
            Err(InjectError::Unavailable(_))
        ));
        server.await.unwrap();
        std::fs::remove_dir_all(directory).ok();
    }

    #[tokio::test]
    async fn malformed_json_is_rejected() {
        let directory = scratch("malformed");
        let socket = directory.join("provider.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            let _ = receive(&mut reader).await;
            write.write_all(b"not json\n").await.unwrap();
        });

        let mut injector = ExtensionInjector::at(&socket);
        assert!(matches!(
            injector.acquire().await,
            Err(InjectError::Backend(_))
        ));
        server.await.unwrap();
        std::fs::remove_dir_all(directory).ok();
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_provider_times_out() {
        let directory = scratch("timeout");
        let socket = directory.join("provider.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });

        let mut injector = ExtensionInjector::at(&socket);
        assert!(matches!(
            injector.acquire().await,
            Err(InjectError::Backend(_))
        ));
        server.abort();
        std::fs::remove_dir_all(directory).ok();
    }

    #[tokio::test]
    async fn an_oversized_reply_is_rejected_without_buffering_past_the_limit() {
        let (mut writer, reader) = tokio::io::duplex(MAX_LINE_BYTES + 1024);
        let server = tokio::spawn(async move {
            writer
                .write_all(&vec![b'x'; MAX_LINE_BYTES + 1])
                .await
                .unwrap();
        });
        let mut reader = BufReader::new(reader);
        assert!(matches!(
            read_line_bounded(&mut reader).await,
            Err(InjectError::Backend(_))
        ));
        server.await.unwrap();
    }

    #[test]
    fn provider_ids_are_bounded_safe_file_names() {
        for valid in ["fcitx5", "org.example.extension-1"] {
            let path = socket_path(Path::new("/runtime"), valid).unwrap();
            assert_eq!(path, Path::new("/runtime").join(format!("{valid}.sock")));
        }
        for invalid in ["", "../escape", "with space", ".hidden", "ends."] {
            assert!(socket_path(Path::new("/runtime"), invalid).is_err());
        }
    }

    #[tokio::test]
    async fn a_regular_file_is_not_accepted_as_a_provider_socket() {
        let directory = scratch("regular");
        let path = directory.join("provider.sock");
        std::fs::write(&path, b"not a socket").unwrap();
        let mut injector = ExtensionInjector::at(&path);
        assert!(matches!(
            injector.acquire().await,
            Err(InjectError::Unavailable(_))
        ));
        std::fs::remove_dir_all(directory).ok();
    }
}
