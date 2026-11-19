use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

#[derive(Debug)]
pub enum LockError {
    AlreadyRunning,
    Io(io::Error),
}

/// The lock lives as long as this value: the kernel drops it when the process dies, however it dies.
#[derive(Debug)]
pub struct Lock {
    _file: File,
}

pub fn acquire(path: &Path) -> Result<Lock, LockError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(LockError::Io)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(LockError::Io)?;
    // SAFETY: the descriptor belongs to `file`, which outlives the call.
    let status = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if status == 0 {
        return Ok(Lock { _file: file });
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::WouldBlock {
        Err(LockError::AlreadyRunning)
    } else {
        Err(LockError::Io(error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_holder_is_refused_until_the_first_lets_go() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.lock");

        let first = acquire(&path).unwrap();
        assert!(matches!(acquire(&path), Err(LockError::AlreadyRunning)));
        drop(first);
        assert!(acquire(&path).is_ok());
    }
}
