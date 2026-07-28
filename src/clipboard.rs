//! The freedesktop file-clipboard wire format, and collision-free naming.
//!
//! Nautilus, Thunar, PCManFM and Caja all speak `x-special/gnome-copied-files`:
//! an operation word on the first line, then one URI per line. Everything here
//! is plain string handling, so it is testable without a display.

/// What a clipboard payload asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardOp {
    Copy,
    Cut,
}

/// The mime type that carries the operation word alongside the URIs.
pub const GNOME_MIME: &str = "x-special/gnome-copied-files";

/// The mime type nearly everything else understands. It has no operation word,
/// so a paste sourced from it is always treated as a copy.
pub const URI_LIST_MIME: &str = "text/uri-list";

/// Builds an `x-special/gnome-copied-files` payload.
pub fn encode(op: ClipboardOp, uris: &[String]) -> String {
    let word = match op {
        ClipboardOp::Copy => "copy",
        ClipboardOp::Cut => "cut",
    };

    std::iter::once(word.to_owned())
        .chain(uris.iter().cloned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parses an `x-special/gnome-copied-files` payload.
///
/// Returns `None` when the operation word is missing or unknown, or when no URI
/// follows it \u{2014} which is also how plain text on the clipboard gets rejected
/// instead of being mistaken for a file list.
pub fn decode(payload: &str) -> Option<(ClipboardOp, Vec<String>)> {
    let mut lines = payload
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty());

    let op = match lines.next()? {
        "copy" => ClipboardOp::Copy,
        "cut" => ClipboardOp::Cut,
        _ => return None,
    };

    let uris: Vec<String> = lines.map(str::to_owned).collect();
    (!uris.is_empty()).then_some((op, uris))
}

/// Builds a `text/uri-list` payload: CRLF separated with a trailing CRLF, per
/// RFC 2483.
pub fn encode_uri_list(uris: &[String]) -> String {
    uris.iter().map(|uri| format!("{uri}\r\n")).collect()
}

/// Parses a `text/uri-list`. Comment lines starting with `#` are part of the
/// format and are dropped.
pub fn decode_uri_list(payload: &str) -> Vec<String> {
    payload
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// Returns a name `taken` rejects, derived from `name`.
///
/// `notas.txt` becomes `notas (copy).txt`, then `notas (copy 2).txt`. The
/// suffix goes before the final extension so the file keeps its type. Nothing
/// in this feature ever overwrites; this is how that promise is kept.
pub fn free_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_owned();
    }

    let (stem, extension) = split_extension(name);
    let mut attempt = 1usize;

    loop {
        let suffix = if attempt == 1 {
            "(copy)".to_owned()
        } else {
            format!("(copy {attempt})")
        };

        let candidate = if extension.is_empty() {
            format!("{stem} {suffix}")
        } else {
            format!("{stem} {suffix}.{extension}")
        };

        if !taken(&candidate) {
            return candidate;
        }

        attempt += 1;
    }
}

/// Splits a file name into stem and extension. A leading dot belongs to the
/// stem, so `.bashrc` has no extension, and only the final suffix counts, so
/// `archive.tar.gz` splits into `archive.tar` and `gz`.
fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(0) | None => (name, ""),
        Some(index) => (&name[..index], &name[index + 1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uris(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn encodes_a_copy_the_way_nautilus_reads_it() {
        let payload = encode(
            ClipboardOp::Copy,
            &uris(&["file:///a.txt", "file:///b.txt"]),
        );
        assert_eq!(payload, "copy\nfile:///a.txt\nfile:///b.txt");
    }

    #[test]
    fn the_operation_word_is_the_only_difference_for_a_cut() {
        let payload = encode(ClipboardOp::Cut, &uris(&["file:///a.txt"]));
        assert_eq!(payload, "cut\nfile:///a.txt");
    }

    #[test]
    fn decodes_what_it_encodes() {
        let files = uris(&["file:///a.txt", "file:///b.txt"]);
        for op in [ClipboardOp::Copy, ClipboardOp::Cut] {
            let round_trip = decode(&encode(op, &files));
            assert_eq!(round_trip, Some((op, files.clone())));
        }
    }

    #[test]
    fn tolerates_the_trailing_newline_other_apps_add() {
        assert_eq!(
            decode("copy\nfile:///a.txt\n"),
            Some((ClipboardOp::Copy, uris(&["file:///a.txt"])))
        );
    }

    #[test]
    fn rejects_a_payload_that_is_not_a_file_clipboard() {
        // Plain text on the clipboard must never be mistaken for files.
        assert_eq!(decode("just some copied text"), None);
        assert_eq!(decode(""), None);
        // An operation word with nothing to operate on is not actionable.
        assert_eq!(decode("copy"), None);
    }

    #[test]
    fn a_uri_list_is_crlf_terminated() {
        assert_eq!(
            encode_uri_list(&uris(&["file:///a.txt", "file:///b.txt"])),
            "file:///a.txt\r\nfile:///b.txt\r\n"
        );
    }

    #[test]
    fn a_uri_list_drops_its_comment_lines() {
        // Comments starting with '#' are part of RFC 2483, and some apps send them.
        let parsed = decode_uri_list("# a comment\r\nfile:///a.txt\r\n\r\nfile:///b.txt\r\n");
        assert_eq!(parsed, uris(&["file:///a.txt", "file:///b.txt"]));
    }

    #[test]
    fn a_free_name_is_left_alone() {
        assert_eq!(free_name("notas.txt", |_| false), "notas.txt");
    }

    #[test]
    fn a_taken_name_gets_the_copy_suffix_before_its_extension() {
        assert_eq!(
            free_name("notas.txt", |n| n == "notas.txt"),
            "notas (copy).txt"
        );
    }

    #[test]
    fn the_suffix_counts_up_while_names_stay_taken() {
        let taken = |n: &str| matches!(n, "notas.txt" | "notas (copy).txt");
        assert_eq!(free_name("notas.txt", taken), "notas (copy 2).txt");
    }

    #[test]
    fn a_name_without_an_extension_keeps_the_suffix_at_the_end() {
        assert_eq!(free_name("README", |n| n == "README"), "README (copy)");
    }

    #[test]
    fn a_dotfiles_leading_dot_is_not_an_extension() {
        assert_eq!(free_name(".bashrc", |n| n == ".bashrc"), ".bashrc (copy)");
    }

    #[test]
    fn only_the_final_suffix_counts_as_the_extension() {
        assert_eq!(
            free_name("archive.tar.gz", |n| n == "archive.tar.gz"),
            "archive.tar (copy).gz"
        );
    }
}
