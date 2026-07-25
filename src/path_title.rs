//! Pure path shortening and markup for the window title.
//!
//! Ranger's trick: when the path outgrows the bar, ancestor names collapse to
//! their initial from the left, so `~/projects/software/dev/fm/src` becomes
//! `~/p/s/d/fm/src`. The directory the cursor is in is never abbreviated.

use std::path::Path;

/// Splits `path` into the segments the title shows, replacing a `home` prefix
/// with `~`.
pub fn segments(path: &Path, home: Option<&Path>) -> Vec<String> {
    if let Some(home) = home {
        if let Ok(rest) = path.strip_prefix(home) {
            let mut out = vec!["~".to_owned()];
            out.extend(rest.iter().map(|part| part.to_string_lossy().into_owned()));
            return out;
        }
    }

    path.iter()
        .map(|part| part.to_string_lossy().into_owned())
        .collect()
}

/// Shortens from the left until `fits` accepts the result. The last segment and
/// a leading `~` or `/` are never abbreviated; if nothing fits even fully
/// abbreviated, the caller's label ellipsises what is left.
pub fn shorten(segments: &[String], fits: impl Fn(&str) -> bool) -> String {
    let mut shortened = segments.to_vec();
    let mut candidate = join(&shortened);
    if fits(&candidate) {
        return candidate;
    }

    let last = shortened.len().saturating_sub(1);
    for index in 0..last {
        if index == 0 && (shortened[0] == "/" || shortened[0] == "~") {
            continue;
        }

        shortened[index] = initial(&shortened[index]);
        candidate = join(&shortened);
        if fits(&candidate) {
            return candidate;
        }
    }

    candidate
}

/// First character of a directory name, keeping the leading dot of hidden
/// directories: `.config` becomes `.c`.
pub fn initial(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        None => String::new(),
        Some('.') => match chars.next() {
            Some(second) => format!(".{second}"),
            None => ".".to_owned(),
        },
        Some(first) => first.to_string(),
    }
}

/// Pango markup for `path`: ancestors dimmed, the current directory emphasised.
pub fn markup(path: &str) -> String {
    match path.rsplit_once('/') {
        None => format!("<b>{}</b>", escape(path)),
        Some((ancestors, current)) => format!(
            "<span alpha=\"55%\">{}/</span><b>{}</b>",
            escape(ancestors),
            escape(current)
        ),
    }
}

/// Joins segments back into a path without doubling the leading separator.
fn join(segments: &[String]) -> String {
    match segments.split_first() {
        None => String::new(),
        Some((first, rest)) if first == "/" => format!("/{}", rest.join("/")),
        Some((first, rest)) if rest.is_empty() => first.clone(),
        Some((first, rest)) => format!("{}/{}", first, rest.join("/")),
    }
}

/// Escapes the three characters Pango markup treats as syntax.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn owned(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    /// Stands in for Pango: accepts anything up to `limit` characters.
    fn width_limit(limit: usize) -> impl Fn(&str) -> bool {
        move |candidate: &str| candidate.chars().count() <= limit
    }

    #[test]
    fn replaces_the_home_prefix_with_a_tilde() {
        let segments = segments(
            Path::new("/home/milo/projects/fm"),
            Some(Path::new("/home/milo")),
        );
        assert_eq!(segments, owned(&["~", "projects", "fm"]));
    }

    #[test]
    fn keeps_the_root_separator_as_its_own_segment() {
        let segments = segments(Path::new("/etc/systemd"), Some(Path::new("/home/milo")));
        assert_eq!(segments, owned(&["/", "etc", "systemd"]));
    }

    #[test]
    fn leaves_a_path_that_already_fits_alone() {
        let segments = owned(&["~", "projects", "fm"]);
        assert_eq!(shorten(&segments, width_limit(80)), "~/projects/fm");
    }

    #[test]
    fn abbreviates_from_the_left_one_segment_at_a_time() {
        let segments = owned(&["~", "projects", "software", "dev", "fm", "src"]);
        assert_eq!(
            shorten(&segments, width_limit(24)),
            "~/p/software/dev/fm/src"
        );
        assert_eq!(shorten(&segments, width_limit(18)), "~/p/s/dev/fm/src");
        assert_eq!(shorten(&segments, width_limit(14)), "~/p/s/d/fm/src");
    }

    #[test]
    fn never_abbreviates_the_current_directory() {
        let segments = owned(&["~", "projects", "software", "dev", "fm", "src"]);
        assert!(shorten(&segments, width_limit(1)).ends_with("/src"));
    }

    #[test]
    fn never_abbreviates_the_leading_root() {
        let segments = owned(&["/", "etc", "systemd", "user"]);
        assert_eq!(shorten(&segments, width_limit(1)), "/e/s/user");
    }

    #[test]
    fn keeps_the_dot_of_hidden_directories() {
        assert_eq!(initial(".config"), ".c");
        assert_eq!(initial("projects"), "p");
        assert_eq!(initial("."), ".");
        assert_eq!(initial(""), "");
    }

    #[test]
    fn dims_ancestors_and_emphasises_the_current_directory() {
        assert_eq!(
            markup("~/p/src"),
            "<span alpha=\"55%\">~/p/</span><b>src</b>"
        );
    }

    #[test]
    fn a_bare_root_has_no_ancestors_to_dim() {
        assert_eq!(markup("~"), "<b>~</b>");
    }

    #[test]
    fn escapes_pango_markup_syntax() {
        assert_eq!(
            markup("~/a&b/<c>"),
            "<span alpha=\"55%\">~/a&amp;b/</span><b>&lt;c&gt;</b>"
        );
    }
}
