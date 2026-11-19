use http::header::{HeaderMap, HeaderName, HeaderValue};

/// Sent on every response, copied from the pinned Caddyfile's site-level `header` block.
pub const PAGE: &[(&str, &str)] = &[
    (
        "content-security-policy",
        "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; worker-src 'none'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'",
    ),
    ("referrer-policy", "no-referrer"),
    ("x-content-type-options", "nosniff"),
    ("cross-origin-opener-policy", "same-origin"),
    ("cross-origin-resource-policy", "same-origin"),
];

/// Written last on `/trpc`, `/api` and `/healthz`, copied from the Caddyfile's `data_headers` snippet.
pub const DATA: &[(&str, &str)] = &[
    ("content-type", "application/json; charset=utf-8"),
    ("content-security-policy", "sandbox; default-src 'none'"),
    ("x-content-type-options", "nosniff"),
    ("cross-origin-resource-policy", "same-origin"),
    ("content-disposition", "attachment"),
    ("cache-control", "no-store"),
];

pub const ASSET_CACHE: &str = "public, max-age=31536000, immutable";
pub const PAGE_CACHE: &str = "no-cache";

fn write(headers: &mut HeaderMap, table: &[(&'static str, &'static str)]) {
    for (name, value) in table {
        headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
}

pub fn apply_page(headers: &mut HeaderMap) {
    write(headers, PAGE);
}

pub fn apply_data(headers: &mut HeaderMap) {
    write(headers, DATA);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tables_match_the_pinned_caddyfile_and_the_data_set_wins_on_overlap() {
        let caddyfile =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/resources/Caddyfile"))
                .expect("pnpm core:build stages the Caddyfile");
        for (_, value) in PAGE.iter().chain(DATA) {
            assert!(
                caddyfile.contains(&format!("\"{value}\"")),
                "{value} is not in the pinned Caddyfile"
            );
        }
        for value in [ASSET_CACHE, PAGE_CACHE] {
            assert!(caddyfile.contains(&format!("\"{value}\"")));
        }

        let mut headers = HeaderMap::new();
        apply_page(&mut headers);
        apply_data(&mut headers);
        assert_eq!(
            headers["content-security-policy"],
            "sandbox; default-src 'none'"
        );
        assert_eq!(headers["referrer-policy"], "no-referrer");
    }
}
