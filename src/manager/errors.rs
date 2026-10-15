//! Turn engine failures into the user-facing [`ErrorInfo`] categories.

use super::protocol::{ErrorCode, ErrorInfo};
use super::spec::MountSpec;
use crate::setup::{SetupError, SetupErrorKind};

const TOKENS_URL: &str = "https://huggingface.co/settings/tokens";

/// Classify a failed `try_build_with_runtime`.
///
/// The Hub answers 401 both for private/gated repositories accessed
/// anonymously and for repositories that do not exist, so an anonymous 401 is
/// reported as "not found or private" with a sign-in hint, while a 401 with a
/// token means the token was rejected.
pub fn classify_setup_error(err: &SetupError, spec: &MountSpec, authenticated: bool) -> ErrorInfo {
    let source = spec.display_source();
    match err.kind {
        SetupErrorKind::InvalidSource | SetupErrorKind::InvalidOptions => {
            ErrorInfo::new(ErrorCode::InvalidArgument, err.message.clone())
        }
        SetupErrorKind::LocalIo => ErrorInfo::new(ErrorCode::Internal, err.message.clone())
            .with_hint("check that the cache directory is writable and has free space"),
        SetupErrorKind::Storage => ErrorInfo::new(ErrorCode::Internal, err.message.clone()),
        SetupErrorKind::Hub => match err.hub_status {
            Some(401) if authenticated => ErrorInfo::new(
                ErrorCode::InvalidToken,
                format!("the Hugging Face token was rejected for {source}"),
            )
            .with_hint(format!("check the token or create a new one at {TOKENS_URL}")),
            Some(401) => ErrorInfo::new(
                ErrorCode::AuthRequired,
                format!("{source} was not found, or it is private or gated"),
            )
            .with_hint("sign in with a token that has access (hf-mount auth login), or check the name"),
            Some(403) if spec.source_type.is_repo() => {
                ErrorInfo::new(ErrorCode::Forbidden, format!("access to {source} is forbidden"))
                    .with_hint("for gated repositories, accept the terms on huggingface.co with the same account")
            }
            Some(403) => ErrorInfo::new(
                ErrorCode::Forbidden,
                format!("the token does not have the permission needed for {source}"),
            )
            .with_hint(if spec.read_only {
                "the token needs read access to this bucket".to_string()
            } else {
                format!(
                    "read-write bucket mounts need a token with write access ({TOKENS_URL}); \
                     or mount it read-only"
                )
            }),
            Some(404) => ErrorInfo::new(ErrorCode::NotFound, format!("{source} was not found"))
                .with_hint("check the namespace, name, type and revision"),
            Some(429) => ErrorInfo::new(
                ErrorCode::RateLimited,
                format!("the Hub is rate limiting requests for {source}"),
            )
            .with_hint("wait a few minutes and retry"),
            Some(status) if (500..600).contains(&status) => ErrorInfo::new(
                ErrorCode::Network,
                format!("the Hub returned a server error ({status}) for {source}"),
            )
            .with_hint("the Hub may be degraded; retry later"),
            _ if err.transient => {
                ErrorInfo::new(ErrorCode::Network, format!("could not reach the Hub: {}", err.message))
                    .with_hint("check your internet connection, proxy or firewall")
            }
            _ => ErrorInfo::new(ErrorCode::Internal, err.message.clone()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub(status: Option<u16>, transient: bool) -> SetupError {
        SetupError {
            kind: SetupErrorKind::Hub,
            message: "Failed to initialize Hub client: Hub API error".into(),
            hub_status: status,
            transient,
        }
    }

    #[test]
    fn classifies_hub_failures() {
        let repo = MountSpec::parse("repo", "user/private-model", "H:").unwrap();
        let bucket = MountSpec::parse("bucket", "user/b", "X:").unwrap();
        assert_eq!(
            classify_setup_error(&hub(Some(401), false), &repo, false).code,
            ErrorCode::AuthRequired
        );
        assert_eq!(
            classify_setup_error(&hub(Some(401), false), &repo, true).code,
            ErrorCode::InvalidToken
        );
        assert_eq!(
            classify_setup_error(&hub(Some(404), false), &repo, true).code,
            ErrorCode::NotFound
        );
        let forbidden = classify_setup_error(&hub(Some(403), false), &bucket, true);
        assert_eq!(forbidden.code, ErrorCode::Forbidden);
        assert!(forbidden.hint.unwrap().contains("write access"));
        assert_eq!(
            classify_setup_error(&hub(Some(429), true), &repo, false).code,
            ErrorCode::RateLimited
        );
        assert_eq!(
            classify_setup_error(&hub(Some(503), true), &repo, false).code,
            ErrorCode::Network
        );
        assert_eq!(
            classify_setup_error(&hub(None, true), &repo, false).code,
            ErrorCode::Network
        );
        assert_eq!(
            classify_setup_error(&hub(None, false), &repo, false).code,
            ErrorCode::Internal
        );
    }

    #[test]
    fn classifies_local_failures() {
        let repo = MountSpec::parse("repo", "user/m", "H:").unwrap();
        let e = SetupError {
            kind: SetupErrorKind::InvalidSource,
            message: "invalid repo path".into(),
            hub_status: None,
            transient: false,
        };
        assert_eq!(classify_setup_error(&e, &repo, false).code, ErrorCode::InvalidArgument);
    }
}
