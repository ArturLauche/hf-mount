use std::borrow::Cow;

use nom_rfc8288::complete::{LinkData, link_lenient};
use reqwest::{Response, Url, header::LINK};

use crate::error::{Error, Result};

/// Validate every link before returning a continuation. Polling uses a
/// successful end of listing as evidence that missing children were deleted.
pub(super) fn next_page(response: &Response) -> Result<Option<Url>> {
    let mut next = None;
    for header in response.headers().get_all(LINK) {
        let header = header
            .to_str()
            .map_err(|_| Error::hub("invalid tree listing Link header"))?;
        let header = normalize_parameter_whitespace(header);
        let links = link_lenient(&header).map_err(|_| Error::hub("invalid tree listing Link header"))?;
        for link in links.into_iter().flatten() {
            next = next.or(continuation(response.url(), &link)?);
        }
    }
    Ok(next)
}

fn continuation(current: &Url, link: &LinkData<'_>) -> Result<Option<Url>> {
    // RFC 8288 §3.3 requires rel, and only its first occurrence counts.
    let param = |name| link.params.iter().find(|p| p.key.eq_ignore_ascii_case(name));
    let relations = param("rel")
        .and_then(|p| p.val.as_deref())
        .filter(|value| !value.trim_ascii().is_empty())
        .ok_or_else(|| Error::hub("tree listing Link header missing or empty rel"))?;
    if !relations
        .split_ascii_whitespace()
        .any(|r| r.eq_ignore_ascii_case("next"))
    {
        return Ok(None);
    }

    // The anchor changes the resource to which the relation applies (§3.2).
    if let Some(anchor) = param("anchor") {
        let value = anchor
            .val
            .as_deref()
            .ok_or_else(|| Error::hub("tree listing Link header missing anchor value"))?;
        let context = current
            .join(value)
            .map_err(|e| Error::hub(format!("invalid tree listing anchor: {e}")))?;
        if context != *current {
            return Ok(None);
        }
    }
    let mut next = current
        .join(link.url)
        .map_err(|e| Error::hub(format!("invalid tree listing continuation: {e}")))?;
    if !matches!(next.scheme(), "http" | "https") || !next.username().is_empty() || next.password().is_some() {
        return Err(Error::hub("invalid tree listing continuation URL"));
    }
    // Fragments never reach the server and cannot identify a new page.
    next.set_fragment(None);
    Ok(Some(next))
}

/// nom-rfc8288 accepts whitespace after a parameter's '=' but rejects the
/// SP/HTAB before it allowed by §3. Copy only when removing that whitespace.
fn normalize_parameter_whitespace(header: &str) -> Cow<'_, str> {
    let mut normalized = String::new();
    let mut start = 0;
    let mut chars = header.char_indices();
    while let Some((i, ch)) = chars.next() {
        match ch {
            '<' => {
                chars.by_ref().find(|&(_, c)| c == '>');
            }
            '"' => {
                while let Some((_, c)) = chars.next() {
                    match c {
                        '\\' => {
                            chars.next();
                        }
                        '"' => break,
                        _ => {}
                    }
                }
            }
            '=' => {
                let before = &header[start..i];
                let trimmed = before.trim_end_matches([' ', '\t']);
                if trimmed.len() != before.len() {
                    normalized.push_str(trimmed);
                    start = i;
                }
            }
            _ => {}
        }
    }
    if start == 0 {
        Cow::Borrowed(header)
    } else {
        normalized.push_str(&header[start..]);
        Cow::Owned(normalized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_inside_targets_and_escaped_quoted_values_is_preserved() {
        let header = r#"<?cursor=a = b>; title="a \"; rel = next"; rel=next"#;
        assert!(matches!(normalize_parameter_whitespace(header), Cow::Borrowed(_)));
        assert_eq!(
            normalize_parameter_whitespace(&header.replace("; rel=next", "; rel \t=next")),
            header
        );
    }
}
