//! Text injection through an installed Myna input-method extension.
//!
//! Some input frameworks do not expose the IBus engine-registration protocol.
//! An installed provider runs inside that framework, keeps the focused input
//! context, and implements this socket protocol. The Fcitx5 provider in
//! `input-methods/fcitx5/` is the first implementation.
//!
//! Each utterance uses one private Unix-socket connection. The protocol is a
//! sequence of one-byte frames with a big-endian length before each payload:
//!
//! ```text
//! Myna -> provider: H, A, C <u32 length> <UTF-8>, P <u32 length> <UTF-8>, R
//! provider -> Myna: V <u32 version>, Y <u32 length> <capabilities>,
//!                   K, N, S, F, G,
//!                   E <u32 length> <UTF-8>
//! ```
//!
//! H is hello, A acquire, C commit, P preedit and R release; V is the provider's
//! protocol version, Y acquired and K acknowledged. N means no target, S a
//! secure target, F focus lost, G target gone and E a protocol error. The
//! acquire payload starts with a capability byte; bit 0 adds the character
//! before the cursor as UTF-8. Replies are serialized per request, while F and
//! G may also arrive unsolicited.

use std::io::ErrorKind;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use super::{FocusEvent, InjectError, Injector, Support, Target, TextInputCapabilities};

const HELLO: u8 = b'H';
const ACQUIRE: u8 = b'A';
const COMMIT: u8 = b'C';
const PREEDIT: u8 = b'P';
const RELEASE: u8 = b'R';

const READY: u8 = b'V';
const ACQUIRED: u8 = b'Y';
const COMMITTED: u8 = b'K';
const NO_TARGET: u8 = b'N';
const SECURE: u8 = b'S';
const FOCUS_LOST: u8 = b'F';
const TARGET_GONE: u8 = b'G';
const ERROR: u8 = b'E';

const IO_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_FRAME_BYTES: usize = 1024 * 1024;
const ACQUIRED_HAS_BEFORE: u8 = 1 << 0;
pub const PROTOCOL_VERSION: u32 = 1;

/// Protocol 1 requires replacement-safe preedit and surrounding text. It
/// cannot distinguish a password field.
pub const CAPABILITIES: TextInputCapabilities = TextInputCapabilities {
    preedit: true,
    surrounding_text: true,
    secure_field_detection: Support::Unknown,
};

/// Where installed providers publish their sockets. The directory lives in
/// the snap-scoped runtime directory; unconfined providers can create it and
/// Myna's existing AppArmor profile grants access.
pub fn default_provider_directory() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/run/user"))
        .join("myna-input-methods")
}

/// An exact provider override, used by a single extension during development.
pub fn configured_provider_socket() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("MYNA_INPUT_METHOD_SOCKET") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    None
}

pub fn provider_sockets(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut sockets: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.path().extension().is_some_and(|ext| ext == "sock")
                && entry
                    .file_type()
                    .is_ok_and(|file_type| file_type.is_socket())
        })
        .map(|entry| entry.path())
        .collect();
    sockets.sort();
    sockets
}

#[derive(Debug, Clone)]
enum ServerEvent {
    Committed,
    FocusLost,
    TargetGone,
    Error(String),
}

pub struct ExternalInjector {
    path: PathBuf,
    first_connection: Option<UnixStream>,
}

impl ExternalInjector {
    /// Connect and negotiate once up front, so a stale socket or incompatible
    /// protocol does not hide a working IBus fallback. The probe is reused by
    /// the first utterance.
    pub async fn connect_at(path: impl Into<PathBuf>) -> Result<Self, InjectError> {
        let path = path.into();
        let stream = connect_negotiated(&path).await?;
        Ok(Self {
            path,
            first_connection: Some(stream),
        })
    }

    async fn connection(&mut self) -> Result<UnixStream, InjectError> {
        match self.first_connection.take() {
            Some(stream) => Ok(stream),
            None => connect_negotiated(&self.path).await,
        }
    }
}

async fn connect(path: &Path) -> Result<UnixStream, InjectError> {
    match tokio::time::timeout(IO_TIMEOUT, UnixStream::connect(path)).await {
        Ok(Ok(stream)) => Ok(stream),
        Ok(Err(err)) => Err(InjectError::Unavailable(format!(
            "input-method extension socket {}: {err}",
            path.display()
        ))),
        Err(_) => Err(InjectError::Unavailable(format!(
            "input-method extension socket {} timed out",
            path.display()
        ))),
    }
}

async fn connect_negotiated(path: &Path) -> Result<UnixStream, InjectError> {
    let mut stream = connect(path).await?;
    write_frame(&mut stream, &[HELLO]).await?;
    let mut ready = [0u8; 5];
    tokio::time::timeout(IO_TIMEOUT, stream.read_exact(&mut ready))
        .await
        .map_err(|_| {
            InjectError::Unavailable(format!(
                "input-method extension {} did not answer the hello",
                path.display()
            ))
        })?
        .map_err(|err| {
            InjectError::Unavailable(format!("input-method extension hello: {err}"))
        })?;
    if ready[0] != READY {
        return Err(InjectError::Unavailable(format!(
            "input-method extension {} sent unexpected hello reply {:?}",
            path.display(),
            ready[0]
        )));
    }
    let version = u32::from_be_bytes([ready[1], ready[2], ready[3], ready[4]]);
    if version != PROTOCOL_VERSION {
        return Err(InjectError::Unavailable(format!(
            "input-method extension protocol {version} is unsupported (need {PROTOCOL_VERSION})"
        )));
    }
    Ok(stream)
}

#[async_trait]
impl Injector for ExternalInjector {
    async fn acquire(&mut self) -> Result<Box<dyn Target>, InjectError> {
        let mut stream = self.connection().await?;
        write_frame(&mut stream, &[ACQUIRE]).await?;
        let reply = tokio::time::timeout(IO_TIMEOUT, read_frame(&mut stream))
            .await
            .map_err(|_| {
                InjectError::Unavailable(
                    "input-method extension timed out acquiring the target".to_string(),
                )
            })??;
        let before_cursor = match reply.first().copied() {
            Some(ACQUIRED) => acquired_before_cursor(&reply)?,
            Some(NO_TARGET) if reply.len() == 1 => return Err(InjectError::NoTarget),
            Some(SECURE) if reply.len() == 1 => return Err(InjectError::SecureField),
            Some(ERROR) => return Err(InjectError::Backend(read_error(&reply))),
            other => {
                return Err(InjectError::Backend(format!(
                    "unexpected input-method extension acquire reply {other:?}"
                )));
            }
        };

        let (events_tx, events_rx) = broadcast::channel(32);
        let (reader, writer) = stream.into_split();
        let reader_task = tokio::spawn(read_events(reader, events_tx));
        Ok(Box::new(ExternalTarget {
            writer,
            events: Some(events_rx),
            reader_task: Some(reader_task),
            before_cursor,
        }))
    }

    fn capabilities(&self) -> TextInputCapabilities {
        CAPABILITIES
    }
}

async fn read_events(mut reader: OwnedReadHalf, events: broadcast::Sender<ServerEvent>) {
    loop {
        match read_frame(&mut reader).await {
            Ok(frame) => {
                let event = match frame.first().copied() {
                    Some(COMMITTED) => ServerEvent::Committed,
                    Some(FOCUS_LOST) => ServerEvent::FocusLost,
                    Some(TARGET_GONE) => ServerEvent::TargetGone,
                    Some(ERROR) => ServerEvent::Error(read_error(&frame)),
                    other => ServerEvent::Error(format!(
                        "unexpected input-method extension reply {other:?}"
                    )),
                };
                if events.send(event).is_err() {
                    return;
                }
            }
            Err(InjectError::Unavailable(_)) => {
                let _ = events.send(ServerEvent::TargetGone);
                return;
            }
            Err(err) => {
                let _ = events.send(ServerEvent::Error(err.to_string()));
                return;
            }
        }
    }
}

#[derive(Debug)]
struct ExternalTarget {
    writer: OwnedWriteHalf,
    events: Option<broadcast::Receiver<ServerEvent>>,
    reader_task: Option<JoinHandle<()>>,
    before_cursor: Option<char>,
}

impl ExternalTarget {
    async fn wait_for_ack(
        &mut self,
        events: &mut broadcast::Receiver<ServerEvent>,
        operation: &str,
    ) -> Result<(), InjectError> {
        match tokio::time::timeout(IO_TIMEOUT, events.recv()).await {
            Ok(Ok(ServerEvent::Committed)) => Ok(()),
            Ok(Ok(ServerEvent::FocusLost | ServerEvent::TargetGone)) => Err(InjectError::FocusLost),
            Ok(Ok(ServerEvent::Error(message))) => Err(InjectError::Backend(message)),
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => Err(InjectError::Backend(format!(
                "missed input-method extension {operation} reply"
            ))),
            Ok(Err(broadcast::error::RecvError::Closed)) => Err(InjectError::Unavailable(
                "input-method extension disconnected".to_string(),
            )),
            Err(_) => Err(InjectError::Backend(format!(
                "timed out waiting for input-method extension {operation}"
            ))),
        }
    }

    async fn send_text(&mut self, tag: u8, text: &str, operation: &str) -> Result<(), InjectError> {
        let mut events = self
            .events
            .as_ref()
            .expect("focus events are taken once")
            .resubscribe();
        let len = u32::try_from(text.len()).map_err(|_| {
            InjectError::Backend(format!("input-method extension {operation} is too long"))
        })?;
        let mut frame = Vec::with_capacity(5 + text.len());
        frame.push(tag);
        frame.extend_from_slice(&len.to_be_bytes());
        frame.extend_from_slice(text.as_bytes());
        write_frame(&mut self.writer, &frame).await?;
        self.wait_for_ack(&mut events, operation).await
    }
}

#[async_trait]
impl Target for ExternalTarget {
    async fn commit(&mut self, text: &str) -> Result<(), InjectError> {
        if text.is_empty() {
            return Ok(());
        }
        self.send_text(COMMIT, text, "commit").await
    }

    async fn set_preedit(&mut self, text: &str) {
        let _ = self.send_text(PREEDIT, text, "preedit").await;
    }

    fn char_before_cursor(&self) -> Option<char> {
        self.before_cursor
    }

    fn focus_events(&self) -> BoxStream<'static, FocusEvent> {
        let receiver = self
            .events
            .as_ref()
            .expect("focus events are taken once")
            .resubscribe();
        Box::pin(stream::unfold(receiver, |mut receiver| async move {
            loop {
                match receiver.recv().await {
                    Ok(ServerEvent::FocusLost) => {
                        return Some((FocusEvent::FocusOut, receiver));
                    }
                    Ok(ServerEvent::TargetGone) => {
                        return Some((FocusEvent::TargetGone, receiver));
                    }
                    Ok(ServerEvent::Committed | ServerEvent::Error(_)) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        return Some((FocusEvent::TargetGone, receiver));
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        }))
    }

    async fn release(mut self: Box<Self>) {
        let _ = write_frame(&mut self.writer, &[RELEASE]).await;
        self.writer.shutdown().await.ok();
        if let Some(task) = self.reader_task.take() {
            task.abort();
        }
    }
}

impl Drop for ExternalTarget {
    fn drop(&mut self) {
        if let Some(task) = self.reader_task.take() {
            task.abort();
        }
    }
}

async fn write_frame(
    writer: &mut (impl AsyncWriteExt + Unpin),
    frame: &[u8],
) -> Result<(), InjectError> {
    tokio::time::timeout(IO_TIMEOUT, writer.write_all(frame))
        .await
        .map_err(|_| {
            InjectError::Unavailable("input-method extension write timed out".to_string())
        })?
        .map_err(|err| InjectError::Unavailable(format!("input-method extension write: {err}")))
}

async fn read_frame(reader: &mut (impl AsyncReadExt + Unpin)) -> Result<Vec<u8>, InjectError> {
    let mut tag = [0u8; 1];
    match reader.read_exact(&mut tag).await {
        Ok(_) => {}
        Err(err) if matches!(err.kind(), ErrorKind::UnexpectedEof) => {
            return Err(InjectError::Unavailable(
                "input-method extension disconnected".to_string(),
            ));
        }
        Err(err) => {
            return Err(InjectError::Unavailable(format!(
                "input-method extension read: {err}"
            )));
        }
    }
    let mut frame = vec![tag[0]];
    if matches!(tag[0], ACQUIRED | COMMIT | PREEDIT | ERROR) {
        let mut length = [0u8; 4];
        reader.read_exact(&mut length).await.map_err(|err| {
            InjectError::Unavailable(format!("input-method extension frame length: {err}"))
        })?;
        let len = u32::from_be_bytes(length) as usize;
        if len > MAX_FRAME_BYTES {
            return Err(InjectError::Backend(
                "input-method extension reply exceeds the maximum size".to_string(),
            ));
        }
        let mut message = vec![0u8; len];
        reader.read_exact(&mut message).await.map_err(|err| {
            InjectError::Unavailable(format!("input-method extension error text: {err}"))
        })?;
        frame.extend_from_slice(&length);
        frame.extend_from_slice(&message);
    }
    Ok(frame)
}

fn read_error(frame: &[u8]) -> String {
    String::from_utf8_lossy(frame.get(5..).unwrap_or_default()).into_owned()
}

fn acquired_before_cursor(frame: &[u8]) -> Result<Option<char>, InjectError> {
    if frame.len() < 6 {
        return Err(InjectError::Backend(
            "input-method extension acquire reply has no capabilities".to_string(),
        ));
    }
    let flags = frame[5];
    if flags & ACQUIRED_HAS_BEFORE == 0 {
        return Ok(None);
    }
    let before = std::str::from_utf8(&frame[6..]).map_err(|err| {
        InjectError::Backend(format!("input-method extension surrounding text: {err}"))
    })?;
    Ok(before.chars().next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    fn scratch(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("myna-external-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    async fn reply(writer: &mut OwnedWriteHalf, frame: &[u8]) {
        write_frame(writer, frame).await.unwrap();
    }

    async fn hello(reader: &mut OwnedReadHalf, writer: &mut OwnedWriteHalf) {
        assert_eq!(read_frame(reader).await.unwrap(), vec![HELLO]);
        let mut ready = vec![READY];
        ready.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
        reply(writer, &ready).await;
    }

    fn acquired(before: Option<&str>) -> Vec<u8> {
        let mut payload = vec![before.map_or(0, |_| ACQUIRED_HAS_BEFORE)];
        payload.extend_from_slice(before.unwrap_or_default().as_bytes());
        let mut frame = vec![ACQUIRED];
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(&payload);
        frame
    }

    #[tokio::test]
    async fn commit_and_focus_loss_use_the_addon_protocol() {
        let dir = scratch("commit");
        let path = dir.join("frontend.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();
            hello(&mut reader, &mut writer).await;
            let acquire = read_frame(&mut reader).await.unwrap();
            assert_eq!(acquire, vec![ACQUIRE]);
            reply(&mut writer, &acquired(None)).await;
            let frame = read_frame(&mut reader).await.unwrap();
            assert_eq!(frame[0], COMMIT);
            assert_eq!(&frame[5..], b"hello");
            reply(&mut writer, &[FOCUS_LOST]).await;
            reply(&mut writer, &[COMMITTED]).await;
        });

        let mut injector = ExternalInjector::connect_at(&path).await.unwrap();
        let mut target = injector.acquire().await.unwrap();
        assert_eq!(
            target.commit("hello").await.unwrap_err().to_string(),
            "focus left the dictation target"
        );
        server.await.unwrap();
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn preedit_and_surrounding_text_cross_the_boundary() {
        let dir = scratch("preedit");
        let path = dir.join("frontend.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();
            hello(&mut reader, &mut writer).await;
            assert_eq!(read_frame(&mut reader).await.unwrap(), vec![ACQUIRE]);
            reply(&mut writer, &acquired(Some("世"))).await;
            let frame = read_frame(&mut reader).await.unwrap();
            assert_eq!(frame[0], PREEDIT);
            assert_eq!(&frame[5..], "正在".as_bytes());
            reply(&mut writer, &[COMMITTED]).await;
        });

        let mut injector = ExternalInjector::connect_at(&path).await.unwrap();
        let mut target = injector.acquire().await.unwrap();
        assert_eq!(target.char_before_cursor(), Some('世'));
        target.set_preedit("正在").await;
        server.await.unwrap();
        target.release().await;
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn no_target_is_not_a_dead_connection() {
        let dir = scratch("no-target");
        let path = dir.join("frontend.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();
            hello(&mut reader, &mut writer).await;
            assert_eq!(read_frame(&mut reader).await.unwrap(), vec![ACQUIRE]);
            reply(&mut writer, &[NO_TARGET]).await;
        });

        let mut injector = ExternalInjector::connect_at(&path).await.unwrap();
        assert!(matches!(
            injector.acquire().await.unwrap_err(),
            InjectError::NoTarget
        ));
        server.await.unwrap();
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn an_incompatible_protocol_is_rejected_before_selection() {
        let dir = scratch("protocol");
        let path = dir.join("frontend.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();
            assert_eq!(read_frame(&mut reader).await.unwrap(), vec![HELLO]);
            let mut ready = vec![READY];
            ready.extend_from_slice(&(PROTOCOL_VERSION + 1).to_be_bytes());
            reply(&mut writer, &ready).await;
        });

        let error = match ExternalInjector::connect_at(&path).await {
            Ok(_) => panic!("an incompatible protocol must not become the backend"),
            Err(error) => error,
        };
        assert!(matches!(error, InjectError::Unavailable(_)));
        server.await.unwrap();
        std::fs::remove_dir_all(dir).ok();
    }
}
