//! Windows file-name rules.
//!
//! Remote names come from the Hub and follow POSIX rules (anything but `/`
//! and NUL). Some of them cannot be represented on a Windows volume without
//! changing which file they resolve to, so they are hidden from listings
//! rather than shown under a name that would open something else. Names that
//! Windows callers send us are validated before they reach `VirtualFs`.

/// NTFS/WinFsp component limit, in UTF-16 code units.
pub const MAX_COMPONENT_UTF16: usize = 255;

/// Why a name cannot be used as-is on a Windows volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameProblem {
    Empty,
    /// `.` or `..`.
    DotOrDotDot,
    /// One of `< > : " / \ | ? *` or a control character.
    InvalidChar(char),
    /// Win32 strips trailing dots and spaces from paths, so `foo.` would
    /// silently resolve to `foo`.
    TrailingDotOrSpace,
    /// `CON`, `NUL`, `COM1`… — Win32 maps these to devices.
    ReservedDeviceName,
    TooLong,
}

impl std::fmt::Display for NameProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NameProblem::Empty => f.write_str("empty name"),
            NameProblem::DotOrDotDot => f.write_str("'.' and '..' are not file names"),
            NameProblem::InvalidChar(c) => write!(f, "character {c:?} is not allowed in Windows file names"),
            NameProblem::TrailingDotOrSpace => f.write_str("Windows file names cannot end with a dot or a space"),
            NameProblem::ReservedDeviceName => f.write_str("reserved Windows device name"),
            NameProblem::TooLong => write!(f, "longer than {MAX_COMPONENT_UTF16} UTF-16 code units"),
        }
    }
}

const RESERVED_DEVICE_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "COM¹", "COM²", "COM³", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8",
    "LPT9", "LPT¹", "LPT²", "LPT³",
];

fn is_invalid_char(c: char) -> bool {
    matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (c as u32) < 0x20
}

/// Whether `name` is a reserved device name. Only the bare name counts:
/// Windows 11 resolves `aux.py` or `con.txt` as ordinary files, so those stay
/// visible (on Windows 10 they may be unreachable; see the README).
pub fn is_reserved_device_name(name: &str) -> bool {
    RESERVED_DEVICE_NAMES.iter().any(|r| r.eq_ignore_ascii_case(name))
}

/// Check a single path component against the Windows naming rules.
pub fn windows_name_problem(name: &str) -> Option<NameProblem> {
    if name.is_empty() {
        return Some(NameProblem::Empty);
    }
    if name == "." || name == ".." {
        return Some(NameProblem::DotOrDotDot);
    }
    if let Some(c) = name.chars().find(|&c| is_invalid_char(c)) {
        return Some(NameProblem::InvalidChar(c));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Some(NameProblem::TrailingDotOrSpace);
    }
    if is_reserved_device_name(name) {
        return Some(NameProblem::ReservedDeviceName);
    }
    if name.encode_utf16().count() > MAX_COMPONENT_UTF16 {
        return Some(NameProblem::TooLong);
    }
    None
}

/// Whether a remote name is safe to show in a Windows directory listing.
pub fn is_representable(name: &str) -> bool {
    windows_name_problem(name).is_none()
}

/// Error splitting a WinFsp path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    /// The path is not absolute (`\…`) or contains an empty component.
    Malformed,
    /// A component is not a valid name (`.`/`..`, stream syntax, …).
    InvalidComponent(NameProblem),
}

/// Split a WinFsp volume-relative path (`\`, `\a\b`) into its components.
///
/// WinFsp hands us paths already normalised by the I/O manager; anything
/// that would address the parent of a component, a named stream
/// (`file:stream`) or an empty component is rejected so nothing can escape
/// the mounted root or alias another file.
pub fn split_path(path: &str) -> Result<Vec<&str>, PathError> {
    let rest = path.strip_prefix('\\').ok_or(PathError::Malformed)?;
    if rest.is_empty() {
        return Ok(Vec::new());
    }
    // A single trailing backslash is a directory marker, not a component.
    let rest = rest.strip_suffix('\\').unwrap_or(rest);
    let mut components = Vec::new();
    for component in rest.split('\\') {
        if component.is_empty() {
            return Err(PathError::Malformed);
        }
        if component == "." || component == ".." {
            return Err(PathError::InvalidComponent(NameProblem::DotOrDotDot));
        }
        if let Some(c) = component.chars().find(|&c| c == ':' || c == '/' || (c as u32) < 0x20) {
            return Err(PathError::InvalidComponent(NameProblem::InvalidChar(c)));
        }
        components.push(component);
    }
    Ok(components)
}

/// Decode a UTF-16 name, ending at the first NUL if there is one. Unpaired
/// surrogates (legal in NTFS names, not in Hub paths) are rejected rather
/// than replaced, so two distinct Windows names never map to one remote name.
pub fn utf16_to_string(units: &[u16]) -> Option<String> {
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16(&units[..end]).ok()
}

/// Upcase one character the way the Windows upcase table does: a 1:1
/// mapping only. Characters whose full uppercase is longer (`ß` → `SS`,
/// `ﬁ` → `FI`) are left unchanged, as NTFS leaves them.
fn upcase(c: char) -> char {
    let mut upper = c.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(u), None) => u,
        _ => c,
    }
}

/// Case-insensitive comparison approximating the Windows upcase table.
pub fn eq_ignore_case(a: &str, b: &str) -> bool {
    a.chars().map(upcase).eq(b.chars().map(upcase))
}

/// The single candidate equal to `wanted` ignoring case, if there is
/// exactly one. Ambiguous matches (e.g. `README.md` and `readme.md` both
/// present) return `None`, so a case-insensitive lookup can never pick one
/// of two distinct remote files arbitrarily.
pub fn unique_case_insensitive_match<'a>(
    wanted: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    let mut found = None;
    for candidate in candidates {
        if eq_ignore_case(wanted, candidate) {
            if found.is_some() {
                return None;
            }
            found = Some(candidate);
        }
    }
    found
}

/// Names File Explorer and the shell probe for in every folder they show.
/// They are answered from the parent listing only, never with a remote
/// HEAD/list "miss probe" (which would cost a Hub round trip per folder view).
pub fn is_shell_probe_name(name: &str) -> bool {
    const PROBES: &[&str] = &[
        "desktop.ini",
        "autorun.inf",
        "thumbs.db",
        "ehthumbs.db",
        "ehthumbs_vista.db",
        "folder.jpg",
        "folder.gif",
        "folder.png",
        "$recycle.bin",
        "recycler",
        "system volume information",
        "$extend",
        "$mft",
        "pagefile.sys",
        "hiberfil.sys",
        "swapfile.sys",
        "dumpstack.log.tmp",
        "indexervolumeguid",
        "wpsettings.dat",
    ];
    let lower = name.to_ascii_lowercase();
    PROBES.contains(&lower.as_str()) || (lower.starts_with("albumart") && lower.ends_with(".jpg"))
}

#[cfg(test)]
mod tests {

    #[test]
    fn case_folding_is_one_to_one_like_ntfs() {
        assert!(eq_ignore_case("Config.JSON", "config.json"));
        assert!(eq_ignore_case("ÄRGER", "ärger"));
        // Full case mapping would make these equal; NTFS keeps them distinct.
        assert!(!eq_ignore_case("STRASSE", "straße"));
        assert!(!eq_ignore_case("FILE", "ﬁle"));
    }

    #[test]
    fn utf16_decoding_stops_at_nul() {
        let mut units: Vec<u16> = "name".encode_utf16().collect();
        units.extend([0, b'x' as u16]);
        assert_eq!(utf16_to_string(&units).as_deref(), Some("name"));
        assert_eq!(utf16_to_string(&[0xD800]), None);
    }
    use super::*;

    #[test]
    fn accepts_ordinary_names() {
        for ok in [
            "config.json",
            "model.safetensors",
            "日本語.txt",
            "emoji 😀.md",
            ".gitattributes",
            "a b",
            "aux.py",
            "con.txt",
        ] {
            assert_eq!(windows_name_problem(ok), None, "{ok:?}");
        }
    }

    #[test]
    fn rejects_unrepresentable_names() {
        let cases = [
            ("", NameProblem::Empty),
            (".", NameProblem::DotOrDotDot),
            ("..", NameProblem::DotOrDotDot),
            ("a:b", NameProblem::InvalidChar(':')),
            ("a\\b", NameProblem::InvalidChar('\\')),
            ("what?", NameProblem::InvalidChar('?')),
            ("star*", NameProblem::InvalidChar('*')),
            ("pipe|", NameProblem::InvalidChar('|')),
            ("quote\"", NameProblem::InvalidChar('"')),
            ("<tag>", NameProblem::InvalidChar('<')),
            ("tab\there", NameProblem::InvalidChar('\t')),
            ("trailing.", NameProblem::TrailingDotOrSpace),
            ("trailing ", NameProblem::TrailingDotOrSpace),
            ("CON", NameProblem::ReservedDeviceName),
            ("nul", NameProblem::ReservedDeviceName),
            ("Com1", NameProblem::ReservedDeviceName),
            ("LPT9", NameProblem::ReservedDeviceName),
            ("CONOUT$", NameProblem::ReservedDeviceName),
        ];
        for (name, problem) in cases {
            assert_eq!(windows_name_problem(name), Some(problem), "{name:?}");
        }
        assert_eq!(windows_name_problem(&"x".repeat(256)), Some(NameProblem::TooLong));
        assert_eq!(windows_name_problem(&"x".repeat(255)), None);
        // 255 UTF-16 units, but more than 255 bytes: still fine.
        assert_eq!(windows_name_problem(&"é".repeat(255)), None);
        // Astral chars count as two UTF-16 units.
        assert_eq!(windows_name_problem(&"😀".repeat(128)), Some(NameProblem::TooLong));
    }

    #[test]
    fn splits_winfsp_paths() {
        assert_eq!(split_path("\\").unwrap(), Vec::<&str>::new());
        assert_eq!(split_path("\\a").unwrap(), vec!["a"]);
        assert_eq!(split_path("\\a\\b c\\d.txt").unwrap(), vec!["a", "b c", "d.txt"]);
        assert_eq!(split_path("\\dir\\").unwrap(), vec!["dir"]);
        assert_eq!(split_path("\\données\\日本").unwrap(), vec!["données", "日本"]);
    }

    #[test]
    fn split_rejects_escapes_and_streams() {
        assert_eq!(split_path("a\\b"), Err(PathError::Malformed));
        assert_eq!(split_path(""), Err(PathError::Malformed));
        assert_eq!(split_path("\\a\\\\b"), Err(PathError::Malformed));
        assert_eq!(
            split_path("\\a\\..\\b"),
            Err(PathError::InvalidComponent(NameProblem::DotOrDotDot))
        );
        assert_eq!(
            split_path("\\.\\b"),
            Err(PathError::InvalidComponent(NameProblem::DotOrDotDot))
        );
        assert_eq!(
            split_path("\\file.txt:secret"),
            Err(PathError::InvalidComponent(NameProblem::InvalidChar(':')))
        );
        assert_eq!(
            split_path("\\a/b"),
            Err(PathError::InvalidComponent(NameProblem::InvalidChar('/')))
        );
    }

    #[test]
    fn utf16_roundtrip_and_surrogates() {
        let name = "naïve 😀 файл";
        let units: Vec<u16> = name.encode_utf16().collect();
        assert_eq!(utf16_to_string(&units).as_deref(), Some(name));
        // Unpaired high surrogate.
        assert_eq!(utf16_to_string(&[0x61, 0xD800, 0x62]), None);
    }

    #[test]
    fn case_insensitive_match_requires_uniqueness() {
        let names = ["README.md", "config.json", "Data"];
        assert_eq!(
            unique_case_insensitive_match("readme.MD", names.iter().copied()),
            Some("README.md")
        );
        assert_eq!(
            unique_case_insensitive_match("data", names.iter().copied()),
            Some("Data")
        );
        assert_eq!(unique_case_insensitive_match("missing", names.iter().copied()), None);
        let ambiguous = ["README.md", "readme.md"];
        assert_eq!(
            unique_case_insensitive_match("Readme.md", ambiguous.iter().copied()),
            None
        );
        assert!(eq_ignore_case("STRASSE", "strasse"));
        assert!(eq_ignore_case("ÉTÉ", "été"));
    }

    #[test]
    fn shell_probe_names() {
        for name in [
            "desktop.ini",
            "Desktop.ini",
            "AUTORUN.INF",
            "Thumbs.db",
            "AlbumArtSmall.jpg",
            "$RECYCLE.BIN",
        ] {
            assert!(is_shell_probe_name(name), "{name}");
        }
        for name in ["config.json", "desktop.ini.bak", "folder.jpeg"] {
            assert!(!is_shell_probe_name(name), "{name}");
        }
    }
}
