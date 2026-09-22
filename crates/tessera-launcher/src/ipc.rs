//! Talking to the compositor (design §4).
//!
//! Requests are short-lived: connect, send one line, read one line, close. The
//! compositor identifies the caller from the socket's peer credentials, which
//! are the launcher's either way, so nothing is lost by not holding the
//! connection open. Events need a long-lived connection instead; see
//! [`Ipc::subscribe`](crate::worker) in `worker.rs`.

use std::{
    io::{BufReader, ErrorKind},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};

use tessera_ipc::{Request, Response};

/// The compositor socket, when Tessera started this process.
pub struct Ipc {
    path: PathBuf,
}

impl Ipc {
    /// Finds the socket from the environment, or `None` outside Tessera.
    pub fn from_env() -> Option<Self> {
        tessera_ipc::socket_from_env().map(|path| Self { path })
    }

    /// The socket's path.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Sends one request and waits for the answer.
    pub fn request(&self, request: &Request) -> std::io::Result<Response> {
        let stream = UnixStream::connect(&self.path)?;
        // The compositor answers immediately; a hang should not freeze the menu.
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;

        let mut writer = &stream;
        tessera_ipc::write_message(&mut writer, request)?;

        let mut reader = BufReader::new(&stream);
        tessera_ipc::read_message(&mut reader)?.ok_or_else(|| {
            std::io::Error::new(
                ErrorKind::UnexpectedEof,
                "the compositor closed the connection",
            )
        })
    }
}
