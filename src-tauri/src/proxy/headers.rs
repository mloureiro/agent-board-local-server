use http::header::{HeaderMap, HeaderName, HeaderValue};

/// Sent on every response, copied from the pinned Caddyfile's site-level `header` block.
pub const PAGE: &[(&str, &str)] = &[
    (
        "content-security-policy",
        "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; worker-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'",
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
    use std::collections::BTreeSet;

    type Pairs = BTreeSet<(String, String)>;

    fn pinned_caddyfile() -> &'static str {
        include_str!("../../../core.Caddyfile")
    }

    fn pair(line: &str) -> Option<(String, String)> {
        let line = line.trim().trim_start_matches('>');
        let (name, rest) = line.split_once(' ')?;
        let value = rest.trim().strip_prefix('"')?.strip_suffix('"')?;
        Some((name.to_ascii_lowercase(), value.to_string()))
    }

    fn block_after(caddyfile: &str, marker: &str) -> Pairs {
        let mut lines = caddyfile
            .lines()
            .skip_while(|line| line.trim() != marker)
            .skip_while(|line| line.trim() != "header {")
            .skip(1);
        let mut found = Pairs::new();
        for line in lines.by_ref().take_while(|line| line.trim() != "}") {
            found.insert(pair(line).unwrap_or_else(|| panic!("unreadable header line: {line}")));
        }
        found
    }

    fn cache_controls(caddyfile: &str) -> BTreeSet<String> {
        caddyfile
            .lines()
            .filter_map(|line| line.trim().strip_prefix("header Cache-Control "))
            .map(|value| value.trim_matches('"').to_string())
            .collect()
    }

    fn table(entries: &[(&str, &str)]) -> Pairs {
        entries
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    fn drift(caddyfile: &str) -> Vec<&'static str> {
        let mut differing = Vec::new();
        if block_after(caddyfile, ":8080 {") != table(PAGE) {
            differing.push("page headers");
        }
        if block_after(caddyfile, "(data_headers) {") != table(DATA) {
            differing.push("data headers");
        }
        if cache_controls(caddyfile) != BTreeSet::from([ASSET_CACHE.into(), PAGE_CACHE.into()]) {
            differing.push("cache control");
        }
        differing
    }

    #[test]
    fn the_tables_equal_the_pinned_caddyfile_and_the_data_set_wins_on_overlap() {
        assert_eq!(drift(pinned_caddyfile()), Vec::<&str>::new());

        let mut headers = HeaderMap::new();
        apply_page(&mut headers);
        apply_data(&mut headers);
        assert_eq!(
            headers["content-security-policy"],
            "sandbox; default-src 'none'"
        );
        assert_eq!(headers["referrer-policy"], "no-referrer");
    }

    #[test]
    fn a_changed_added_or_dropped_caddyfile_header_is_drift() {
        let pinned = pinned_caddyfile();
        let downgraded = pinned.replace(
            "Cross-Origin-Opener-Policy \"same-origin\"",
            "Cross-Origin-Opener-Policy \"unsafe-none\"",
        );
        let added = pinned.replace(
            "\t\t>Cache-Control \"no-store\"\n",
            "\t\t>Cache-Control \"no-store\"\n\t\t>Permissions-Policy \"camera=()\"\n",
        );
        let dropped = pinned.replace("\t\tReferrer-Policy \"no-referrer\"\n", "");
        let recached = pinned.replace("\"no-cache\"", "\"max-age=60\"");

        assert_eq!(drift(&downgraded), ["page headers"]);
        assert_eq!(drift(&added), ["data headers"]);
        assert_eq!(drift(&dropped), ["page headers"]);
        assert_eq!(drift(&recached), ["cache control"]);
    }
}
