//! `SystemTime` ⇄ Windows FILETIME (100 ns intervals since 1601-01-01 UTC).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// FILETIME of the Unix epoch.
pub const UNIX_EPOCH_AS_FILETIME: u64 = 116_444_736_000_000_000;

pub fn to_filetime(t: SystemTime) -> u64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(after) => {
            let ticks = after.as_nanos() / 100;
            UNIX_EPOCH_AS_FILETIME.saturating_add(u64::try_from(ticks).unwrap_or(u64::MAX))
        }
        Err(before) => {
            let ticks = before.duration().as_nanos() / 100;
            UNIX_EPOCH_AS_FILETIME.saturating_sub(u64::try_from(ticks).unwrap_or(u64::MAX))
        }
    }
}

pub fn from_filetime(ft: u64) -> SystemTime {
    if ft >= UNIX_EPOCH_AS_FILETIME {
        UNIX_EPOCH + Duration::from_nanos((ft - UNIX_EPOCH_AS_FILETIME).saturating_mul(100))
    } else {
        UNIX_EPOCH - Duration::from_nanos((UNIX_EPOCH_AS_FILETIME - ft).saturating_mul(100))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_and_roundtrip() {
        assert_eq!(to_filetime(UNIX_EPOCH), UNIX_EPOCH_AS_FILETIME);
        let t = UNIX_EPOCH + Duration::from_secs(1_700_000_000) + Duration::from_nanos(123_400);
        assert_eq!(from_filetime(to_filetime(t)), t);
        // 2023-11-14T22:13:20Z
        assert_eq!(
            to_filetime(UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
            133_444_736_000_000_000
        );
        let before = UNIX_EPOCH - Duration::from_secs(86_400);
        assert_eq!(from_filetime(to_filetime(before)), before);
    }

    #[test]
    fn sub_100ns_precision_truncates() {
        let t = UNIX_EPOCH + Duration::from_nanos(150);
        assert_eq!(to_filetime(t), UNIX_EPOCH_AS_FILETIME + 1);
    }
}
