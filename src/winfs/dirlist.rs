//! Directory enumeration order and WinFsp marker handling.
//!
//! WinFsp enumerates a directory in pages. Each page after the first carries
//! a *marker*: the name of the last entry already returned, and the file
//! system must continue with the entries strictly after it in its own,
//! stable order. We present `.` and `..` first (non-root directories only),
//! then the remaining names sorted by Unicode code point.

/// A sorted listing of one directory, captured when an enumeration starts.
#[derive(Debug, Clone)]
pub struct DirSnapshot<T> {
    entries: Vec<(String, T)>,
}

/// What a page starting after `marker` contains.
#[derive(Debug)]
pub struct Page<'a, T> {
    pub dot: bool,
    pub dot_dot: bool,
    pub entries: &'a [(String, T)],
}

impl<T> DirSnapshot<T> {
    /// Build a snapshot. Duplicate names keep their first occurrence.
    pub fn new(mut entries: Vec<(String, T)>) -> Self {
        entries.retain(|(name, _)| name != "." && name != "..");
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries.dedup_by(|a, b| a.0 == b.0);
        Self { entries }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(n, _)| n.as_str())
    }

    pub fn get(&self, name: &str) -> Option<&T> {
        self.entries
            .binary_search_by(|(n, _)| n.as_str().cmp(name))
            .ok()
            .map(|i| &self.entries[i].1)
    }

    /// Entries to return after `marker`. `with_dots` is false for the root
    /// directory, which has no `.`/`..` entries on Windows.
    pub fn page_after(&self, marker: Option<&str>, with_dots: bool) -> Page<'_, T> {
        match marker {
            None => Page {
                dot: with_dots,
                dot_dot: with_dots,
                entries: &self.entries,
            },
            Some(".") => Page {
                dot: false,
                dot_dot: with_dots,
                entries: &self.entries,
            },
            Some("..") => Page {
                dot: false,
                dot_dot: false,
                entries: &self.entries,
            },
            Some(marker) => {
                let start = self.entries.partition_point(|(n, _)| n.as_str() <= marker);
                Page {
                    dot: false,
                    dot_dot: false,
                    entries: &self.entries[start..],
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(names: &[&str]) -> DirSnapshot<()> {
        DirSnapshot::new(names.iter().map(|n| (n.to_string(), ())).collect())
    }

    fn page_names<T>(page: &Page<'_, T>) -> Vec<String> {
        let mut out = Vec::new();
        if page.dot {
            out.push(".".to_string());
        }
        if page.dot_dot {
            out.push("..".to_string());
        }
        out.extend(page.entries.iter().map(|(n, _)| n.clone()));
        out
    }

    #[test]
    fn sorted_with_dots_first() {
        let s = snap(&["b", "!bang", "A", "a", ".hidden", "..", "."]);
        assert_eq!(
            page_names(&s.page_after(None, true)),
            vec![".", "..", "!bang", ".hidden", "A", "a", "b"]
        );
        assert_eq!(
            page_names(&s.page_after(None, false)),
            vec!["!bang", ".hidden", "A", "a", "b"]
        );
    }

    #[test]
    fn markers_continue_strictly_after() {
        let s = snap(&["a", "b", "c"]);
        assert_eq!(page_names(&s.page_after(Some("."), true)), vec!["..", "a", "b", "c"]);
        assert_eq!(page_names(&s.page_after(Some(".."), true)), vec!["a", "b", "c"]);
        assert_eq!(page_names(&s.page_after(Some("a"), true)), vec!["b", "c"]);
        assert_eq!(page_names(&s.page_after(Some("c"), true)), Vec::<String>::new());
        // A marker that was deleted in between still resumes at the right spot.
        assert_eq!(page_names(&s.page_after(Some("aa"), true)), vec!["b", "c"]);
    }

    #[test]
    fn paging_visits_every_entry_once() {
        let names: Vec<String> = (0..1000).map(|i| format!("file-{i:04}")).collect();
        let s = DirSnapshot::new(names.iter().map(|n| (n.clone(), ())).collect());
        let mut seen = Vec::new();
        let mut marker: Option<String> = None;
        loop {
            let page = s.page_after(marker.as_deref(), true);
            let mut batch = page_names(&page);
            batch.truncate(7); // simulate a small buffer
            if batch.is_empty() {
                break;
            }
            marker = batch.last().cloned();
            seen.extend(batch);
        }
        assert_eq!(seen.len(), 1002);
        assert_eq!(&seen[..2], &[".".to_string(), "..".to_string()]);
        assert_eq!(&seen[2..], names.as_slice());
    }

    #[test]
    fn lookup_and_dedup() {
        let s = DirSnapshot::new(vec![("x".to_string(), 1), ("y".to_string(), 2), ("x".to_string(), 3)]);
        assert_eq!(s.len(), 2);
        assert_eq!(s.get("y"), Some(&2));
        assert_eq!(s.get("z"), None);
    }
}
