use std::fmt::Write;

const UNIT: &str = "bytes=";
const BLANK: [char; 4] = [' ', '\t', '\r', '\n'];
const MAX_ITEMS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u64,
    pub length: u64,
}

impl Span {
    pub fn content_range(self, size: u64) -> String {
        format!(
            "bytes {}-{}/{size}",
            self.start,
            self.start + self.length - 1
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Selection {
    Whole,
    Spans(Vec<Span>),
    /// Not a list of byte ranges; Go's file server (and so Caddy) answers it with 416, not with the whole file.
    Invalid,
    NoOverlap,
}

/// Follows Go's `net/http` `parseRange`, which the pinned Caddy serves files with: an unparsable header is an
/// error, a start past the end of the file only counts when no other range survives, a list whose ranges add up
/// to more than the file is answered with the whole file, and so is a list of more than 200 comma-separated items,
/// counted before any of them is read. Numbers are signed 64-bit there, so one above `i64::MAX` is unparsable.
pub fn select(header: &str, size: u64) -> Selection {
    if header.is_empty() {
        return Selection::Whole;
    }
    let Some(list) = header.strip_prefix(UNIT) else {
        return Selection::Invalid;
    };
    if list.split(',').count() > MAX_ITEMS {
        return Selection::Whole;
    }
    let mut spans = Vec::new();
    let mut beyond_the_end = false;
    for item in list.split(',') {
        let item = item.trim_matches(BLANK);
        if item.is_empty() {
            continue;
        }
        let Some((first, last)) = item.split_once('-') else {
            return Selection::Invalid;
        };
        let (first, last) = (first.trim_matches(BLANK), last.trim_matches(BLANK));
        if first.is_empty() {
            let Some(tail) = last
                .parse::<i64>()
                .ok()
                .filter(|_| !last.starts_with('-'))
                .map(i64::unsigned_abs)
            else {
                return Selection::Invalid;
            };
            if size == 0 {
                beyond_the_end = true;
                continue;
            }
            let tail = tail.min(size);
            spans.push(Span {
                start: size - tail,
                length: tail,
            });
            continue;
        }
        let Ok(start) = first.parse::<i64>().map(i64::unsigned_abs) else {
            return Selection::Invalid;
        };
        if start >= size {
            beyond_the_end = true;
            continue;
        }
        let length = if last.is_empty() {
            size - start
        } else {
            let Ok(end) = last.parse::<i64>() else {
                return Selection::Invalid;
            };
            if i64::try_from(start).is_ok_and(|start| start > end) {
                return Selection::Invalid;
            }
            end.unsigned_abs().min(size - 1) - start + 1
        };
        spans.push(Span { start, length });
    }
    let total = spans
        .iter()
        .fold(0u64, |sum, span| sum.saturating_add(span.length));
    match spans.len() {
        0 if beyond_the_end => Selection::NoOverlap,
        0 => Selection::Whole,
        _ if total > size => Selection::Whole,
        _ => Selection::Spans(spans),
    }
}

/// The body of a `multipart/byteranges` answer, laid out the way Go's `mime/multipart` writer does.
pub fn multipart(
    boundary: &str,
    spans: &[Span],
    size: u64,
    content_type: &str,
    content: &[u8],
) -> Vec<u8> {
    let mut body = Vec::new();
    for (index, span) in spans.iter().enumerate() {
        let lead = if index == 0 { "" } else { "\r\n" };
        let mut head = String::new();
        let _ = write!(
            head,
            "{lead}--{boundary}\r\nContent-Range: {}\r\nContent-Type: {content_type}\r\n\r\n",
            span.content_range(size)
        );
        body.extend_from_slice(head.as_bytes());
        body.extend_from_slice(slice(content, *span));
    }
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

pub fn slice(content: &[u8], span: Span) -> &[u8] {
    let start = span.start as usize;
    &content[start..start + span.length as usize]
}

pub fn boundary() -> String {
    rand::random::<[u8; 30]>()
        .iter()
        .fold(String::with_capacity(60), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(pairs: &[(u64, u64)]) -> Selection {
        Selection::Spans(
            pairs
                .iter()
                .map(|&(start, length)| Span { start, length })
                .collect(),
        )
    }

    #[test]
    fn a_range_header_selects_the_same_bytes_as_go_does() {
        let cases: &[(&str, u64, Selection)] = &[
            ("", 10, Selection::Whole),
            ("bytes=0-4", 10, spans(&[(0, 5)])),
            ("bytes=3-", 10, spans(&[(3, 7)])),
            ("bytes=-4", 10, spans(&[(6, 4)])),
            ("bytes=-99", 10, spans(&[(0, 10)])),
            ("bytes=-0", 10, spans(&[(10, 0)])),
            ("bytes=2-99", 10, spans(&[(2, 8)])),
            ("bytes=9-9", 10, spans(&[(9, 1)])),
            ("bytes= 1 - 2 ", 10, spans(&[(1, 2)])),
            ("bytes=+1-2", 10, spans(&[(1, 2)])),
            ("bytes=0-1,4-5", 10, spans(&[(0, 2), (4, 2)])),
            ("bytes=0-1,,4-5", 10, spans(&[(0, 2), (4, 2)])),
            ("bytes=0-1,20-30", 10, spans(&[(0, 2)])),
            ("bytes=0-6,4-9", 10, Selection::Whole),
            ("bytes=", 10, Selection::Whole),
            ("bytes=,", 10, Selection::Whole),
            ("bytes=10-", 10, Selection::NoOverlap),
            ("bytes=20-5", 10, Selection::NoOverlap),
            ("bytes=20-x", 10, Selection::NoOverlap),
            ("bytes=20-,30-", 10, Selection::NoOverlap),
            ("bytes=0-0", 0, Selection::NoOverlap),
            ("bytes=-5", 0, Selection::NoOverlap),
            ("bytes=5-2", 10, Selection::Invalid),
            ("bytes=a-b", 10, Selection::Invalid),
            ("bytes=-", 10, Selection::Invalid),
            ("bytes=--5", 10, Selection::Invalid),
            ("bytes=5", 10, Selection::Invalid),
            ("bytes=0-1,x", 10, Selection::Invalid),
            ("items=0-1", 10, Selection::Invalid),
            ("Bytes=0-1", 10, Selection::Invalid),
            ("bytes=99999999999999999999-", 10, Selection::Invalid),
            ("bytes=9223372036854775807-", 10, Selection::NoOverlap),
            ("bytes=9223372036854775808-", 10, Selection::Invalid),
            ("bytes=9223372036854775808-5", 10, Selection::Invalid),
            ("bytes=0-9223372036854775807", 10, spans(&[(0, 10)])),
            ("bytes=0-9223372036854775808", 10, Selection::Invalid),
            ("bytes=-9223372036854775807", 10, spans(&[(0, 10)])),
            ("bytes=-9223372036854775808", 10, Selection::Invalid),
            ("bytes=0--0", 10, spans(&[(0, 1)])),
            ("bytes=5--0", 10, Selection::Invalid),
            ("bytes=--0", 10, Selection::Invalid),
        ];
        for (header, size, expected) in cases {
            assert_eq!(&select(header, *size), expected, "{header:?} on {size}");
        }
    }

    #[test]
    fn more_than_200_items_select_the_whole_file_before_any_is_read() {
        let list =
            |count: usize, tail: &str| format!("bytes={}{tail}", vec!["0-0"; count].join(","));
        assert!(
            matches!(select(&list(200, ""), 1000), Selection::Spans(found) if found.len() == 200)
        );
        assert_eq!(select(&list(201, ""), 1000), Selection::Whole);
        assert_eq!(select(&list(200, ","), 1000), Selection::Whole);
        assert_eq!(select(&list(201, ",x"), 1000), Selection::Whole);
        assert_eq!(
            select(&format!("bytes={}", vec!["5000-"; 250].join(",")), 1000),
            Selection::Whole
        );
    }

    #[test]
    fn a_span_names_its_first_and_last_byte_and_the_file_length() {
        let span = Span {
            start: 4,
            length: 3,
        };
        assert_eq!(span.content_range(10), "bytes 4-6/10");
    }

    #[test]
    fn a_multipart_body_has_a_part_per_span_and_a_closing_boundary() {
        let body = multipart(
            "B",
            &[
                Span {
                    start: 0,
                    length: 2,
                },
                Span {
                    start: 4,
                    length: 2,
                },
            ],
            8,
            "text/plain",
            b"abcdefgh",
        );
        assert_eq!(
            String::from_utf8(body).unwrap(),
            "--B\r\nContent-Range: bytes 0-1/8\r\nContent-Type: text/plain\r\n\r\nab\
             \r\n--B\r\nContent-Range: bytes 4-5/8\r\nContent-Type: text/plain\r\n\r\nef\
             \r\n--B--\r\n"
        );
    }

    #[test]
    fn a_boundary_is_sixty_hex_digits_and_differs_each_time() {
        let (one, other) = (boundary(), boundary());
        assert_eq!(one.len(), 60);
        assert!(one.chars().all(|digit| digit.is_ascii_hexdigit()));
        assert_ne!(one, other);
    }
}
