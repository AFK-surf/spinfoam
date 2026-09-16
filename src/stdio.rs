//! Nonblocking Unix pipes/sockets: no background blocking stdin thread.
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{
    io::unix::AsyncFd,
    io::{AsyncRead, AsyncWrite, ReadBuf},
};

pub struct Stdio {
    fd: AsyncFd<OwnedFd>,
    original_flags: i32,
}
impl Stdio {
    pub fn new(fd: i32) -> io::Result<Self> {
        // SAFETY: fcntl creates an owned duplicate; fstat writes a valid stat buffer.
        unsafe {
            let dup = libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3);
            if dup < 0 {
                return Err(io::Error::last_os_error());
            }
            let owned = OwnedFd::from_raw_fd(dup);
            let mut stat = std::mem::zeroed::<libc::stat>();
            if libc::fstat(dup, &mut stat) != 0 {
                return Err(io::Error::last_os_error());
            }
            if !matches!(stat.st_mode & libc::S_IFMT, libc::S_IFIFO | libc::S_IFSOCK) {
                return Err(io::Error::other("stdio must use pipes or Unix sockets"));
            }
            let original_flags = libc::fcntl(dup, libc::F_GETFL);
            if original_flags < 0
                || libc::fcntl(dup, libc::F_SETFL, original_flags | libc::O_NONBLOCK) < 0
            {
                return Err(io::Error::last_os_error());
            }
            match AsyncFd::new(owned) {
                Ok(fd) => Ok(Self { fd, original_flags }),
                Err(e) => {
                    libc::fcntl(fd, libc::F_SETFL, original_flags);
                    Err(e)
                }
            }
        }
    }
}
impl Drop for Stdio {
    fn drop(&mut self) {
        unsafe {
            libc::fcntl(self.fd.as_raw_fd(), libc::F_SETFL, self.original_flags);
        }
    }
}
impl AsyncRead for Stdio {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            let mut ready = std::task::ready!(self.fd.poll_read_ready(cx))?;
            match ready.try_io(|fd| {
                let dst = buf.initialize_unfilled();
                let n = unsafe { libc::read(fd.as_raw_fd(), dst.as_mut_ptr().cast(), dst.len()) };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(Ok(n)) => {
                    buf.advance(n);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => continue,
                Ok(Err(e)) => return Poll::Ready(Err(e)),
                Err(_) => continue,
            }
        }
    }
}
impl AsyncWrite for Stdio {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            let mut ready = std::task::ready!(self.fd.poll_write_ready(cx))?;
            match ready.try_io(|fd| {
                let n = unsafe { libc::write(fd.as_raw_fd(), buf.as_ptr().cast(), buf.len()) };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => continue,
                Ok(result) => return Poll::Ready(result),
                Err(_) => continue,
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
