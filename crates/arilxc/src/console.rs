//! Interactive console sessions ([`ContainerBackend::console`](crate::ContainerBackend::console)).

use std::any::Any;
use std::pin::Pin;
use std::task::{Context, Poll};

pub use arilbox_core::exec::TermSize;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::{Error, Result};

/// The input side of a console: keystrokes go in, and the terminal can be resized.
pub trait ConsoleInput: AsyncWrite + Send + Unpin {
    /// Changes the terminal size; programs in the container see `SIGWINCH`.
    fn resize(&mut self, size: TermSize) -> Result<()>;
}

/// A shell in a running container, attached to a terminal.
///
/// `output` ends (EOF or an error) when the shell exits. Dropping the console ends the
/// session and kills whatever runs it.
pub struct Console {
    /// Terminal output (raw bytes, usually UTF-8 with escape sequences).
    pub output: Box<dyn AsyncRead + Send + Unpin>,
    /// Terminal input.
    pub input: Box<dyn ConsoleInput>,
    /// Keeps the session alive (e.g. the `lxc-attach` process); drop to end it.
    pub session: Box<dyn Any + Send>,
}

impl std::fmt::Debug for Console {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Console").finish_non_exhaustive()
    }
}

/// Input half of a host pseudo-terminal.
pub(crate) struct PtyInput(pub(crate) pty_process::OwnedWritePty);

impl ConsoleInput for PtyInput {
    fn resize(&mut self, size: TermSize) -> Result<()> {
        self.0
            .resize(pty_process::Size::new(size.rows, size.cols))
            .map_err(|e| Error::Backend(format!("resizing console: {e}")))
    }
}

impl AsyncWrite for PtyInput {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}
