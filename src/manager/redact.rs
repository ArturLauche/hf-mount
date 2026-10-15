//! Remove secrets from text before it is logged, shown, exported or sent.
//!
//! Covers Hugging Face tokens (`hf_…`), JWTs (Xet/CAS access tokens),
//! `Authorization`/`Bearer` values and credential-bearing URL query
//! parameters (presigned S3/CloudFront URLs and `token=`-style parameters).

const SENSITIVE_QUERY_KEYS: &[&str] = &[
    "x-amz-signature",
    "x-amz-credential",
    "x-amz-security-token",
    "signature",
    "policy",
    "key-pair-id",
    "token",
    "access_token",
    "sig",
    "auth",
    "expires",
];

const MASK: &str = "***";

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Redact `hf_` tokens (≥ 8 token characters after the prefix).
fn redact_hf_tokens(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find("hf_") {
        let (before, after) = rest.split_at(pos);
        // Only at a token boundary (not inside e.g. "shf_x").
        let boundary = before.chars().next_back().is_none_or(|c| !is_token_char(c));
        let body_len = after[3..]
            .chars()
            .take_while(|c| is_token_char(*c))
            .map(char::len_utf8)
            .sum::<usize>();
        out.push_str(before);
        if boundary && body_len >= 8 {
            out.push_str("hf_");
            out.push_str(MASK);
            rest = &after[3 + body_len..];
        } else {
            out.push_str("hf_");
            rest = &after[3..];
        }
    }
    out.push_str(rest);
    out
}

/// Redact JSON Web Tokens (`eyJ….….…`).
fn redact_jwts(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find("eyJ") {
        let (before, after) = rest.split_at(pos);
        // At least "eyJ" itself, so the loop always advances.
        let len = after
            .chars()
            .take_while(|c| is_token_char(*c) || *c == '.')
            .map(char::len_utf8)
            .sum::<usize>();
        let candidate = &after[..len];
        out.push_str(before);
        if candidate.matches('.').count() >= 2 && len >= 20 {
            out.push_str("eyJ");
            out.push_str(MASK);
        } else {
            out.push_str(candidate);
        }
        rest = &after[len..];
    }
    out.push_str(rest);
    out
}

/// Header names whose whole value is a credential.
const AUTH_HEADERS: [&str; 2] = ["proxy-authorization", "authorization"];
/// Authorization schemes followed by a credential (`Bearer <token>`).
const AUTH_SCHEMES: [&str; 2] = ["bearer", "basic"];

/// Byte offset where the value of an `Authorization:` / `authorization=`
/// marker starting at `i` begins (after separators and an opening quote).
fn header_value_start(lower: &str, i: usize) -> Option<usize> {
    let name = AUTH_HEADERS.iter().find(|h| lower[i..].starts_with(*h))?;
    let rest = &lower[i + name.len()..];
    let after_ws = rest.trim_start_matches([' ', '\t']);
    let after_sep = after_ws.strip_prefix([':', '='])?;
    let value = after_sep.trim_start_matches([' ', '\t', '"', '\'']);
    Some(lower.len() - value.len())
}

/// Byte offset where the credential after `Bearer`/`Basic` and at least one
/// space or tab begins.
fn scheme_value_start(lower: &str, i: usize) -> Option<usize> {
    let scheme = AUTH_SCHEMES.iter().find(|s| lower[i..].starts_with(*s))?;
    let rest = &lower[i + scheme.len()..];
    let value = rest.trim_start_matches([' ', '\t']);
    (value.len() < rest.len()).then(|| lower.len() - value.len())
}

/// Whether the word after `Bearer`/`Basic` is a credential rather than prose
/// ("no bearer token stored"): long enough, and with a digit, a symbol used
/// in tokens or base64, or capitals after the first letter.
fn looks_like_credential(value: &str) -> bool {
    value.len() >= 8
        && (value.chars().any(|c| c.is_ascii_digit() || "+/=_-.~".contains(c))
            || value.chars().skip(1).any(|c| c.is_ascii_uppercase()))
}

/// Redact `Authorization` header values (to the end of the line or the
/// closing quote, whatever the scheme) and credentials after `Bearer` /
/// `Basic`, with any spacing and in any letter case.
fn redact_auth_values(input: &str) -> String {
    // ASCII lowercasing keeps byte offsets identical to `input`.
    let lower = input.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut last = 0usize;
    let mut prev: Option<char> = None;
    let mut iter = input.char_indices();
    while let Some((i, c)) = iter.next() {
        let at_word_start = prev.is_none_or(|p| !p.is_alphanumeric() && p != '_' && p != '-');
        prev = Some(c);
        if i < last || !at_word_start {
            continue;
        }
        let (start, end) = if let Some(start) = header_value_start(&lower, i) {
            let len = input[start..]
                .find(['\n', '\r', '"', '\''])
                .unwrap_or(input.len() - start);
            (start, start + len)
        } else if let Some(start) = scheme_value_start(&lower, i) {
            let len = input[start..]
                .find(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | ',' | ';' | ')'))
                .unwrap_or(input.len() - start);
            if !looks_like_credential(&input[start..start + len]) {
                continue;
            }
            (start, start + len)
        } else {
            continue;
        };
        if end > start {
            out.push_str(&input[last..start]);
            out.push_str(MASK);
            last = end;
            // Skip the masked value.
            while iter.clone().next().is_some_and(|(j, _)| j < end) {
                prev = iter.next().map(|(_, ch)| ch);
            }
        }
    }
    out.push_str(&input[last..]);
    out
}

/// Redact values of credential-bearing query parameters.
fn redact_query_params(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.char_indices().peekable();
    let bytes = input;
    let mut last = 0usize;
    while let Some((i, c)) = chars.next() {
        if c != '?' && c != '&' {
            continue;
        }
        let key_start = i + 1;
        let key_end = bytes[key_start..]
            .find(|ch: char| ch == '=' || ch == '&' || ch.is_whitespace())
            .map(|n| key_start + n)
            .unwrap_or(bytes.len());
        if !bytes[key_end..].starts_with('=') {
            continue;
        }
        let key = bytes[key_start..key_end].to_ascii_lowercase();
        if !SENSITIVE_QUERY_KEYS.contains(&key.as_str()) {
            continue;
        }
        let value_start = key_end + 1;
        let value_end = bytes[value_start..]
            .find(|ch: char| ch == '&' || ch.is_whitespace() || ch == '"' || ch == '\'')
            .map(|n| value_start + n)
            .unwrap_or(bytes.len());
        out.push_str(&bytes[last..value_start]);
        out.push_str(MASK);
        last = value_end;
        while chars.peek().is_some_and(|(j, _)| *j < value_end) {
            chars.next();
        }
    }
    out.push_str(&bytes[last..]);
    out
}

/// Remove secrets from `input`.
pub fn redact(input: &str) -> String {
    let s = redact_auth_values(input);
    let s = redact_hf_tokens(&s);
    let s = redact_jwts(&s);
    redact_query_params(&s)
}

/// Redact a token for display: keeps a short, non-secret hint.
pub fn token_hint(token: &str) -> String {
    let trimmed = token.trim();
    if trimmed.len() <= 8 {
        return MASK.to_string();
    }
    let tail: String = trimmed
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("hf_…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_hf_tokens() {
        let s = redact("token=hf_AbCdEfGhIjKlMnOpQrStUv and HF_TOKEN=hf_1234567890abcdef!");
        assert!(!s.contains("AbCdEf"), "{s}");
        assert!(!s.contains("1234567890abcdef"), "{s}");
        assert!(s.contains("hf_***"));
        // Short or embedded strings are left alone.
        assert_eq!(redact("hf_mount crate"), "hf_mount crate");
        assert_eq!(redact("shf_abcdefghijk"), "shf_abcdefghijk");
    }

    #[test]
    fn redacts_bearer_and_authorization() {
        let s = redact(r#"Authorization: Bearer abc.def.ghi-secret, "authorization=xyz""#);
        assert!(!s.contains("secret"), "{s}");
        assert!(!s.contains("xyz"), "{s}");
        // Any spacing, any case, other schemes, header values to end of line.
        for (input, secret) in [
            ("bearer\tsecret-value-1", "secret-value-1"),
            ("BEARER   secret-value-2 next", "secret-value-2"),
            ("Authorization:Basic dXNlcjpwYXNzd29yZA==\nnext line", "dXNlcjpwYXNz"),
            ("authorization = Token some-opaque-token", "some-opaque-token"),
            ("Proxy-Authorization: Negotiate YIIabc123", "YIIabc123"),
            ("basic c2VjcmV0OnZhbHVl;", "c2VjcmV0"),
        ] {
            let s = redact(input);
            assert!(!s.contains(secret), "{input:?} -> {s:?}");
        }
        assert_eq!(redact("Authorization: x\nnext line"), "Authorization: ***\nnext line");
        // Ordinary words are left alone.
        assert_eq!(redact("basically fine"), "basically fine");
        assert_eq!(redact("no-bearer here"), "no-bearer here");
        assert_eq!(redact("no bearer token stored"), "no bearer token stored");
        assert_eq!(
            redact("Basic authentication is enough"),
            "Basic authentication is enough"
        );
        // Base64 without digits or symbols is still caught by its capitals.
        assert_eq!(redact("Basic YWxhZGRpbjpvcGVuc2VzYW1l"), "Basic ***");
    }

    #[test]
    fn redacts_jwts() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let s = redact(&format!("cas token {jwt} expired"));
        assert!(!s.contains("dozjgNryP4J3"), "{s}");
        assert!(s.contains("cas token eyJ***"), "{s}");
        assert_eq!(redact("eyJ alone"), "eyJ alone");
    }

    #[test]
    fn redacts_presigned_urls() {
        let url = "https://cas-bridge.xethub.hf.co/xorb/abc?X-Amz-Credential=AKIA%2F20260101&X-Amz-Signature=deadbeef&x-id=GetObject";
        let s = redact(&format!("GET {url} failed"));
        assert!(!s.contains("AKIA"), "{s}");
        assert!(!s.contains("deadbeef"), "{s}");
        assert!(s.contains("x-id=GetObject"), "{s}");
        let s = redact("https://cdn.example/f?Policy=pol&Signature=sig&Key-Pair-Id=kp");
        assert_eq!(s, "https://cdn.example/f?Policy=***&Signature=***&Key-Pair-Id=***");
    }

    #[test]
    fn plain_text_is_unchanged() {
        let s = "Mounted model openai-community/gpt2@main at H: (read-only)";
        assert_eq!(redact(s), s);
        assert_eq!(redact("über ünïcode — ok? yes & no"), "über ünïcode — ok? yes & no");
    }

    #[test]
    fn token_hints_hide_the_secret() {
        assert_eq!(token_hint("hf_abcdefghijklmnopWXYZ"), "hf_…WXYZ");
        assert_eq!(token_hint("short"), "***");
    }
}
