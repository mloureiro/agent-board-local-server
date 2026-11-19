use std::path::{Path, PathBuf};

use percent_encoding::percent_decode_str;

const INDEX: &str = "index.html";
const ASSETS_PREFIX: &str = "/assets/";

#[derive(Debug, PartialEq, Eq)]
pub enum Served {
    File {
        path: PathBuf,
        immutable: bool,
    },
    NotFound,
    /// The path tries to leave the frontend folder or is not a path at all.
    Refused,
}

pub struct StaticFiles {
    root: PathBuf,
}

impl StaticFiles {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
        }
    }

    /// `/assets/*` is the hashed bundle and is never replaced by the page; anything else that is not a file
    /// is a route of the single-page app and gets `index.html`.
    pub fn resolve(&self, raw_path: &str) -> Served {
        let Some(segments) = safe_segments(raw_path) else {
            return Served::Refused;
        };
        let assets = raw_path.starts_with(ASSETS_PREFIX);
        let wanted = segments
            .iter()
            .fold(self.root.clone(), |dir, segment| dir.join(segment));

        if let Some(path) = self.existing_file(&wanted) {
            return Served::File {
                path,
                immutable: assets,
            };
        }
        if assets {
            return Served::NotFound;
        }
        match self.existing_file(&self.root.join(INDEX)) {
            Some(path) => Served::File {
                path,
                immutable: false,
            },
            None => Served::NotFound,
        }
    }

    /// A directory answers with its own `index.html`; a symlink out of the root is not followed.
    fn existing_file(&self, candidate: &Path) -> Option<PathBuf> {
        let resolved = if candidate.is_dir() {
            candidate.join(INDEX)
        } else {
            candidate.to_path_buf()
        };
        let real = resolved.canonicalize().ok()?;
        (real.starts_with(&self.root) && real.is_file()).then_some(real)
    }
}

/// Decodes once, then refuses anything that could point outside the root.
fn safe_segments(raw_path: &str) -> Option<Vec<String>> {
    let decoded = percent_decode_str(raw_path).decode_utf8().ok()?;
    if decoded.contains(['\0', '\\']) || !decoded.starts_with('/') {
        return None;
    }
    let mut segments = Vec::new();
    for segment in decoded.split('/') {
        match segment {
            "" | "." => {}
            ".." => return None,
            other => segments.push(other.to_string()),
        }
    }
    Some(segments)
}

pub fn content_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("wasm") => "application/wasm",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn site() -> (tempfile::TempDir, StaticFiles) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("frontend");
        fs::create_dir_all(root.join("assets")).unwrap();
        fs::write(root.join("index.html"), "<html>").unwrap();
        fs::write(root.join("favicon.svg"), "<svg>").unwrap();
        fs::write(root.join("assets/app.js"), "x").unwrap();
        fs::write(dir.path().join("secret.txt"), "top secret").unwrap();
        let files = StaticFiles::new(&root);
        (dir, files)
    }

    fn file_name(served: &Served) -> Option<(&str, bool)> {
        match served {
            Served::File { path, immutable } => {
                Some((path.file_name().unwrap().to_str().unwrap(), *immutable))
            }
            _ => None,
        }
    }

    #[test]
    fn paths_resolve_to_a_file_the_page_or_a_refusal() {
        let (_dir, files) = site();
        let cases: &[(&str, Option<(&str, bool)>)] = &[
            ("/", Some(("index.html", false))),
            ("/boards/42/settings", Some(("index.html", false))),
            ("/favicon.svg", Some(("favicon.svg", false))),
            ("/assets/app.js", Some(("app.js", true))),
            ("/assets/missing.js", None),
            ("/assets/", None),
            ("/assets", Some(("index.html", false))),
        ];
        for (path, expected) in cases {
            let served = files.resolve(path);
            match expected {
                Some(_) => assert_eq!(file_name(&served), *expected, "{path}"),
                None => assert_eq!(served, Served::NotFound, "{path}"),
            }
        }
    }

    #[test]
    fn traversal_and_malformed_paths_are_refused() {
        let (_dir, files) = site();
        for path in [
            "/assets/..%2f..%2fsecret.txt",
            "/assets/%2e%2e/%2e%2e/secret.txt",
            "/../secret.txt",
            "/assets/..%5c..%5csecret.txt",
            "/a%00b",
            "/%ff",
        ] {
            assert_eq!(files.resolve(path), Served::Refused, "{path}");
        }
    }

    #[test]
    fn a_double_encoded_dot_is_a_plain_missing_file() {
        let (_dir, files) = site();
        assert_eq!(
            files.resolve("/assets/%252e%252e/secret.txt"),
            Served::NotFound
        );
    }

    #[test]
    fn a_symlink_out_of_the_root_is_not_followed() {
        let (dir, files) = site();
        std::os::unix::fs::symlink(
            dir.path().join("secret.txt"),
            dir.path().join("frontend/assets/leak.txt"),
        )
        .unwrap();
        assert_eq!(files.resolve("/assets/leak.txt"), Served::NotFound);
    }
}
