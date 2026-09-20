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
    path == parent || (parent == "/" && path.starts_with('/')) || path.starts_with(&(parent + "/"))
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
}
