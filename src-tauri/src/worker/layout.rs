use std::fs;
use std::io;
use std::io::Write;
use std::os::unix::fs::{symlink, OpenOptionsExt};
use std::path::Path;

use super::board_local;
use crate::paths::AppPaths;

pub fn ensure(paths: &AppPaths, board: &Path, app_executable: &Path) -> io::Result<()> {
    for dir in [
        paths.worker_bin_dir(),
        paths.worker_config_dir(),
        paths.worker_state_dir(),
        paths.worker_run_dir(),
    ] {
        fs::create_dir_all(dir)?;
    }
    point(&paths.worker_board(), board)?;
    write_script(&paths.worker_board_local(), &board_local::script(paths))?;
    point(&paths.worker_credentials(), app_executable)?;
    point(&paths.worker_credentials_pending(), app_executable)
}

fn write_script(file: &Path, content: &str) -> io::Result<()> {
    if fs::read_to_string(file).is_ok_and(|current| current == content) {
        return Ok(());
    }
    let staged = file.with_extension("tmp");
    let mut handle = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o755)
        .open(&staged)?;
    handle.write_all(content.as_bytes())?;
    drop(handle);
    fs::rename(staged, file)
}

fn point(link: &Path, target: &Path) -> io::Result<()> {
    if fs::read_link(link).is_ok_and(|current| current == target) {
        return Ok(());
    }
    match fs::symlink_metadata(link) {
        Ok(_) => fs::remove_file(link)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    symlink(target, link)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_links_are_created_kept_and_recreated_when_they_point_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(dir.path());
        let first = dir.path().join("first-board");
        let second = dir.path().join("second-board");
        let app = dir.path().join("app");

        ensure(&paths, &first, &app).unwrap();
        assert_eq!(fs::read_link(paths.worker_board()).unwrap(), first);
        assert_eq!(fs::read_link(paths.worker_credentials()).unwrap(), app);
        assert_eq!(
            fs::read_link(paths.worker_credentials_pending()).unwrap(),
            app
        );
        for sub in ["config", "state", "run"] {
            assert!(paths.worker_dir().join(sub).is_dir(), "{sub}");
        }

        ensure(&paths, &first, &app).unwrap();
        assert_eq!(fs::read_link(paths.worker_board()).unwrap(), first);

        ensure(&paths, &second, &app).unwrap();
        assert_eq!(fs::read_link(paths.worker_board()).unwrap(), second);
    }

    #[test]
    fn the_helper_script_is_executable_left_alone_when_unchanged_and_rewritten_when_different() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(dir.path());
        let board = dir.path().join("board");
        let app = dir.path().join("app");
        let script = paths.worker_board_local();

        ensure(&paths, &board, &app).unwrap();
        assert_eq!(
            fs::read_to_string(&script).unwrap(),
            board_local::script(&paths)
        );
        assert_eq!(
            fs::metadata(&script).unwrap().permissions().mode() & 0o777,
            0o755
        );

        let first_write = fs::metadata(&script).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        ensure(&paths, &board, &app).unwrap();
        assert_eq!(
            fs::metadata(&script).unwrap().modified().unwrap(),
            first_write
        );

        fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        ensure(&paths, &board, &app).unwrap();
        assert_eq!(
            fs::read_to_string(&script).unwrap(),
            board_local::script(&paths)
        );
    }
}
