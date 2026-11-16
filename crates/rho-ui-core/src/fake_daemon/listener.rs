//! Where the fake daemon listens: a Unix socket, or on Windows a named pipe,
//! as rho's daemon does.

use std::{io, path::Path};

#[cfg(unix)]
pub(super) use unix::Listener;
#[cfg(windows)]
pub(super) use windows::Listener;

#[cfg(unix)]
mod unix {
    use super::{io, Path};
    use tokio::net::{UnixListener, UnixStream};

    pub(in crate::fake_daemon) struct Listener(UnixListener);

    impl Listener {
        /// Binds the socket at `path`. Call inside a runtime.
        pub(in crate::fake_daemon) fn bind(path: &Path) -> io::Result<Self> {
            UnixListener::bind(path).map(Self)
        }

        pub(in crate::fake_daemon) async fn accept(&mut self) -> io::Result<UnixStream> {
            Ok(self.0.accept().await?.0)
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::OsString;

    use super::{io, Path};
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

    /// A pipe has no backlog: a client can open it only while an instance
    /// nobody took yet waits, so the next one is made as each is taken.
    pub(in crate::fake_daemon) struct Listener {
        name: OsString,
        next: NamedPipeServer,
    }

    impl Listener {
        /// Creates the pipe `path`. Call inside a runtime.
        pub(in crate::fake_daemon) fn bind(path: &Path) -> io::Result<Self> {
            let next = ServerOptions::new()
                .first_pipe_instance(true)
                .create(path)?;
            Ok(Self {
                name: path.as_os_str().to_owned(),
                next,
            })
        }

        pub(in crate::fake_daemon) async fn accept(&mut self) -> io::Result<NamedPipeServer> {
            self.next.connect().await?;
            let next = ServerOptions::new().create(&self.name)?;
            Ok(std::mem::replace(&mut self.next, next))
        }
    }
}
