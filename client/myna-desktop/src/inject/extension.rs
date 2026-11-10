//! Text-input backends supplied by out-of-tree IM extensions.
//!
//! Each utterance gets one private Unix-domain socket connection. Myna and the
//! provider exchange one JSON object per line, beginning with a versioned
//! hello. An explicitly selected provider is never replaced by IBus: a
//! missing, stale, incompatible, or malformed provider is this utterance's
//! error.

use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use super::lazy::Connect;
use super::{
    Activation, FocusEvent, InjectError, Injector, Support, Target, TextInputCapabilities,
};

/// The only IM extension protocol version this client implements.
pub const PROTOCOL_VERSION: u32 = 1;

const IO_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_ERROR_CHARS: usize = 4096;
const COMMAND_BACKLOG: usize = 64;
const RESPONSE_BACKLOG: usize = 32;
const SOCKET_DIRECTORY: &str = "myna-input-methods";

/// Capabilities known before a provider has answered hello. Preedit and
/// surrounding text are negotiated by the provider; secure-field detection is
/// required during that negotiation.
pub const CAPABILITIES: TextInputCapabilities = TextInputCapabilities {
    preedit: false,
    surrounding_text: false,
    secure_field_detection: Support::Supported,
};

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Hello { version: u32 },
    Acquire,
    Activate,
    Commit { request: u64, text: String },
    Preedit { request: u64, text: String },
    Release { request: u64 },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct ProviderCapabilities {
    preedit: bool,
    surrounding_text: bool,
    secure_field_detection: bool,
}

impl ProviderCapabilities {
    fn text_input(self) -> Result<TextInputCapabilities, InjectError> {
        if !self.secure_field_detection {
            return Err(InjectError::Unavailable(
                "IM extension does not support secure-field detection".to_string(),
            ));
        }
        Ok(TextInputCapabilities {
            preedit: self.preedit,
            surrounding_text: self.surrounding_text,
            secure_field_detection: Support::Supported,
        })
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ProviderMessage {
    Ready {
        version: u32,
        capabilities: ProviderCapabilities,
    },
    Acquired {
        #[serde(default)]
        before_cursor: Option<String>,
    },
    Ack {
        request: u64,
    },
    FocusLost,
    TargetGone,
    Secure,
    Error {
        code: String,
        message: String,
    },
}

#[derive(Debug)]
enum ClientCommand {
    Write(Vec<u8>),
    Close,
}

#[derive(Debug)]
enum ProviderResponse {
    Acquired { before_cursor: Option<char> },
    Ack { request: u64 },
    Error(InjectError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetState {
    Active,
    FocusLost,
    TargetGone,
    Secure,
    Released,
}

impl TargetState {
    fn error(self) -> Option<InjectError> {
        match self {
            Self::Active => None,
            Self::FocusLost | Self::TargetGone | Self::Released => Some(InjectError::FocusLost),
            Self::Secure => Some(InjectError::SecureField),
        }
    }

    fn focus_event(self) -> Option<FocusEvent> {
        match self {
            Self::Active => None,
            Self::FocusLost => Some(FocusEvent::FocusOut),
            Self::TargetGone | Self::Secure | Self::Released => Some(FocusEvent::TargetGone),
        }
    }
}

fn advance_state(state: &watch::Sender<TargetState>, next: TargetState) {
    state.send_if_modified(|current| {
        if *current == TargetState::Active {
            *current = next;
            true
        } else {
            false
        }
    });
}

#[derive(Debug)]
struct LeaseSlot {
    state: watch::Sender<TargetState>,
}

#[derive(Clone)]
struct ActivationBinding {
    generation: u64,
    commands: mpsc::Sender<ClientCommand>,
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
        let negotiated = connect_negotiated(&socket).await?;
        Ok(Box::new(ExtensionInjector::new(socket, negotiated)))
    }

    fn capabilities(&self) -> TextInputCapabilities {
        CAPABILITIES
    }
}

/// An injector that opens one provider connection per utterance.
pub struct ExtensionInjector {
    socket: PathBuf,
    first_connection: Option<Negotiated>,
    capabilities: TextInputCapabilities,
    next_generation: u64,
    current: Arc<Mutex<Option<LeaseSlot>>>,
    activation: Arc<Mutex<Option<ActivationBinding>>>,
}

impl ExtensionInjector {
    fn new(socket: PathBuf, negotiated: Negotiated) -> Self {
        Self {
            socket,
            capabilities: negotiated.capabilities,
            first_connection: Some(negotiated),
            next_generation: 0,
            current: Arc::new(Mutex::new(None)),
            activation: Arc::new(Mutex::new(None)),
        }
    }

    fn next_generation(&mut self) -> u64 {
        self.next_generation = self.next_generation.wrapping_add(1);
        if self.next_generation == 0 {
            self.next_generation = 1;
        }
        self.next_generation
    }

    async fn connection(&mut self) -> Result<Negotiated, InjectError> {
        match self.first_connection.take() {
            Some(connection) => Ok(connection),
            None => connect_negotiated(&self.socket).await,
        }
    }
}

/// One acquired extension target. The provider connection owns its lease.
pub struct ExtensionTarget {
    commands: mpsc::Sender<ClientCommand>,
    responses: mpsc::Receiver<ProviderResponse>,
    state: watch::Sender<TargetState>,
    generation: u64,
    activation: Arc<Mutex<Option<ActivationBinding>>>,
    next_request: u64,
    before_cursor: Option<char>,
    reader_task: Option<JoinHandle<()>>,
    writer_task: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for ExtensionTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtensionTarget")
            .field("generation", &self.generation)
            .field("next_request", &self.next_request)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Injector for ExtensionInjector {
    async fn acquire(&mut self) -> Result<Box<dyn Target>, InjectError> {
        let generation = self.next_generation();
        *self
            .activation
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        if let Some(previous) = self
            .current
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            advance_state(&previous.state, TargetState::FocusLost);
        }

        let negotiated = self.connection().await?;
        self.capabilities = negotiated.capabilities;
        let (reader, writer) = negotiated.stream.into_split();
        let (commands, command_rx) = mpsc::channel(COMMAND_BACKLOG);
        let (responses, mut response_rx) = mpsc::channel(RESPONSE_BACKLOG);
        let (state, _state_rx) = watch::channel(TargetState::Active);

        let reader_task = tokio::spawn(read_events(
            BufReader::new(reader),
            state.clone(),
            responses.clone(),
        ));
        let writer_task = tokio::spawn(write_commands(writer, command_rx, state.clone()));

        let binding = ActivationBinding {
            generation,
            commands: commands.clone(),
        };
        *self
            .activation
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(binding);
        *self
            .current
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(LeaseSlot {
            state: state.clone(),
        });

        let acquire = encode_message(&ClientMessage::Acquire)?;
        if !matches!(
            tokio::time::timeout(IO_TIMEOUT, commands.send(ClientCommand::Write(acquire))).await,
            Ok(Ok(()))
        ) {
            advance_state(&state, TargetState::TargetGone);
        }

        let mut changed = state.subscribe();
        loop {
            if let Some(error) = state.borrow().error() {
                reader_task.abort();
                writer_task.abort();
                return Err(error);
            }
            tokio::select! {
                biased;
                response = response_rx.recv() => {
                    match response {
                        Some(ProviderResponse::Acquired { before_cursor }) => {
                            return Ok(Box::new(ExtensionTarget {
                                commands,
                                responses: response_rx,
                                state,
                                generation,
                                activation: Arc::clone(&self.activation),
                                next_request: 1,
                                before_cursor,
                                reader_task: Some(reader_task),
                                writer_task: Some(writer_task),
                            }));
                        }
                        Some(ProviderResponse::Error(error)) => {
                            reader_task.abort();
                            writer_task.abort();
                            return Err(error);
                        }
                        Some(ProviderResponse::Ack { .. }) => {
                            reader_task.abort();
                            writer_task.abort();
                            return Err(InjectError::Backend(
                                "IM extension acknowledged before acquiring".to_string(),
                            ));
                        }
                        None => {
                            reader_task.abort();
                            writer_task.abort();
                            return Err(InjectError::Unavailable(
                                "IM extension disconnected while acquiring".to_string(),
                            ));
                        }
                    }
                }
                _ = changed.changed() => {}
            }
        }
    }

    fn capabilities(&self) -> TextInputCapabilities {
        self.capabilities
    }

    fn activation(&self) -> Activation {
        let activation = Arc::clone(&self.activation);
        Activation::new(move || {
            let binding = activation
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            if let Some(binding) = binding {
                if let Ok(message) = encode_message(&ClientMessage::Activate) {
                    let _ = binding.commands.try_send(ClientCommand::Write(message));
                }
            }
        })
    }
}

#[async_trait]
impl Target for ExtensionTarget {
    async fn commit(&mut self, text: &str) -> Result<(), InjectError> {
        if text.is_empty() {
            return Ok(());
        }
        self.request_text(TextRequest::Commit, text, "commit").await
    }

    async fn set_preedit(&mut self, text: &str) {
        let _ = self
            .request_text(TextRequest::Preedit, text, "preedit")
            .await;
    }

    fn char_before_cursor(&self) -> Option<char> {
        self.before_cursor
    }

    fn focus_events(&self) -> BoxStream<'static, FocusEvent> {
        let mut state = self.state.subscribe();
        Box::pin(stream::once(async move {
            loop {
                let current = *state.borrow_and_update();
                if let Some(event) = current.focus_event() {
                    return event;
                }
                if state.changed().await.is_err() {
                    return FocusEvent::TargetGone;
                }
            }
        }))
    }

    fn activated(&self) {
        if !self.writable() {
            return;
        }
        if let Ok(message) = encode_message(&ClientMessage::Activate) {
            let _ = self.commands.try_send(ClientCommand::Write(message));
        }
    }

    async fn release(mut self: Box<Self>) {
        advance_state(&self.state, TargetState::Released);
        let request = self.take_request();
        if let Ok(message) = encode_message(&ClientMessage::Release { request }) {
            if matches!(
                tokio::time::timeout(
                    IO_TIMEOUT,
                    self.commands.send(ClientCommand::Write(message))
                )
                .await,
                Ok(Ok(()))
            ) {
                let _ = tokio::time::timeout(IO_TIMEOUT, self.responses.recv()).await;
            }
        }
        let _ = self.commands.try_send(ClientCommand::Close);
        if let Some(mut task) = self.writer_task.take() {
            if tokio::time::timeout(IO_TIMEOUT, &mut task).await.is_err() {
                task.abort();
            }
        }
        if let Some(task) = self.reader_task.take() {
            task.abort();
        }
        self.clear_activation();
    }
}

impl Drop for ExtensionTarget {
    fn drop(&mut self) {
        advance_state(&self.state, TargetState::Released);
        if let Some(task) = self.reader_task.take() {
            task.abort();
        }
        if let Some(task) = self.writer_task.take() {
            task.abort();
        }
        self.clear_activation();
    }
}

impl ExtensionTarget {
    fn clear_activation(&self) {
        let mut activation = self
            .activation
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if activation
            .as_ref()
            .is_some_and(|binding| binding.generation == self.generation)
        {
            *activation = None;
        }
    }

    fn writable(&self) -> bool {
        *self.state.borrow() == TargetState::Active
    }

    fn check_writable(&self) -> Result<(), InjectError> {
        let state = *self.state.borrow();
        state.error().map_or(Ok(()), Err)
    }

    fn take_request(&mut self) -> u64 {
        let request = self.next_request;
        self.next_request = self.next_request.wrapping_add(1);
        request
    }

    async fn request_text(
        &mut self,
        kind: TextRequest,
        text: &str,
        operation: &str,
    ) -> Result<(), InjectError> {
        self.check_writable()?;
        let mut state = self.state.subscribe();
        let request = self.take_request();
        let text = text.to_owned();
        let message = match kind {
            TextRequest::Commit => ClientMessage::Commit { request, text },
            TextRequest::Preedit => ClientMessage::Preedit { request, text },
        };
        let encoded = encode_message(&message)?;
        match tokio::time::timeout(
            IO_TIMEOUT,
            self.commands.send(ClientCommand::Write(encoded)),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                return Err(InjectError::Unavailable(
                    "IM extension writer stopped".to_string(),
                ));
            }
            Err(_) => {
                return Err(InjectError::Backend(
                    "IM extension writer backlog is full".to_string(),
                ));
            }
        }

        loop {
            if let Some(error) = state.borrow().error() {
                return Err(error);
            }
            tokio::select! {
                biased;
                response = self.responses.recv() => {
                    match response {
                        Some(ProviderResponse::Ack { request: acknowledged })
                            if acknowledged == request => {
                            if let Some(error) = state.borrow().error() {
                                return Err(error);
                            }
                            return Ok(());
                        }
                        Some(ProviderResponse::Ack { .. }) => {
                            return Err(InjectError::Backend(
                                "IM extension acknowledged the wrong request".to_string(),
                            ));
                        }
                        Some(ProviderResponse::Error(error)) => return Err(error),
                        Some(ProviderResponse::Acquired { .. }) => {
                            return Err(InjectError::Backend(format!(
                                "IM extension reacquired during {operation}"
                            )));
                        }
                        None => {
                            return Err(InjectError::Unavailable(
                                "IM extension disconnected".to_string(),
                            ));
                        }
                    }
                }
                _ = state.changed() => {}
                _ = tokio::time::sleep(IO_TIMEOUT) => {
                    return Err(InjectError::Backend(format!(
                        "timed out waiting for IM extension {operation}"
                    )));
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum TextRequest {
    Commit,
    Preedit,
}

struct Negotiated {
    stream: UnixStream,
    capabilities: TextInputCapabilities,
}

async fn connect_negotiated(path: &Path) -> Result<Negotiated, InjectError> {
    validate_socket(path)?;
    let mut stream = tokio::time::timeout(IO_TIMEOUT, UnixStream::connect(path))
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

    write_message(
        &mut stream,
        &ClientMessage::Hello {
            version: PROTOCOL_VERSION,
        },
    )
    .await?;
    let ready = {
        let mut reader = BufReader::new(&mut stream);
        tokio::time::timeout(IO_TIMEOUT, read_message(&mut reader, "hello"))
            .await
            .map_err(|_| {
                InjectError::Backend("timed out waiting for IM extension hello".to_string())
            })??
    };
    match ready {
        ProviderMessage::Ready {
            version,
            capabilities,
        } if version == PROTOCOL_VERSION => {
            let capabilities = capabilities.text_input()?;
            Ok(Negotiated {
                stream,
                capabilities,
            })
        }
        ProviderMessage::Ready { version, .. } => Err(InjectError::Unavailable(format!(
            "IM extension protocol {version} is unsupported (need {PROTOCOL_VERSION})"
        ))),
        ProviderMessage::Error { code, message } => Err(provider_error(&code, &message)),
        other => Err(InjectError::Backend(format!(
            "unexpected IM extension hello reply: {other:?}"
        ))),
    }
}

async fn read_events(
    mut reader: BufReader<OwnedReadHalf>,
    state: watch::Sender<TargetState>,
    responses: mpsc::Sender<ProviderResponse>,
) {
    loop {
        let message = match read_message(&mut reader, "event").await {
            Ok(message) => message,
            Err(InjectError::Unavailable(_)) => {
                advance_state(&state, TargetState::TargetGone);
                return;
            }
            Err(error) => {
                advance_state(&state, TargetState::TargetGone);
                let _ = responses.send(ProviderResponse::Error(error)).await;
                return;
            }
        };

        match message {
            ProviderMessage::Acquired { before_cursor } => {
                let before_cursor = match parse_before_cursor(before_cursor) {
                    Ok(before_cursor) => before_cursor,
                    Err(error) => {
                        advance_state(&state, TargetState::TargetGone);
                        let _ = responses.send(ProviderResponse::Error(error)).await;
                        return;
                    }
                };
                if responses
                    .send(ProviderResponse::Acquired { before_cursor })
                    .await
                    .is_err()
                {
                    advance_state(&state, TargetState::TargetGone);
                    return;
                }
            }
            ProviderMessage::Ack { request } => {
                if responses
                    .send(ProviderResponse::Ack { request })
                    .await
                    .is_err()
                {
                    advance_state(&state, TargetState::TargetGone);
                    return;
                }
            }
            ProviderMessage::FocusLost => {
                advance_state(&state, TargetState::FocusLost);
            }
            ProviderMessage::TargetGone => {
                advance_state(&state, TargetState::TargetGone);
            }
            ProviderMessage::Secure => {
                advance_state(&state, TargetState::Secure);
            }
            ProviderMessage::Error { code, message } => {
                let error = provider_error(&code, &message);
                let next_state = match &error {
                    InjectError::FocusLost => TargetState::FocusLost,
                    InjectError::SecureField => TargetState::Secure,
                    _ => TargetState::TargetGone,
                };
                advance_state(&state, next_state);
                let _ = responses.send(ProviderResponse::Error(error)).await;
            }
            ProviderMessage::Ready { .. } => {
                advance_state(&state, TargetState::TargetGone);
                let _ = responses
                    .send(ProviderResponse::Error(InjectError::Backend(
                        "IM extension sent a second ready message".to_string(),
                    )))
                    .await;
                return;
            }
        }
    }
}

async fn write_commands(
    mut writer: OwnedWriteHalf,
    mut commands: mpsc::Receiver<ClientCommand>,
    state: watch::Sender<TargetState>,
) {
    while let Some(command) = commands.recv().await {
        match command {
            ClientCommand::Write(line) => {
                let written = tokio::time::timeout(IO_TIMEOUT, writer.write_all(&line)).await;
                if !matches!(written, Ok(Ok(()))) {
                    advance_state(&state, TargetState::TargetGone);
                    break;
                }
            }
            ClientCommand::Close => break,
        }
    }
    let _ = writer.shutdown().await;
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

fn encode_message(message: &ClientMessage) -> Result<Vec<u8>, InjectError> {
    let mut line = serde_json::to_vec(message)
        .map_err(|error| InjectError::Backend(format!("encode IM extension message: {error}")))?;
    if line.len() >= MAX_LINE_BYTES {
        return Err(InjectError::Backend(
            "IM extension request exceeds the maximum size".to_string(),
        ));
    }
    line.push(b'\n');
    Ok(line)
}

async fn write_message(
    writer: &mut (impl AsyncWrite + Unpin),
    message: &ClientMessage,
) -> Result<(), InjectError> {
    let line = encode_message(message)?;
    tokio::time::timeout(IO_TIMEOUT, writer.write_all(&line))
        .await
        .map_err(|_| InjectError::Unavailable("IM extension write timed out".to_string()))?
        .map_err(|error| InjectError::Unavailable(format!("write to IM extension: {error}")))
}

async fn read_message(
    reader: &mut (impl AsyncBufRead + Unpin),
    operation: &str,
) -> Result<ProviderMessage, InjectError> {
    let line = read_line_bounded(reader).await?;
    let message: ProviderMessage = serde_json::from_slice(&line).map_err(|error| {
        InjectError::Backend(format!("invalid IM extension {operation} reply: {error}"))
    })?;
    if matches!(
        &message,
        ProviderMessage::Error { message, .. } if message.chars().count() > MAX_ERROR_CHARS
    ) {
        return Err(InjectError::Backend(
            "IM extension error message exceeds the maximum size".to_string(),
        ));
    }
    Ok(message)
}

fn parse_before_cursor(value: Option<String>) -> Result<Option<char>, InjectError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let mut characters = value.chars();
    let character = characters.next().ok_or_else(|| {
        InjectError::Backend("IM extension acquire reply has an empty before_cursor".to_string())
    })?;
    if characters.next().is_some() {
        return Err(InjectError::Backend(
            "IM extension acquire reply has more than one before_cursor character".to_string(),
        ));
    }
    Ok(Some(character))
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
        if line.len() + take >= MAX_LINE_BYTES {
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
    use futures_util::StreamExt;
    use myna_platform::conformance::text_input::{run, Field, FieldKind, FieldView, Fixture};
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::net::UnixListener;
    use tokio::time::Instant;

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
            json!({
                "type": "ready",
                "version": PROTOCOL_VERSION,
                "capabilities": {
                    "preedit": true,
                    "surrounding_text": true,
                    "secure_field_detection": true
                }
            }),
        )
        .await;
    }

    async fn capability_provider(listener: UnixListener) {
        for preedit in [false, true] {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            assert!(matches!(
                read_client_message(&mut reader).await.unwrap(),
                ClientMessage::Hello {
                    version: PROTOCOL_VERSION
                }
            ));
            write_provider_message(
                &mut write,
                &ProviderMessage::Ready {
                    version: PROTOCOL_VERSION,
                    capabilities: ProviderCapabilities {
                        preedit,
                        ..FULL_CAPABILITIES
                    },
                },
            )
            .await
            .unwrap();
            assert!(matches!(
                read_client_message(&mut reader).await.unwrap(),
                ClientMessage::Acquire
            ));
            write_provider_message(
                &mut write,
                &ProviderMessage::Acquired {
                    before_cursor: None,
                },
            )
            .await
            .unwrap();
            let ClientMessage::Release { request } =
                read_client_message(&mut reader).await.unwrap()
            else {
                panic!("expected release");
            };
            write_provider_message(&mut write, &ProviderMessage::Ack { request })
                .await
                .unwrap();
        }
    }

    const FULL_CAPABILITIES: ProviderCapabilities = ProviderCapabilities {
        preedit: true,
        surrounding_text: true,
        secure_field_detection: true,
    };

    #[test]
    fn a_terminal_target_state_is_sticky() {
        let (state, receiver) = watch::channel(TargetState::Active);
        advance_state(&state, TargetState::FocusLost);
        advance_state(&state, TargetState::Secure);
        assert_eq!(*receiver.borrow(), TargetState::FocusLost);
    }

    #[derive(Debug)]
    struct ProviderFieldState {
        text: String,
        preedit: String,
        focused: bool,
        secure: bool,
        last_activation: Option<Instant>,
        blip_start: Option<Instant>,
        active_connection: Option<u64>,
        replies: Option<mpsc::UnboundedSender<ProviderMessage>>,
    }

    impl ProviderFieldState {
        fn reply(&self, message: ProviderMessage) {
            if let Some(replies) = &self.replies {
                let _ = replies.send(message);
            }
        }

        fn end_connection(&mut self, connection: u64) {
            if self.active_connection == Some(connection) {
                self.active_connection = None;
                self.replies = None;
                self.preedit.clear();
            }
        }
    }

    struct AbortTask(JoinHandle<()>);

    impl Drop for AbortTask {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    struct TestProviderField {
        state: Arc<Mutex<ProviderFieldState>>,
        _server: Arc<AbortTask>,
    }

    impl TestProviderField {
        fn schedule_possible_loss(&self, start: Instant) {
            let state = Arc::clone(&self.state);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let state = state.lock().unwrap_or_else(|error| error.into_inner());
                if state
                    .last_activation
                    .is_none_or(|activated| activated < start)
                {
                    state.reply(ProviderMessage::FocusLost);
                }
            });
        }
    }

    #[async_trait]
    impl Field for TestProviderField {
        async fn lose_focus(&mut self) {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.focused = false;
            state.preedit.clear();
            state.reply(ProviderMessage::FocusLost);
        }

        async fn focus(&mut self) {
            self.state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .focused = true;
        }

        async fn blip(&mut self) {
            let start = Instant::now();
            {
                let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
                state.focused = false;
                state.blip_start = Some(start);
                state.preedit.clear();
            }
            self.state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .focused = true;
            self.schedule_possible_loss(start);
        }

        async fn grab(&mut self) {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.focused = false;
            state.blip_start = None;
            state.preedit.clear();
        }

        async fn ungrab(&mut self) {
            let start = Instant::now();
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.focused = true;
            state.blip_start = Some(start);
            self.schedule_possible_loss(start);
        }

        async fn turn_secure(&mut self) {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.secure = true;
            state.preedit.clear();
            state.reply(ProviderMessage::Secure);
        }

        async fn observe(&mut self) -> Option<FieldView> {
            let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            Some(FieldView {
                text: state.text.clone(),
                preedit: state.preedit.clone(),
            })
        }
    }

    struct SocketFixture {
        next: usize,
    }

    #[async_trait]
    impl Fixture for SocketFixture {
        async fn setup(&mut self, kind: FieldKind) -> (Box<dyn Injector>, Box<dyn Field>) {
            self.next += 1;
            let directory = scratch(&format!("conformance-{}", self.next));
            let socket = directory.join("provider.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let state = Arc::new(Mutex::new(ProviderFieldState {
                text: String::new(),
                preedit: String::new(),
                focused: true,
                secure: kind == FieldKind::Secure,
                last_activation: None,
                blip_start: None,
                active_connection: None,
                replies: None,
            }));
            let server = Arc::new(AbortTask(tokio::spawn(serve_provider(
                listener,
                Arc::clone(&state),
            ))));
            let injector = ExtensionConnect::at(socket).connect().await.unwrap();
            (
                injector,
                Box::new(TestProviderField {
                    state,
                    _server: server,
                }),
            )
        }
    }

    async fn serve_provider(listener: UnixListener, state: Arc<Mutex<ProviderFieldState>>) {
        static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(1);
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let connection = NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed);
            tokio::spawn(handle_provider(stream, Arc::clone(&state), connection));
        }
    }

    async fn handle_provider(
        stream: UnixStream,
        state: Arc<Mutex<ProviderFieldState>>,
        connection: u64,
    ) {
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        let (replies, mut outgoing) = mpsc::unbounded_channel();
        let mut writer = tokio::spawn(async move {
            while let Some(message) = outgoing.recv().await {
                if write_provider_message(&mut write, &message).await.is_err() {
                    return;
                }
            }
        });

        let Ok(message) = read_client_message(&mut reader).await else {
            writer.abort();
            return;
        };
        if !matches!(
            message,
            ClientMessage::Hello {
                version: PROTOCOL_VERSION
            }
        ) {
            writer.abort();
            return;
        }
        let _ = replies.send(ProviderMessage::Ready {
            version: PROTOCOL_VERSION,
            capabilities: FULL_CAPABILITIES,
        });

        loop {
            let Ok(message) = read_client_message(&mut reader).await else {
                let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
                state.end_connection(connection);
                break;
            };
            match message {
                ClientMessage::Acquire => {
                    let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
                    if !state.focused {
                        let _ = replies.send(ProviderMessage::Error {
                            code: "no_target".to_string(),
                            message: String::new(),
                        });
                    } else if state.secure {
                        let _ = replies.send(ProviderMessage::Error {
                            code: "secure_target".to_string(),
                            message: String::new(),
                        });
                    } else {
                        state.active_connection = Some(connection);
                        state.replies = Some(replies.clone());
                        let before_cursor = state
                            .text
                            .chars()
                            .next_back()
                            .map(|character| character.to_string());
                        state.reply(ProviderMessage::Acquired { before_cursor });
                    }
                }
                ClientMessage::Activate => {
                    state
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .last_activation = Some(Instant::now());
                }
                ClientMessage::Commit { request, text } => {
                    let blip_allows_write = wait_for_blip(&state).await;
                    let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
                    if state.active_connection != Some(connection) {
                        state.reply(ProviderMessage::FocusLost);
                    } else if !blip_allows_write {
                        state.preedit.clear();
                        state.reply(ProviderMessage::FocusLost);
                    } else if state.secure {
                        state.preedit.clear();
                        state.reply(ProviderMessage::Secure);
                    } else if !state.focused {
                        state.preedit.clear();
                        state.reply(ProviderMessage::FocusLost);
                    } else {
                        state.preedit.clear();
                        state.text.push_str(&text);
                        state.reply(ProviderMessage::Ack { request });
                    }
                }
                ClientMessage::Preedit { request, text } => {
                    let blip_allows_write = wait_for_blip(&state).await;
                    let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
                    if state.active_connection != Some(connection) {
                        state.reply(ProviderMessage::FocusLost);
                    } else if !blip_allows_write {
                        state.preedit.clear();
                        state.reply(ProviderMessage::FocusLost);
                    } else if state.secure {
                        state.preedit.clear();
                        state.reply(ProviderMessage::Secure);
                    } else if !state.focused {
                        state.preedit.clear();
                        state.reply(ProviderMessage::FocusLost);
                    } else {
                        state.preedit = text;
                        state.reply(ProviderMessage::Ack { request });
                    }
                }
                ClientMessage::Release { request } => {
                    {
                        let mut field = state.lock().unwrap_or_else(|error| error.into_inner());
                        if field.active_connection == Some(connection) {
                            field.preedit.clear();
                            field.active_connection = None;
                            field.replies = None;
                        }
                        let _ = replies.send(ProviderMessage::Ack { request });
                    }
                    let _ = tokio::time::timeout(Duration::from_secs(1), &mut writer).await;
                    return;
                }
                ClientMessage::Hello { .. } => {
                    let state = state.lock().unwrap_or_else(|error| error.into_inner());
                    state.reply(ProviderMessage::Error {
                        code: "protocol_error".to_string(),
                        message: "second hello".to_string(),
                    });
                    break;
                }
            }
        }

        let _ = tokio::time::timeout(Duration::from_secs(1), writer).await;
    }

    async fn read_client_message(
        reader: &mut BufReader<OwnedReadHalf>,
    ) -> Result<ClientMessage, InjectError> {
        let line = read_line_bounded(reader).await?;
        serde_json::from_slice(&line).map_err(|error| InjectError::Backend(error.to_string()))
    }

    async fn wait_for_blip(state: &Arc<Mutex<ProviderFieldState>>) -> bool {
        let start = state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .blip_start;
        let Some(start) = start else {
            return true;
        };
        let deadline = start + Duration::from_millis(500);
        if let Some(delay) = deadline.checked_duration_since(Instant::now()) {
            tokio::time::sleep(delay).await;
        }
        let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
        state.blip_start = None;
        state
            .last_activation
            .is_some_and(|activated| activated >= start)
    }

    async fn write_provider_message(
        writer: &mut OwnedWriteHalf,
        message: &ProviderMessage,
    ) -> Result<(), InjectError> {
        let mut line =
            serde_json::to_vec(message).map_err(|error| InjectError::Backend(error.to_string()))?;
        line.push(b'\n');
        writer
            .write_all(&line)
            .await
            .map_err(|error| InjectError::Unavailable(error.to_string()))
    }

    #[tokio::test]
    async fn the_socket_provider_conforms_to_the_target_contract() {
        let report = run(&mut SocketFixture { next: 0 }).await;
        assert!(report.unobserved.is_empty(), "{report:?}");
        assert_eq!(report.not_applicable, ["preedit_without_support_is_inert"]);
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
            send(
                &mut write,
                json!({"type": "acquired", "before_cursor": null}),
            )
            .await;
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
            let release = receive(&mut reader).await;
            assert_eq!(release["type"], "release");
            send(
                &mut write,
                json!({"type": "ack", "request": release["request"]}),
            )
            .await;
        });

        let mut injector = ExtensionConnect::at(&socket).connect().await.unwrap();
        let mut target = injector.acquire().await.unwrap();
        target.commit("你好").await.unwrap();
        target.release().await;
        server.await.unwrap();
        assert_eq!(&*committed.lock().unwrap(), &["你好"]);
        std::fs::remove_dir_all(directory).ok();
    }

    #[tokio::test]
    async fn a_reconnect_replaces_the_negotiated_capabilities() {
        let directory = scratch("reconnect-capabilities");
        let socket = directory.join("provider.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(capability_provider(listener));

        let mut injector = ExtensionConnect::at(socket).connect().await.unwrap();
        assert!(!injector.capabilities().preedit);

        let target = injector.acquire().await.unwrap();
        assert!(!injector.capabilities().preedit);
        target.release().await;

        let target = injector.acquire().await.unwrap();
        assert!(injector.capabilities().preedit);
        target.release().await;

        server.await.unwrap();
        std::fs::remove_dir_all(directory).ok();
    }

    #[tokio::test]
    async fn an_unset_provider_is_not_fabricated_by_the_extension_factory() {
        let socket = scratch("missing").join("provider.sock");
        assert!(matches!(
            ExtensionConnect::at(socket).connect().await,
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
                json!({
                    "type": "ready",
                    "version": PROTOCOL_VERSION + 1,
                    "capabilities": {
                        "preedit": true,
                        "surrounding_text": true,
                        "secure_field_detection": true
                    }
                }),
            )
            .await;
        });

        assert!(matches!(
            ExtensionConnect::at(&socket).connect().await,
            Err(InjectError::Unavailable(_))
        ));
        server.await.unwrap();
        std::fs::remove_dir_all(directory).ok();
    }

    #[tokio::test]
    async fn a_provider_without_secure_field_detection_is_unavailable() {
        let directory = scratch("no-secure-detection");
        let socket = directory.join("provider.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            let _ = receive(&mut reader).await;
            send(
                &mut write,
                json!({
                    "type": "ready",
                    "version": PROTOCOL_VERSION,
                    "capabilities": {
                        "preedit": true,
                        "surrounding_text": true,
                        "secure_field_detection": false
                    }
                }),
            )
            .await;
        });

        assert!(matches!(
            ExtensionConnect::at(&socket).connect().await,
            Err(InjectError::Unavailable(_))
        ));
        server.await.unwrap();
        std::fs::remove_dir_all(directory).ok();
    }

    #[tokio::test]
    async fn a_target_lost_before_the_focus_stream_is_taken_is_remembered() {
        let directory = scratch("late-target-gone");
        let socket = directory.join("provider.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            hello(&mut reader, &mut write).await;
            assert_eq!(receive(&mut reader).await, json!({"type": "acquire"}));
            send(
                &mut write,
                json!({"type": "acquired", "before_cursor": null}),
            )
            .await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            send(&mut write, json!({"type": "target_gone"})).await;
            let release = receive(&mut reader).await;
            send(
                &mut write,
                json!({"type": "ack", "request": release["request"]}),
            )
            .await;
        });

        let mut injector = ExtensionConnect::at(&socket).connect().await.unwrap();
        let target = injector.acquire().await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut events = target.focus_events();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), events.next())
                .await
                .unwrap(),
            Some(FocusEvent::TargetGone)
        );
        target.release().await;
        server.await.unwrap();
        std::fs::remove_dir_all(directory).ok();
    }

    #[tokio::test]
    async fn a_wrong_acknowledgement_is_a_protocol_error() {
        let directory = scratch("wrong-ack");
        let socket = directory.join("provider.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            hello(&mut reader, &mut write).await;
            assert_eq!(receive(&mut reader).await, json!({"type": "acquire"}));
            send(
                &mut write,
                json!({"type": "acquired", "before_cursor": null}),
            )
            .await;
            let commit = receive(&mut reader).await;
            send(
                &mut write,
                json!({"type": "ack", "request": commit["request"].as_u64().unwrap() + 1}),
            )
            .await;
            let release = receive(&mut reader).await;
            send(
                &mut write,
                json!({"type": "ack", "request": release["request"]}),
            )
            .await;
        });

        let mut injector = ExtensionConnect::at(&socket).connect().await.unwrap();
        let mut target = injector.acquire().await.unwrap();
        assert!(matches!(
            target.commit("wrong ack").await,
            Err(InjectError::Backend(_))
        ));
        target.release().await;
        server.await.unwrap();
        std::fs::remove_dir_all(directory).ok();
    }

    #[tokio::test]
    async fn a_provider_error_keeps_its_classification() {
        let directory = scratch("provider-error");
        let socket = directory.join("provider.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            hello(&mut reader, &mut write).await;
            assert_eq!(receive(&mut reader).await, json!({"type": "acquire"}));
            send(
                &mut write,
                json!({"type": "acquired", "before_cursor": null}),
            )
            .await;
            let _ = receive(&mut reader).await;
            send(
                &mut write,
                json!({"type": "error", "code": "unavailable", "message": "provider stopped"}),
            )
            .await;
            let release = receive(&mut reader).await;
            send(
                &mut write,
                json!({"type": "ack", "request": release["request"]}),
            )
            .await;
        });

        let mut injector = ExtensionConnect::at(&socket).connect().await.unwrap();
        let mut target = injector.acquire().await.unwrap();
        assert!(matches!(
            target.commit("classified").await,
            Err(InjectError::Unavailable(_))
        ));
        target.release().await;
        server.await.unwrap();
        std::fs::remove_dir_all(directory).ok();
    }

    #[tokio::test]
    async fn a_provider_disconnect_ends_the_target() {
        let directory = scratch("disconnect");
        let socket = directory.join("provider.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            hello(&mut reader, &mut write).await;
            assert_eq!(receive(&mut reader).await, json!({"type": "acquire"}));
            send(
                &mut write,
                json!({"type": "acquired", "before_cursor": null}),
            )
            .await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let mut injector = ExtensionConnect::at(&socket).connect().await.unwrap();
        let mut target = injector.acquire().await.unwrap();
        server.await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(matches!(
            target.commit("after disconnect").await,
            Err(InjectError::FocusLost)
        ));
        target.release().await;
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

        assert!(matches!(
            ExtensionConnect::at(&socket).connect().await,
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

        assert!(matches!(
            ExtensionConnect::at(&socket).connect().await,
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
        assert!(matches!(
            ExtensionConnect::at(&path).connect().await,
            Err(InjectError::Unavailable(_))
        ));
        std::fs::remove_dir_all(directory).ok();
    }
}
