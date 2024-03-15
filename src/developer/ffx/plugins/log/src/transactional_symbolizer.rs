// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Error};
use ffx_config::global_env_context;
use futures::{ready, select, FutureExt, Stream, StreamExt};
use log_command::log_formatter::{LogData, LogEntry, Symbolize};
use pin_project::pin_project;
use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    collections::VecDeque,
    fmt::{Debug, Display},
    mem::swap,
    ops::Deref,
    pin::Pin,
    process::Stdio,
    task::Poll,
};
use symbol_index::ensure_symbol_index_registered;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
};

use crate::{condition_variable::LocalConditionVariable, mutex::LocalOrderedMutex};

/// Connection to a symbolizer.
pub trait SymbolizerProcess {
    type Stdin;
    type Stdout;
    fn take_stdin(&mut self) -> Option<Self::Stdin>;
    fn take_stdout(&mut self) -> Option<Self::Stdout>;
}

/// Symbolizer state that can only be accessed by one future at a time.
#[derive(Debug)]
struct SymbolizerState<T>
where
    T::Stdin: AsyncWrite + Unpin,
    T::Stdout: AsyncRead + Unpin,
    T: SymbolizerProcess,
{
    stdin: T::Stdin,
    stdout: AsyncTransactionReader<T::Stdout>,
}

impl<T> SymbolizerState<T>
where
    T::Stdin: AsyncWrite + Unpin,
    T::Stdout: AsyncRead + Unpin,
    T: SymbolizerProcess,
{
    fn new(stdin: T::Stdin, stdout: T::Stdout) -> Self {
        Self { stdin, stdout: AsyncTransactionReader::new(stdout) }
    }
}

#[derive(Debug)]
pub struct TransactionalSymbolizer<T: SymbolizerProcess>
where
    T::Stdin: AsyncWrite + Unpin + Debug,
    T::Stdout: AsyncRead + Unpin + Debug,
    T: SymbolizerProcess,
{
    /// Lines that need to be written to the symbolizer
    pending_lines: RefCell<VecDeque<String>>,
    current_transaction_id: Cell<u64>,
    /// State that can only be accessed by one future at once
    state: LocalOrderedMutex<SymbolizerState<T>>,
    /// Woken when a line is available from the device.
    line_available_event: LocalConditionVariable,
    disabled: Cell<bool>,
}

#[async_trait::async_trait(?Send)]
impl<T> Symbolize for TransactionalSymbolizer<T>
where
    T::Stdin: AsyncWrite + Unpin + Debug,
    T::Stdout: AsyncRead + Unpin + Debug,
    T: SymbolizerProcess + 'static,
{
    async fn symbolize(&self, entry: LogEntry) -> Option<LogEntry> {
        if self.disabled.get() {
            return Some(entry);
        }
        let LogEntry { timestamp, data } = entry;
        match self.symbolize_internal(LogEntry { timestamp, data }).await {
            Ok(entry) => Some(entry),
            Err(ReadError::EmptyOutputFromSymbolizer) => None,
            Err(error) => {
                self.disabled.set(true);
                let error = error.to_string();
                eprintln!("Internal symbolizer error: {error}. Symbolization has been disabled.");
                Some(LogEntry { timestamp, data: LogData::MalformedTargetLog(error) })
            }
        }
    }
}

/// Start of transaction message
const TRANSACTION_BEGIN_STR: &'static str = "TXN";

/// Replacement message to escape out TXN
const TRANSACTION_REPLACEMENT_STR: &'static str = "RTXN";

/// Represents a transactional message parsed
/// from the symbolizer output stream.
#[derive(Debug, PartialEq)]
struct Message<'a> {
    /// Transaction ID
    transaction_id: Option<u64>,
    /// Message, which may be empty
    message: EscapedMessage<'a>,
    /// Whether or not this is a commit message.
    is_txn_commit: bool,
}

#[derive(Debug, PartialEq, Clone)]
struct EscapedMessage<'a>(Cow<'a, str>);

#[derive(Debug, PartialEq, Clone)]
struct UnescapedMessage<'a>(Cow<'a, str>);

impl<'a> From<Cow<'a, str>> for EscapedMessage<'a> {
    fn from(value: Cow<'a, str>) -> Self {
        Self(value)
    }
}

impl<'a> From<&'a str> for EscapedMessage<'a> {
    fn from(value: &'a str) -> Self {
        Self(Cow::Borrowed(value))
    }
}

impl<'a> Deref for EscapedMessage<'a> {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a> Deref for UnescapedMessage<'a> {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a> Display for UnescapedMessage<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl<'a> Display for EscapedMessage<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl<'a> From<Cow<'a, str>> for UnescapedMessage<'a> {
    fn from(value: Cow<'a, str>) -> Self {
        Self(value)
    }
}

impl<'a> From<&'a str> for UnescapedMessage<'a> {
    fn from(value: &'a str) -> Self {
        Self(Cow::Borrowed(value))
    }
}

impl<'a> PartialEq<&str> for UnescapedMessage<'a> {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl<'a> PartialEq<&str> for EscapedMessage<'a> {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl<'a> EscapedMessage<'a> {
    /// Unescapes a message received from the symbolizer. The string has to
    /// be modified when sending and receiving from the symbolizer in order
    /// to ensure that a string containing something that looks like a TXN
    /// isn't actually treated as a TXN unless it comes from us.
    fn unescape(self) -> UnescapedMessage<'a> {
        if !self.contains(TRANSACTION_REPLACEMENT_STR) {
            // Can't use Deref here as the Deref trait
            // doesn't preserve the incoming 'a lifetime,
            // and is instead bound by the anonymous lifetime of
            // the value passed in.
            self.0.into()
        } else {
            Cow::Owned::<'a, str>(self.replace(TRANSACTION_REPLACEMENT_STR, TRANSACTION_BEGIN_STR))
                .into()
        }
    }

    /// Parses this escaped message into a Message.
    fn parse(self) -> Result<Message<'a>, Error> {
        let parts = self.split(':');
        if parts.count() < 3 || !self.starts_with(TRANSACTION_BEGIN_STR) {
            return Ok(Message { transaction_id: None, message: self, is_txn_commit: false });
        }
        let mut parts = self.split(':');
        // Read header
        let header = parts.next();
        // Read TXN ID
        // SAFETY: This unwrap is safe because we've verified that there are 3 parts
        // to the message above.
        let txn_id = parts.next().unwrap().parse::<u64>()?;
        Ok(Message {
            transaction_id: Some(txn_id),
            message: "".into(),
            is_txn_commit: header == Some("TXN-COMMIT"),
        })
    }
}

impl<'a> UnescapedMessage<'a> {
    /// Sanitizes a message to be sent to the symbolizer. This is needed
    /// to ensure that strings starting with TXN: are not treated as a
    /// valid TXN if they are contained within a log message.
    /// We are being extra cautious here and replacing all instances of TXN
    /// with RTXN to prevent us from being susceiptible to Unicode-based
    /// attacks where certain Unicode sequences could look like newlines
    /// when viewed as bytes rather than as Unicode characters.
    /// It's safest to just replace all instances for this reason.
    fn sanitize(self) -> EscapedMessage<'a> {
        if !self.contains(TRANSACTION_BEGIN_STR) {
            // Can't use Deref here as the Deref trait
            // doesn't preserve the incoming 'a lifetime,
            // and is instead bound by the anonymous lifetime of
            // the value passed in.
            self.0.into()
        } else {
            Cow::Owned::<'a, str>(self.replace(TRANSACTION_BEGIN_STR, TRANSACTION_REPLACEMENT_STR))
                .into()
        }
    }
}

/// Real implementation of the symbolizer, not used by tests.
pub struct RealSymbolizerProcess {
    process: Child,
}

impl RealSymbolizerProcess {
    /// Constructs a new symbolizer with prettification enabled.
    pub async fn new() -> anyhow::Result<Self> {
        let sdk =
            global_env_context().context("Loading global environment context")?.get_sdk().await?;
        if let Err(e) = ensure_symbol_index_registered(&sdk).await {
            tracing::warn!("ensure_symbol_index_registered failed, error was: {:#?}", e);
        }

        let path = sdk.get_host_tool("symbolizer").context("getting symbolizer binary path")?;
        let c = Command::new(path)
            .args(vec![
                "--symbol-server",
                "gs://fuchsia-artifacts/debug",
                "--symbol-server",
                "gs://fuchsia-artifacts-internal/debug",
                "--symbol-server",
                "gs://fuchsia-artifacts-release/debug",
                "--prettify-backtrace",
            ])
            .stdout(Stdio::piped())
            .stdin(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("Spawning symbolizer")?;
        Ok(Self { process: c })
    }
}

impl SymbolizerProcess for RealSymbolizerProcess {
    type Stdin = ChildStdin;

    type Stdout = BufReader<ChildStdout>;

    fn take_stdin(&mut self) -> Option<Self::Stdin> {
        self.process.stdin.take()
    }

    fn take_stdout(&mut self) -> Option<Self::Stdout> {
        self.process.stdout.take().map(|stdout| BufReader::new(stdout))
    }
}

impl<T> TransactionalSymbolizer<T>
where
    T: SymbolizerProcess + 'static,
    T::Stdin: AsyncWrite + Debug + Unpin,
    T::Stdout: AsyncRead + Unpin + Debug,
{
    pub fn new(mut symbolizer_process: T) -> Result<Self, ReadError> {
        Ok(Self {
            pending_lines: RefCell::new(VecDeque::new()),
            state: LocalOrderedMutex::new(SymbolizerState::new(
                symbolizer_process.take_stdin().ok_or(ReadError::NoStdin)?,
                symbolizer_process.take_stdout().ok_or(ReadError::NoStdout)?,
            )),
            current_transaction_id: Cell::new(0),
            line_available_event: LocalConditionVariable::new(),
            disabled: Cell::new(false),
        })
    }

    async fn symbolize_internal(&self, mut entry: LogEntry) -> Result<LogEntry, ReadError> {
        // Get a transaction ID, later these will be generated on-device
        // and then translated to global IDs once devices support transactional logs.
        let id = self.current_transaction_id.get();
        self.current_transaction_id.set(id + 1);

        // Encode incoming log into transactional log and write to symbolizer
        let target_log = entry.data.as_target_log_mut().ok_or(ReadError::NoTargetLog)?;
        self.pending_lines.borrow_mut().push_back(format!(
            "TXN:{id}:\n{}\nTXN-COMMIT:{id}:COMMIT\n",
            UnescapedMessage::from(Cow::from(target_log.msg().unwrap_or("").to_string()))
                .sanitize()
                .to_string()
        ));

        // If an existing task was waiting for input, wake it.
        self.line_available_event.notify_one();
        // Execute symbolization, only one future can execute at once
        let mut state = self.state.lock().await;
        // Write any pending lines to the symbolizer and check for incoming messages
        // from the device.
        let (txn_id, msg) = loop {
            self.write_lines_to_symbolizer(&mut state).await?;
            // Check for incoming lines from the device, and output from the symbolizer concurrently.
            // Ordering doesn't matter here so select! is OK instead of select_biased!.
            let v = select! {
                res = state.stdout.next().fuse() => res,
                _ = self.line_available_event.clone().fuse() => None,
            };
            if let Some(v) = v {
                break v;
            }
        }?;

        // The TXN ID we get from the symbolizer should always match the expected TXN.
        assert_eq!(txn_id, id);
        let msg = EscapedMessage::from(msg.as_str()).unescape().to_string();
        // Message wasn't changed by the symbolizer.
        if msg == target_log.msg().unwrap_or("") {
            return Ok(entry);
        }
        // Message was changed by the symbolizer and is empty,
        // omit the message to avoid printing an extra newline.
        if msg.is_empty() {
            return Err(ReadError::EmptyOutputFromSymbolizer);
        }
        *target_log
            .msg_mut()
            .expect("if a symbolized message is provided then the payload has a message") = msg;
        Ok(entry)
    }

    /// Writes pending lines to the symbolizer
    async fn write_lines_to_symbolizer(
        &self,
        state: &mut SymbolizerState<T>,
    ) -> Result<(), ReadError> {
        while let Some(line) = self.pop_pending_line() {
            state.stdin.write_all(line.as_bytes()).await?;
        }
        Ok(())
    }

    /// Pops a pending line if one is available.
    fn pop_pending_line(&self) -> Option<String> {
        let mut pending_lines = self.pending_lines.borrow_mut();
        pending_lines.pop_front()
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ReadError {
    #[error("Mismatched transaction ID")]
    MismatchedTransaction,
    #[error(transparent)]
    UnknownError(#[from] anyhow::Error),
    #[error(transparent)]
    IoError(#[from] std::io::Error),
    #[error("No target log found in JSON")]
    NoTargetLog,
    #[error("Failed to take stdin")]
    NoStdin,
    #[error("Failed to take stdout")]
    NoStdout,
    #[error("Not an error -- empty symbolizer output")]
    EmptyOutputFromSymbolizer,
    #[error("Unexpected message received before a valid transaction ID")]
    UnexpectedMessageOutsideTransaction,
}

#[derive(Debug)]
enum TransactionState<'a> {
    /// Initial state
    Start,
    /// In transaction with ID and partial message
    InTransaction { transaction_id: u64, message: UnescapedMessage<'a>, is_first_message: bool },
}

impl<'a> Default for TransactionState<'a> {
    fn default() -> Self {
        Self::Start
    }
}

/// Transaction reader, which accepts input strings and outputs
/// transaction data.
#[derive(Default, Debug)]
struct TransactionReader<'a> {
    state: TransactionState<'a>,
}

impl<'a> TransactionReader<'a> {
    /// Pushes a line of text to the reader, returning information about a completed
    /// transaction if committed.
    fn push_line(&mut self, line: EscapedMessage<'a>) -> Result<Option<(u64, String)>, ReadError> {
        let msg = line.parse()?;
        let mut state = TransactionState::Start;
        swap(&mut state, &mut self.state);
        match (state, msg.is_txn_commit, msg.transaction_id) {
            // Not in transaction, no TXN ID to associate with
            (TransactionState::Start, false, None) | (TransactionState::Start, true, _) => {
                Err(ReadError::UnexpectedMessageOutsideTransaction)
            }
            // Not in transaction, first message containing TXN ID
            (TransactionState::Start, false, Some(txn_id)) => {
                self.state = TransactionState::InTransaction {
                    transaction_id: txn_id,
                    message: msg.message.unescape().into(),
                    is_first_message: true,
                };
                Ok(None)
            }
            // Mismatched transaction IDs when commit is specified
            (TransactionState::InTransaction { transaction_id, .. }, true, Some(id))
                if id != transaction_id =>
            {
                Err(ReadError::MismatchedTransaction)
            }
            // Commit transaction
            (TransactionState::InTransaction { transaction_id, message, .. }, true, _) => {
                Ok(Some((transaction_id, (*message).into())))
            }
            // Inside transaction (first message)
            (
                TransactionState::InTransaction { transaction_id, message, is_first_message },
                false,
                _,
            ) => {
                let mut s = message.to_string();
                if !is_first_message {
                    s.push('\n');
                }
                s.push_str(&*msg.message.unescape());
                self.state = TransactionState::InTransaction {
                    transaction_id,
                    message: Cow::Owned::<'a, str>(s).into(),
                    is_first_message: false,
                };
                Ok(None)
            }
        }
    }
}

/// Asynchronous version of TransactionReader,
/// which can read from a symbolizer's stdout
/// and return transactions as they complete.
#[pin_project]
#[derive(Debug)]
struct AsyncTransactionReader<T> {
    reader: TransactionReader<'static>,
    #[pin]
    lines: Lines<BufReader<T>>,
}

impl<T> AsyncTransactionReader<T>
where
    T: AsyncRead + Unpin,
{
    /// Creates a new async transaction reader in the reading state.
    fn new(stream: T) -> Self {
        let lines = BufReader::new(stream).lines();
        Self { reader: Default::default(), lines }
    }
}

impl<T> Stream for AsyncTransactionReader<T>
where
    T: AsyncRead + Unpin,
{
    type Item = Result<(u64, String), ReadError>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let this = self.project();
        match ready!(this.lines.poll_next_line(cx)) {
            Ok(Some(line)) => {
                // Tokio's poll works different than futures::Stream.
                // It expects poll to be called continuously until it returns
                // Pending, rather than itself signalling the waker if more space
                // is available in the buffer.
                // If it returns a line, it's necessary to schedule another wakeup
                // so that additional lines are returned if they are available in the buffer.
                cx.waker().wake_by_ref();
                match this.reader.push_line(Cow::from(line).into()) {
                    Ok(Some(value)) => Poll::Ready(Some(Ok(value))),
                    Ok(None) => Poll::Pending,
                    Err(e) => Poll::Ready(Some(Err(e))),
                }
            }
            Err(err) => Poll::Ready(Some(Err(err.into()))),
            Ok(None) => Poll::Ready(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use diagnostics_data::{BuilderArgs, LogsDataBuilder, Severity, Timestamp};
    use fuchsia_sync::Mutex;
    use futures::Future;
    use std::fmt::Write;
    use std::{
        sync::Arc,
        task::{Context, Wake},
    };
    use tokio::io::{duplex, DuplexStream};

    /// Size of duplex buffer
    const DUPLEX_BUFFER_SIZE: usize = 4096;

    /// Fake waker for testing purposes. Allows a test to determine
    /// whether or not a future is expecting further calls to poll.
    #[derive(Default)]
    struct FakeWaker {
        woke: Mutex<bool>,
    }

    impl FakeWaker {
        fn reset(&self) {
            *self.woke.lock() = false;
        }

        /// Polls a future until it both returns Poll::Pending
        /// the waker isn't woke or the future returns a result.
        /// This method polls the future at least once.
        /// This is needed due to implementation details of Tokio,
        /// which frequently yields to the executor in order to allow
        /// for cooperative multitasking.
        fn poll_while_woke<T>(
            self: Arc<Self>,
            mut future: Pin<&mut (impl Future<Output = T> + ?Sized)>,
        ) -> Poll<T> {
            let waker = self.clone().into();
            let mut context = Context::from_waker(&waker);
            self.clone().wake();
            while *self.woke.lock() {
                self.reset();
                if let Poll::Ready(result) = future.as_mut().poll(&mut context) {
                    return Poll::Ready(result);
                }
            }
            Poll::Pending
        }
    }

    impl Wake for FakeWaker {
        fn wake(self: std::sync::Arc<Self>) {
            *self.woke.lock() = true;
        }
    }

    #[derive(Debug)]
    struct FakeProcess {
        stdin: Option<DuplexStream>,
        stdout: Option<DuplexStream>,
    }

    impl SymbolizerProcess for FakeProcess {
        type Stdin = DuplexStream;

        type Stdout = DuplexStream;

        fn take_stdin(&mut self) -> Option<Self::Stdin> {
            self.stdin.take()
        }

        fn take_stdout(&mut self) -> Option<Self::Stdout> {
            self.stdout.take()
        }
    }

    impl FakeProcess {
        fn new(stdin: DuplexStream, stdout: DuplexStream) -> Self {
            Self { stdin: Some(stdin), stdout: Some(stdout) }
        }
    }

    #[fuchsia::test]
    async fn string_escape_test() {
        // Escaped string
        let input = "TXN:0:start\nHello world!\n\n\nTest line 2\nRTXN:0:nothing\nTXN-COMMIT:0:COMMIT\nTXN:1:start";
        // Escaped view of string
        let unescaped_input = UnescapedMessage::from(input);

        assert_eq!(unescaped_input.clone().sanitize(), "RTXN:0:start\nHello world!\n\n\nTest line 2\nRRTXN:0:nothing\nRTXN-COMMIT:0:COMMIT\nRTXN:1:start");
        // Roundtrip
        assert_eq!(unescaped_input.sanitize().unescape(), input);
        // No-op
        assert_eq!(UnescapedMessage::from("nop").sanitize(), "nop");
        assert_eq!(UnescapedMessage::from("nop").sanitize(), "nop");
    }

    #[fuchsia::test]
    async fn async_read_transacted_test() {
        let input = BufReader::new("TXN:0:start\nHello world!\n\n\nTest line 2\nRTXN:0:escaped message\nTXN-COMMIT:0:COMMIT\nTXN:1:second\nsecond transaction\nTXN-COMMIT:1:end\nTXN:1:start".as_bytes());
        let mut reader = AsyncTransactionReader::new(input);
        let (txn, msg) = assert_matches!(reader.next().await, Some(Ok(value)) => value);
        assert_eq!(msg, "Hello world!\n\n\nTest line 2\nTXN:0:escaped message");
        assert_eq!(txn, 0);
        let (txn, msg) = assert_matches!(reader.next().await, Some(Ok(value)) => value);
        assert_eq!(msg, "second transaction");
        assert_eq!(txn, 1);
        assert_matches!(reader.next().await, None);
    }

    #[fuchsia::test]
    async fn read_transacted_test() {
        let mut reader = TransactionReader::default();
        let stdout = "TXN:0:start\nHello world!\n\n\nTest line 2\nRTXN:0:nothing\nTXN-COMMIT:0:COMMIT\nTXN:1:start";
        let lines = stdout.lines();
        let mut output = String::new();
        for line in lines {
            if let Some((transaction_id, message)) = reader.push_line(line.into()).unwrap() {
                assert_eq!(transaction_id, 0);
                write!(output, "{message}").unwrap();
            }
        }
        assert_eq!(output, "Hello world!\n\n\nTest line 2\nTXN:0:nothing");
    }

    #[fuchsia::test]
    async fn read_transacted_integration_test() {
        let fake_waker = Arc::new(FakeWaker::default());
        let txn = LogEntry {
            timestamp: 0.into(),
            data: LogData::TargetLog(
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".into()),
                    moniker: "ffx".into(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(0),
                })
                .set_pid(1)
                .set_tid(2)
                .set_message("Hello world!")
                .build(),
            ),
        };
        let mut txn_data = assert_matches!(txn.data.as_target_log(), Some(value) => value).clone();
        let txn_2 = LogEntry {
            timestamp: 1.into(),
            data: LogData::TargetLog(
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".into()),
                    moniker: "ffx".into(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(1),
                })
                .set_pid(1)
                .set_tid(2)
                .set_message("Unused world!")
                .build(),
            ),
        };
        let mut txn_data_2 =
            assert_matches!(txn_2.data.as_target_log(), Some(value) => value).clone();
        let (stdin_sender, stdin_receiver) = duplex(DUPLEX_BUFFER_SIZE);
        let (mut stdout_sender, stdout_receiver) = duplex(DUPLEX_BUFFER_SIZE);
        let symbolizer = assert_matches!(
            TransactionalSymbolizer::new(FakeProcess::new(stdin_sender, stdout_receiver)),
            Ok(value) => value
        );
        let mut symbolize_task_0 = symbolizer.symbolize(txn);
        let mut symbolize_task_1 = symbolizer.symbolize(txn_2);
        // Both tasks should be in pending state until symbolizer transactions complete.
        assert_eq!(fake_waker.clone().poll_while_woke(symbolize_task_0.as_mut()), Poll::Pending);
        assert_eq!(fake_waker.clone().poll_while_woke(symbolize_task_1.as_mut()), Poll::Pending);

        let mut reader = BufReader::new(stdin_receiver).lines();
        assert_matches!(reader.next_line().await, Ok(Some(value)) if value == "TXN:0:");
        assert_matches!(reader.next_line().await, Ok(Some(value)) if value == "Hello world!");
        assert_matches!(stdout_sender.write_all("TXN:0:start\nHello world!\n\n\nTest line 2\nRTXN:0:nothing\nTXN-COMMIT:0:COMMIT\n".as_bytes()).await, Ok(()));
        // TXN 1 shouldn't be completed
        assert_eq!(fake_waker.clone().poll_while_woke(symbolize_task_1.as_mut()), Poll::Pending);
        // TXN 0 should be completed
        // Hello world!\n\n\nTest line 2\nTXN:0:nothing
        *txn_data.msg_mut().unwrap() = "Hello world!\n\n\nTest line 2\nTXN:0:nothing".into();
        assert_eq!(
            fake_waker.clone().poll_while_woke(symbolize_task_0.as_mut()),
            Poll::Ready(Some(LogEntry {
                data: LogData::TargetLog(txn_data.clone()),
                timestamp: Timestamp::from(0),
            }))
        );
        // TXN 1 should still not be completed.
        assert_eq!(fake_waker.clone().poll_while_woke(symbolize_task_1.as_mut()), Poll::Pending);
        // Complete the second transaction
        assert_matches!(
            stdout_sender
                .write_all("TXN:1:start\nHello world 2!\nTXN-COMMIT:1:COMMIT\n".as_bytes())
                .await,
            Ok(())
        );

        // TXN 1 should have completed
        // Hello world 2!
        *txn_data_2.msg_mut().unwrap() = "Hello world 2!".into();
        assert_eq!(
            fake_waker.clone().poll_while_woke(symbolize_task_1.as_mut()),
            Poll::Ready(Some(LogEntry {
                data: LogData::TargetLog(txn_data_2.clone()),
                timestamp: Timestamp::from(1),
            }))
        );
    }

    #[fuchsia::test]
    async fn read_transacted_malformed_input_test() {
        let fake_waker = Arc::new(FakeWaker::default());
        let txn = LogEntry {
            timestamp: 0.into(),
            data: LogData::TargetLog(
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".into()),
                    moniker: "ffx".into(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(0),
                })
                .set_pid(1)
                .set_tid(2)
                .set_message("Hello world!")
                .build(),
            ),
        };
        let txn_2 = LogEntry {
            timestamp: 1.into(),
            data: LogData::TargetLog(
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".into()),
                    moniker: "ffx".into(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(1),
                })
                .set_pid(1)
                .set_tid(2)
                .set_message("Not symbolized")
                .build(),
            ),
        };
        let txn_2_clone = txn_2.clone();
        let (stdin_sender, stdin_receiver) = duplex(DUPLEX_BUFFER_SIZE);
        let (mut stdout_sender, stdout_receiver) = duplex(DUPLEX_BUFFER_SIZE);
        let symbolizer = assert_matches!(
            TransactionalSymbolizer::new(FakeProcess::new(stdin_sender, stdout_receiver)),
            Ok(value) => value
        );
        let mut symbolize_task_0 = symbolizer.symbolize(txn);
        let mut symbolize_task_1 = symbolizer.symbolize(txn_2);
        // Task should be pending initially.
        assert_eq!(fake_waker.clone().poll_while_woke(symbolize_task_0.as_mut()), Poll::Pending);
        let mut reader = BufReader::new(stdin_receiver).lines();
        // Read the output from the device
        assert_matches!(reader.next_line().await, Ok(Some(value)) if value == "TXN:0:");
        assert_matches!(reader.next_line().await, Ok(Some(value)) if value == "Hello world!");
        assert_matches!(
            stdout_sender.write_all("this is invalid input\n".as_bytes()).await,
            Ok(())
        );
        // TXN should complete with error
        assert_eq!(
            fake_waker.clone().poll_while_woke(symbolize_task_0.as_mut()),
            Poll::Ready(Some(LogEntry {
                data: LogData::MalformedTargetLog(
                    "Unexpected message received before a valid transaction ID".into()
                ),
                timestamp: Timestamp::from(0),
            }))
        );
        // Symbolizer should be disabled by the error
        assert_eq!(
            fake_waker.clone().poll_while_woke(symbolize_task_1.as_mut()),
            Poll::Ready(Some(txn_2_clone))
        );
    }

    #[fuchsia::test]
    async fn read_transacted_should_discard_empty_lines_test() {
        let fake_waker = Arc::new(FakeWaker::default());
        let txn = LogEntry {
            timestamp: 0.into(),
            data: LogData::TargetLog(
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".into()),
                    moniker: "ffx".into(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(0),
                })
                .set_pid(1)
                .set_tid(2)
                .set_message("Hello world!")
                .build(),
            ),
        };
        let (stdin_sender, stdin_receiver) = duplex(DUPLEX_BUFFER_SIZE);
        let (mut stdout_sender, stdout_receiver) = duplex(DUPLEX_BUFFER_SIZE);
        let symbolizer = assert_matches!(
            TransactionalSymbolizer::new(FakeProcess::new(stdin_sender, stdout_receiver)),
            Ok(value) => value
        );
        let mut symbolize_task_0 = symbolizer.symbolize(txn);
        // Task should be pending initially.
        assert_eq!(fake_waker.clone().poll_while_woke(symbolize_task_0.as_mut()), Poll::Pending);
        let mut reader = BufReader::new(stdin_receiver).lines();
        // Read the output from the device
        assert_matches!(reader.next_line().await, Ok(Some(value)) if value == "TXN:0:");
        assert_matches!(reader.next_line().await, Ok(Some(value)) if value == "Hello world!");
        assert_matches!(
            stdout_sender.write_all("TXN:0:ignored\nTXN-COMMIT:0:end\n".as_bytes()).await,
            Ok(())
        );
        // TXN should discard the log message (this matches
        // previous behavior in log_formatter for empty symbolized messages).
        // The symbolizer sends an empty line when given an input to indicate
        // the message should be discarded.
        assert_eq!(
            fake_waker.clone().poll_while_woke(symbolize_task_0.as_mut()),
            Poll::Ready(None)
        );
    }

    #[fuchsia::test]
    async fn transaction_parser_test() {
        // Invalid transaction
        assert_matches!(EscapedMessage::from("TXN:notaninteger:this is not valid").parse(), Err(_));
        // Valid transaction with ignored part. Messages should be on their own line,
        // because they might contain symbolizer markup.
        assert_matches!(
            EscapedMessage::from("TXN:42:ignored").parse(),
            Ok(msg) if msg == Message {
                is_txn_commit: false,
                message: "".into(),
                transaction_id: Some(42),
            }
        );
        // Commit transaction, which indicates the entire message
        // has been handled by the symbolizer.
        assert_matches!(
            EscapedMessage::from("TXN-COMMIT:42:ignored").parse(),
            Ok(msg) if msg == Message {
                is_txn_commit: true,
                message: "".into(),
                transaction_id: Some(42),
            }
        );
        // Message, which might either be symbolizer markup, or just
        // our own message
        assert_matches!(
            EscapedMessage::from("not ignored, possible symbolizer markup").parse(),
            Ok(msg) if msg == Message {
                is_txn_commit: false,
                message: "not ignored, possible symbolizer markup".into(),
                transaction_id: None,
            }
        );

        // Message where the symbolizer markup starts with TXN: for some reason
        // but is not actually a transaction.
        assert_matches!(
            EscapedMessage::from("TXN:").parse(),
            Ok(msg) if msg == Message {
                is_txn_commit: false,
                message: "TXN:".into(),
                transaction_id: None,
            }
        );
    }
}
