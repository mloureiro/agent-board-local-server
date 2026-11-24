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
    lock(file)
}

pub fn acquire_existing(path: &Path) -> Result<Option<Lock>, LockError> {
    match OpenOptions::new().write(true).open(path) {
        Ok(file) => lock(file).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(LockError::Io(error)),
    }
}

fn lock(file: File) -> Result<Lock, LockError> {
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
