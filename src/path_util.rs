//! Safe path resolution for image downloads.
//! Mirrors upstream `utils/local-path.ts`: a user-supplied path is resolved
//! against the image dir; absolute paths are accepted only when they land
//! inside it; drive-letter paths reject loudly on POSIX.

use std::path::{Component, Path, PathBuf};

pub fn resolve_local_path(raw: &str, base: &Path) -> Result<PathBuf, String> {
    // Normalize separators: on POSIX accept either slash style (LLMs mix them).
    #[cfg(unix)]
    {
        if let Some(_drive) = drive_letter_prefix(raw) {
            return Err(format!(
                "path {raw:?} starts with a Windows drive letter, but this host is POSIX"
            ));
        }
    }
    let normalized = raw.replace('\\', "/");
    let base_abs = absolutize(base);
    let candidate = if Path::new(&normalized).is_absolute() {
        normalize(Path::new(&normalized))
    } else {
        normalize(&base_abs.join(&normalized))
    };
    if !is_within(&base_abs, &candidate) {
        let hint = if normalized.starts_with('/') {
            format!(
                " If you meant a directory inside the image directory, drop the leading slash (e.g. {:?}).",
                normalized.trim_start_matches('/')
            )
        } else {
            String::new()
        };
        return Err(format!(
            "path {raw:?} resolves outside the image directory.{hint}"
        ));
    }
    Ok(candidate)
}

#[cfg(unix)]
fn drive_letter_prefix(s: &str) -> Option<char> {
    let mut c = s.chars();
    let a = c.next()?;
    if a.is_ascii_alphabetic() && c.next() == Some(':') {
        Some(a)
    } else {
        None
    }
}

fn absolutize(p: &Path) -> PathBuf {
    if p.is_absolute() {
        normalize(p)
    } else {
        std::env::current_dir()
            .map(|cwd| normalize(&cwd.join(p)))
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

/// Lexical normalization (no I/O, no symlink resolution — same contract as
/// upstream: symlinks pointing outside are not detected).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    let is_abs = p.is_absolute();
    for comp in p.components() {
        match comp {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(c) => out.push(c),
        }
    }
    if out.as_os_str().is_empty() {
        return if is_abs {
            PathBuf::from("/")
        } else {
            PathBuf::from(".")
        };
    }
    out
}

fn is_within(base: &Path, candidate: &Path) -> bool {
    candidate.starts_with(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_resolves_inside() {
        let base = Path::new("/proj");
        assert_eq!(
            resolve_local_path("public/images", base).unwrap(),
            PathBuf::from("/proj/public/images")
        );
    }

    #[test]
    fn absolute_inside_accepted() {
        let base = Path::new("/proj");
        assert_eq!(
            resolve_local_path("/proj/a/b", base).unwrap(),
            PathBuf::from("/proj/a/b")
        );
    }

    #[test]
    fn outside_rejected() {
        let base = Path::new("/proj");
        assert!(resolve_local_path("../../etc", base).is_err());
        assert!(resolve_local_path("/etc/passwd", base).is_err());
    }

    #[test]
    fn leading_slash_hint_errors() {
        let base = Path::new("/proj");
        let err = resolve_local_path("/public/images", base).unwrap_err();
        assert!(err.contains("drop the leading slash"));
    }
}
