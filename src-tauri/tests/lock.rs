// Never spawn a process in this file: a spawn in progress in the same process holds a copy of the locked file's description, so the release on drop is not observable.
use agent_board_local_server::lock::{acquire, LockError};

#[test]
fn a_second_holder_is_refused_until_the_first_lets_go() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.lock");

    let first = acquire(&path).unwrap();
    assert!(matches!(acquire(&path), Err(LockError::AlreadyRunning)));
    drop(first);
    assert!(acquire(&path).is_ok());
}
