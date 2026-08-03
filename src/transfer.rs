//! What the corner indicator says while files are moving.
//!
//! Pure: the set of running transfers in, a line of text and a bar fraction
//! out. No GTK, so the rules that are easy to get subtly wrong — an unknown
//! total, nothing running, several at once — are settled by tests rather than
//! by watching the window.

/// One transfer as the indicator sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Active {
    pub description: String,
    pub current: i64,
    pub total: i64,
}

/// What to show for the transfers currently running.
///
/// The outer `None` means nothing is running and the indicator hides — which is
/// not the same as a bar sitting at zero, and reads very differently.
///
/// The inner `None` fraction means *indeterminate*: work is happening and
/// nobody can say how far along it is. That is not a corner case. A copy within
/// one filesystem goes through `copy_file_range`, a single kernel call that
/// blocks for as long as it takes and reports nothing in between — measured
/// here as exactly **two** progress events for a 3 GB copy, against 384,001 for
/// the same file copied across filesystems. A bar frozen at 0% for fifteen
/// seconds lies as loudly as no bar at all; a pulsing one tells the truth.
pub fn summarize(active: &[Active]) -> Option<(String, Option<f64>)> {
    if active.is_empty() {
        return None;
    }

    let description = match active {
        [only] => only.description.clone(),
        many => format!("{} transfers", many.len()),
    };

    let current: i64 = active.iter().map(|transfer| transfer.current).sum();
    let total: i64 = active.iter().map(|transfer| transfer.total).sum();

    // No bytes reported yet, or no total to measure against: say "working"
    // rather than "0% done". Dividing by a zero total would also produce NaN,
    // which GTK paints as a bar stuck at nothing.
    let fraction = if total > 0 && current > 0 {
        Some((current as f64 / total as f64).clamp(0.0, 1.0))
    } else {
        None
    };

    Some((description, fraction))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transfer(description: &str, current: i64, total: i64) -> Active {
        Active {
            description: description.to_owned(),
            current,
            total,
        }
    }

    #[test]
    fn nothing_running_hides_the_indicator() {
        // Not "a bar at zero" — the caller must be able to tell the difference.
        assert_eq!(summarize(&[]), None);
    }

    #[test]
    fn a_single_transfer_is_named() {
        let (description, fraction) =
            summarize(&[transfer("Copying 'pelicula.mkv'", 1, 4)]).unwrap();
        assert_eq!(description, "Copying 'pelicula.mkv'");
        assert_eq!(fraction, Some(0.25));
    }

    #[test]
    fn several_transfers_are_counted_instead_of_named() {
        // Naming one of three would claim the other two are not happening.
        let (description, _) = summarize(&[
            transfer("Copying 'a'", 0, 1),
            transfer("Moving 'b'", 0, 1),
            transfer("Copying 'c'", 0, 1),
        ])
        .unwrap();
        assert_eq!(description, "3 transfers");
    }

    #[test]
    fn several_transfers_share_one_fraction() {
        let (_, fraction) = summarize(&[
            transfer("Copying 'a'", 50, 100),
            transfer("Copying 'b'", 25, 100),
        ])
        .unwrap();
        assert_eq!(fraction, Some(0.375));
    }

    #[test]
    fn an_unknown_total_is_indeterminate_not_a_division() {
        // Dividing by zero gives NaN, which GTK paints as a bar frozen at
        // nothing while the copy plainly runs.
        let (_, fraction) = summarize(&[transfer("Copying 'x'", 0, 0)]).unwrap();
        assert_eq!(fraction, None);
    }

    #[test]
    fn a_copy_that_has_reported_no_bytes_yet_is_indeterminate() {
        // The same-filesystem case: `copy_file_range` reports the total up
        // front and then nothing until it is finished. Zero of three gigabytes
        // is not "0% done", it is "no idea yet".
        let (_, fraction) = summarize(&[transfer("Copying 'huge.bin'", 0, 3_000_000_000)]).unwrap();
        assert_eq!(fraction, None);
    }

    #[test]
    fn a_fraction_never_leaves_its_bar() {
        // A copy can report more bytes than it predicted, on a sparse file or
        // a filesystem that pads. The bar must not overflow.
        let (_, fraction) = summarize(&[transfer("Copying 'x'", 120, 100)]).unwrap();
        assert_eq!(fraction, Some(1.0));
    }
}
