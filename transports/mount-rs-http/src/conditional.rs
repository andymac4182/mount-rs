//! Entity tags and conditional requests for files (RFC 9110, section 13).

use hyper::HeaderMap;
use hyper::header::{self, HeaderName};
use mount_rs_core::Stats;

/// What a conditional request is evaluated against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Current {
    /// Nothing is at the path.
    Missing,
    /// Something is at the path that has no entity tag: a directory.
    Untagged,
    /// A file, with its entity tag, quotes included.
    Tagged(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Proceed,
    /// Answer a GET or HEAD with 304 Not Modified.
    NotModified,
    /// Answer with 412 Precondition Failed.
    Failed,
}

/// The entity tag of a file, quotes included: the driver's content tag
/// where it has one, so the tag changes exactly when the bytes do, or else
/// one made from the metadata that changes when they are written.
pub(crate) fn entity_tag(stats: &Stats, content: Option<String>) -> String {
    match content {
        Some(tag) if usable(&tag) => format!("\"{tag}\""),
        _ => format!(
            "\"m{:x}-{:x}-{:x}-{:x}-{:x}\"",
            stats.dev, stats.ino, stats.size, stats.mtime_ms, stats.ctime_ms
        ),
    }
}

/// A tag a driver gave that fits in a quoted entity tag unchanged.
fn usable(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 128
        && tag
            .bytes()
            .all(|byte| (0x21..=0x7e).contains(&byte) && byte != b'"')
}

/// Whether a request has preconditions this module evaluates.
pub(crate) fn is_conditional(headers: &HeaderMap) -> bool {
    headers.contains_key(header::IF_MATCH) || headers.contains_key(header::IF_NONE_MATCH)
}

/// Evaluate `If-Match`, then `If-None-Match`. `safe` is a GET or HEAD,
/// which a matching `If-None-Match` answers with 304 rather than 412.
pub(crate) fn evaluate(headers: &HeaderMap, current: &Current, safe: bool) -> Outcome {
    if let Some(value) = joined(headers, &header::IF_MATCH)
        && !matches(&value, current, true)
    {
        return Outcome::Failed;
    }
    if let Some(value) = joined(headers, &header::IF_NONE_MATCH)
        && matches(&value, current, false)
    {
        return if safe {
            Outcome::NotModified
        } else {
            Outcome::Failed
        };
    }
    Outcome::Proceed
}

/// Whether a `Range` should be honoured: without `If-Range`, or when
/// `If-Range` names the current tag strongly. A date, which these
/// responses give nothing to compare with, or a weak tag means the whole
/// file is sent.
pub(crate) fn range_applies(headers: &HeaderMap, etag: &str) -> bool {
    let Some(value) = headers.get(header::IF_RANGE) else {
        return true;
    };
    value
        .to_str()
        .is_ok_and(|value| value.trim().starts_with('"') && value.trim() == etag)
}

/// Every instance of a list header, joined as one list.
fn joined(headers: &HeaderMap, name: &HeaderName) -> Option<String> {
    let mut values = headers.get_all(name).iter().peekable();
    values.peek()?;
    Some(
        values
            .map(|value| value.to_str().unwrap_or(""))
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// Whether a list of entity tags, or `*`, matches. Strong comparison
/// ignores weak tags; weak comparison compares the quoted values only.
fn matches(value: &str, current: &Current, strong: bool) -> bool {
    if value.trim() == "*" {
        return *current != Current::Missing;
    }
    let Current::Tagged(etag) = current else {
        return false;
    };
    entity_tags(value).any(|(weak, tag)| (!strong || !weak) && tag == etag)
}

/// The entity tags in a list, each with whether it is weak and its quoted
/// value. An entry that is not an entity tag is skipped.
fn entity_tags(value: &str) -> impl Iterator<Item = (bool, &str)> {
    value.split(',').filter_map(|entry| {
        let entry = entry.trim();
        let (weak, quoted) = match entry.strip_prefix("W/") {
            Some(rest) => (true, rest),
            None => (false, entry),
        };
        (quoted.len() >= 2
            && quoted.starts_with('"')
            && quoted.ends_with('"')
            && !quoted[1..quoted.len() - 1].contains('"'))
        .then_some((weak, quoted))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::header::HeaderValue;

    fn headers(pairs: &[(HeaderName, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(name.clone(), HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    fn tagged(tag: &str) -> Current {
        Current::Tagged(tag.to_owned())
    }

    #[test]
    fn if_match_needs_a_strong_match_or_an_existing_target_for_a_star() {
        let current = tagged("\"a\"");
        for (value, outcome) in [
            ("\"a\"", Outcome::Proceed),
            ("\"b\", \"a\"", Outcome::Proceed),
            ("W/\"a\"", Outcome::Failed),
            ("\"b\"", Outcome::Failed),
            ("*", Outcome::Proceed),
            ("a", Outcome::Failed),
        ] {
            let headers = headers(&[(header::IF_MATCH, value)]);
            assert_eq!(evaluate(&headers, &current, false), outcome, "{value}");
        }
        let star = headers(&[(header::IF_MATCH, "*")]);
        assert_eq!(evaluate(&star, &Current::Missing, false), Outcome::Failed);
        assert_eq!(evaluate(&star, &Current::Untagged, false), Outcome::Proceed);
        let tag = headers(&[(header::IF_MATCH, "\"a\"")]);
        assert_eq!(evaluate(&tag, &Current::Untagged, false), Outcome::Failed);
    }

    #[test]
    fn if_none_match_compares_weakly_and_answers_reads_with_not_modified() {
        let current = tagged("\"a\"");
        let weak = headers(&[(header::IF_NONE_MATCH, "W/\"a\"")]);
        assert_eq!(evaluate(&weak, &current, true), Outcome::NotModified);
        assert_eq!(evaluate(&weak, &current, false), Outcome::Failed);
        let other = headers(&[(header::IF_NONE_MATCH, "\"b\"")]);
        assert_eq!(evaluate(&other, &current, true), Outcome::Proceed);
        let star = headers(&[(header::IF_NONE_MATCH, "*")]);
        assert_eq!(evaluate(&star, &Current::Missing, false), Outcome::Proceed);
        assert_eq!(evaluate(&star, &current, false), Outcome::Failed);
    }

    #[test]
    fn repeated_headers_form_one_list_and_if_match_is_evaluated_first() {
        let current = tagged("\"a\"");
        let repeated = headers(&[(header::IF_MATCH, "\"b\""), (header::IF_MATCH, "\"a\"")]);
        assert_eq!(evaluate(&repeated, &current, false), Outcome::Proceed);
        let both = headers(&[
            (header::IF_MATCH, "\"b\""),
            (header::IF_NONE_MATCH, "\"a\""),
        ]);
        assert_eq!(evaluate(&both, &current, true), Outcome::Failed);
    }

    #[test]
    fn if_range_honours_the_range_only_for_the_current_strong_tag() {
        assert!(range_applies(&HeaderMap::new(), "\"a\""));
        assert!(range_applies(
            &headers(&[(header::IF_RANGE, "\"a\"")]),
            "\"a\""
        ));
        assert!(!range_applies(
            &headers(&[(header::IF_RANGE, "\"b\"")]),
            "\"a\""
        ));
        assert!(!range_applies(
            &headers(&[(header::IF_RANGE, "W/\"a\"")]),
            "\"a\""
        ));
        assert!(!range_applies(
            &headers(&[(header::IF_RANGE, "Wed, 21 Oct 2015 07:28:00 GMT")]),
            "\"a\""
        ));
    }

    #[test]
    fn a_driver_tag_that_cannot_be_quoted_falls_back_to_metadata() {
        let stats = Stats {
            dev: 1,
            ino: 2,
            mode: 0o100644,
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            size: 3,
            blksize: 4096,
            blocks: 1,
            atime_ms: 0,
            mtime_ms: 5,
            ctime_ms: 6,
            birthtime_ms: 0,
        };
        assert_eq!(entity_tag(&stats, Some("abc".into())), "\"abc\"");
        let fallback = "\"m1-2-3-5-6\"";
        assert_eq!(entity_tag(&stats, None), fallback);
        assert_eq!(entity_tag(&stats, Some("a\"b".into())), fallback);
        assert_eq!(entity_tag(&stats, Some(String::new())), fallback);
        assert_eq!(entity_tag(&stats, Some("a b".into())), fallback);
    }
}
