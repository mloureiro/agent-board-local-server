use http::header::{HeaderMap, HOST, ORIGIN};
use http::{Method, StatusCode};

const SEC_FETCH_SITE: &str = "sec-fetch-site";

/// Hop-by-hop and forwarding headers: a client must not decide what the backend is told about the connection.
const NOT_FORWARDED: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Host,
    Forbidden,
}

impl Refusal {
    pub fn status(self) -> StatusCode {
        match self {
            Self::Host => StatusCode::MISDIRECTED_REQUEST,
            Self::Forbidden => StatusCode::FORBIDDEN,
        }
    }
}

pub struct Guard {
    hosts: [String; 2],
    origins: [String; 2],
}

impl Guard {
    pub fn new(port: u16) -> Self {
        Self {
            hosts: [format!("127.0.0.1:{port}"), format!("localhost:{port}")],
            origins: [
                format!("http://127.0.0.1:{port}"),
                format!("http://localhost:{port}"),
            ],
        }
    }

    /// A browser always sends the real Host, an Origin on writes and Sec-Fetch-Site, so these checks stop
    /// web pages (DNS rebinding, cross-site writes). Another process on the machine can send any header.
    pub fn check(&self, method: &Method, headers: &HeaderMap) -> Result<(), Refusal> {
        let mut hosts = headers.get_all(HOST).iter();
        match (hosts.next(), hosts.next()) {
            (Some(host), None) if self.hosts.iter().any(|allowed| host == allowed.as_str()) => {}
            _ => return Err(Refusal::Host),
        }

        let safe_method = matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
        if !safe_method {
            let mut origins = headers.get_all(ORIGIN).iter();
            let allowed = match (origins.next(), origins.next()) {
                (None, _) => true,
                (Some(origin), None) => self.origins.iter().any(|ok| origin == ok.as_str()),
                _ => false,
            };
            if !allowed {
                return Err(Refusal::Forbidden);
            }
        }

        let same_site_only = headers
            .get_all(SEC_FETCH_SITE)
            .iter()
            .all(|site| site == "same-origin" || site == "none");
        if !same_site_only {
            return Err(Refusal::Forbidden);
        }
        Ok(())
    }

    /// Removes the hop-by-hop and forwarding headers a client sent; every other header passes.
    pub fn strip_forwarding(&self, headers: &mut HeaderMap) {
        for name in NOT_FORWARDED {
            headers.remove(*name);
        }
    }
}

#[cfg(test)]
mod tests {
    use http::header::{HeaderName, HeaderValue};

    use super::*;

    const PORT: u16 = 4785;

    type Case<'a> = (
        &'a str,
        Method,
        &'a [(&'a str, &'a str)],
        Result<(), Refusal>,
    );

    fn guard() -> Guard {
        Guard::new(PORT)
    }

    fn request(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        headers
    }

    #[test]
    fn requests_are_allowed_or_refused_by_host_origin_and_fetch_site() {
        let cases: &[Case] = &[
            (
                "loopback ip host",
                Method::GET,
                &[("host", "127.0.0.1:4785")],
                Ok(()),
            ),
            (
                "localhost host",
                Method::GET,
                &[("host", "localhost:4785")],
                Ok(()),
            ),
            (
                "other port",
                Method::GET,
                &[("host", "127.0.0.1:4786")],
                Err(Refusal::Host),
            ),
            (
                "no port",
                Method::GET,
                &[("host", "127.0.0.1")],
                Err(Refusal::Host),
            ),
            (
                "other host",
                Method::GET,
                &[("host", "evil.test:4785")],
                Err(Refusal::Host),
            ),
            (
                "rebinding name",
                Method::GET,
                &[("host", "127.0.0.1.evil.test:4785")],
                Err(Refusal::Host),
            ),
            ("no host", Method::GET, &[], Err(Refusal::Host)),
            (
                "two hosts",
                Method::GET,
                &[("host", "127.0.0.1:4785"), ("host", "evil.test")],
                Err(Refusal::Host),
            ),
            (
                "write from the ip origin",
                Method::POST,
                &[
                    ("host", "127.0.0.1:4785"),
                    ("origin", "http://127.0.0.1:4785"),
                ],
                Ok(()),
            ),
            (
                "write from the localhost origin",
                Method::POST,
                &[
                    ("host", "localhost:4785"),
                    ("origin", "http://localhost:4785"),
                ],
                Ok(()),
            ),
            (
                "write with no origin",
                Method::POST,
                &[("host", "127.0.0.1:4785")],
                Ok(()),
            ),
            (
                "write from a foreign origin",
                Method::POST,
                &[("host", "127.0.0.1:4785"), ("origin", "https://evil.test")],
                Err(Refusal::Forbidden),
            ),
            (
                "write from the null origin",
                Method::DELETE,
                &[("host", "127.0.0.1:4785"), ("origin", "null")],
                Err(Refusal::Forbidden),
            ),
            (
                "write from the same host on another port",
                Method::PUT,
                &[("host", "127.0.0.1:4785"), ("origin", "http://127.0.0.1:9")],
                Err(Refusal::Forbidden),
            ),
            (
                "write from https on this port",
                Method::POST,
                &[
                    ("host", "127.0.0.1:4785"),
                    ("origin", "https://127.0.0.1:4785"),
                ],
                Err(Refusal::Forbidden),
            ),
            (
                "write with two origins",
                Method::POST,
                &[
                    ("host", "127.0.0.1:4785"),
                    ("origin", "http://127.0.0.1:4785"),
                    ("origin", "https://evil.test"),
                ],
                Err(Refusal::Forbidden),
            ),
            (
                "read with a foreign origin",
                Method::GET,
                &[("host", "127.0.0.1:4785"), ("origin", "https://evil.test")],
                Ok(()),
            ),
            (
                "same-origin fetch",
                Method::GET,
                &[
                    ("host", "127.0.0.1:4785"),
                    ("sec-fetch-site", "same-origin"),
                ],
                Ok(()),
            ),
            (
                "typed address",
                Method::GET,
                &[("host", "127.0.0.1:4785"), ("sec-fetch-site", "none")],
                Ok(()),
            ),
            (
                "cross-site fetch",
                Method::GET,
                &[("host", "127.0.0.1:4785"), ("sec-fetch-site", "cross-site")],
                Err(Refusal::Forbidden),
            ),
            (
                "same-site fetch",
                Method::GET,
                &[("host", "127.0.0.1:4785"), ("sec-fetch-site", "same-site")],
                Err(Refusal::Forbidden),
            ),
            (
                "same-origin and cross-site together",
                Method::GET,
                &[
                    ("host", "127.0.0.1:4785"),
                    ("sec-fetch-site", "same-origin"),
                    ("sec-fetch-site", "cross-site"),
                ],
                Err(Refusal::Forbidden),
            ),
        ];
        for (label, method, headers, expected) in cases {
            assert_eq!(
                guard().check(method, &request(headers)),
                *expected,
                "{label}"
            );
        }
    }

    #[test]
    fn a_host_check_failure_wins_over_an_origin_failure() {
        let headers = request(&[("host", "evil.test"), ("origin", "https://evil.test")]);
        assert_eq!(guard().check(&Method::POST, &headers), Err(Refusal::Host));
    }

    #[test]
    fn forwarding_and_hop_by_hop_headers_are_removed_and_the_rest_pass() {
        let mut headers = request(&[
            ("host", "127.0.0.1:4785"),
            ("connection", "keep-alive"),
            ("x-forwarded-for", "1.2.3.4"),
            ("x-forwarded-host", "evil.test"),
            ("forwarded", "for=1.2.3.4"),
            ("transfer-encoding", "chunked"),
            ("authorization", "Bearer machine-token"),
            ("accept", "application/json"),
        ]);
        guard().strip_forwarding(&mut headers);

        for gone in [
            "connection",
            "x-forwarded-for",
            "x-forwarded-host",
            "forwarded",
            "transfer-encoding",
        ] {
            assert!(!headers.contains_key(gone), "{gone} survived");
        }
        assert_eq!(headers["authorization"], "Bearer machine-token");
        assert_eq!(headers["accept"], "application/json");
        assert_eq!(headers["host"], "127.0.0.1:4785");
    }
}
