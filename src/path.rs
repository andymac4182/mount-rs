/// Normalize an absolute POSIX-style path and clamp `..` at the virtual root.
pub fn normalize_path(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            value => segments.push(value),
        }
    }
    if segments.is_empty() {
        "/".to_owned()
    } else {
        format!("/{}", segments.join("/"))
    }
}

pub fn split_path(path: &str) -> Vec<String> {
    normalize_path(path)
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

pub fn join_path(parts: &[&str]) -> String {
    normalize_path(&parts.join("/"))
}

pub fn dirname(path: &str) -> String {
    let normalized = normalize_path(path);
    match normalized.rfind('/') {
        Some(0) | None => "/".to_owned(),
        Some(index) => normalized[..index].to_owned(),
    }
}

pub fn basename(path: &str) -> String {
    let normalized = normalize_path(path);
    normalized
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("/")
        .to_owned()
}

pub fn is_path_inside(path: &str, parent: &str) -> bool {
    let path = normalize_path(path);
    let parent = normalize_path(parent);
    has_component_prefix(&path, &parent)
}

fn has_component_prefix(path: &str, parent: &str) -> bool {
    path == parent
        || (parent == "/" && path.starts_with('/'))
        || (path.starts_with(parent) && path.as_bytes().get(parent.len()) == Some(&b'/'))
}

#[cfg(kani)]
mod verification {
    use super::*;

    #[kani::proof]
    #[kani::unwind(8)]
    fn namespace_path_boundaries() {
        let suffix: u8 = kani::any();
        kani::assume(suffix < 4);
        let candidate = ["/a", "/a/x", "/ab", "/ab/x"][suffix as usize];
        let inside = has_component_prefix(candidate, "/a");
        kani::cover!(inside);
        kani::cover!(!inside);
        assert_eq!(inside, suffix < 2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_clamps() {
        assert_eq!(normalize_path("a//b/../c/"), "/a/c");
        assert_eq!(normalize_path("../../"), "/");
        assert_eq!(dirname("/a/b"), "/a");
        assert_eq!(basename("/a/b"), "b");
    }

    #[test]
    fn canonical_paths_are_closed_and_idempotent() {
        for input in ["", "/", "a//..", "//a/./b//../c", "../../a", "/a/../a"] {
            let canonical = normalize_path(input);
            assert!(canonical.starts_with('/'), "{input}");
            assert_eq!(normalize_path(&canonical), canonical, "{input}");
            assert!(canonical == "/" || !canonical.ends_with('/'), "{input}");
            assert!(
                split_path(&canonical)
                    .iter()
                    .all(|part| !part.is_empty() && part != "." && part != "..")
            );
            let parts = split_path(&canonical);
            let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
            assert_eq!(join_path(&refs), canonical, "{input}");
        }
    }

    #[test]
    fn containment_respects_name_boundaries_after_normalization() {
        assert!(is_path_inside("/a", "/a"));
        assert!(is_path_inside("/a/b", "/a"));
        assert!(is_path_inside("/a/./b", "/a"));
        assert!(!is_path_inside("/ab", "/a"));
        assert!(!is_path_inside("/a/../ab", "/a"));
    }
}
