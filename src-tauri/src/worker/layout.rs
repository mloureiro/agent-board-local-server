use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::Path;

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
    point(&paths.worker_credentials(), app_executable)
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
        for sub in ["config", "state", "run"] {
            assert!(paths.worker_dir().join(sub).is_dir(), "{sub}");
        }

        ensure(&paths, &first, &app).unwrap();
        assert_eq!(fs::read_link(paths.worker_board()).unwrap(), first);

        ensure(&paths, &second, &app).unwrap();
        assert_eq!(fs::read_link(paths.worker_board()).unwrap(), second);
    }
}
