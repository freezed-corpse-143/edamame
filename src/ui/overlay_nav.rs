//! Shared "advance focus by N, skipping non-focusable rows" walk for the modal overlays.  Row
//! types differ between overlays, so the helpers take a focusability predicate.

/// Nearest row index reached by stepping `delta` from `current`, skipping non-focusable rows.
/// Non-wrapping by design (bouncing off the ends feels jumpy): returns `None` when no focusable
/// row lies in that direction, so the caller can leave focus where it was.
pub fn next_focusable<T>(
    rows: &[T],
    current: usize,
    delta: i32,
    is_focusable: impl Fn(&T) -> bool,
) -> Option<usize> {
    if rows.is_empty() || delta == 0 {
        return None;
    }
    let len = rows.len() as i32;
    let mut idx = current as i32 + delta;
    while (0..len).contains(&idx) {
        let i = idx as usize;
        if is_focusable(&rows[i]) {
            return Some(i);
        }
        idx += delta;
    }
    None
}

/// Like [`next_focusable`], but wrapping: past the last focusable row the walk continues from
/// the first.  When `current` is the only focusable row it is reached on the final wrap step and
/// returned unchanged (a no-op move).  Used by the welcome and export-HTML modals, whose focus
/// rings wrap; settings / keybinds use the non-wrapping variant.
pub fn next_focusable_wrapping<T>(
    rows: &[T],
    current: usize,
    delta: i32,
    is_focusable: impl Fn(&T) -> bool,
) -> Option<usize> {
    if rows.is_empty() || delta == 0 {
        return None;
    }
    let len = rows.len() as i32;
    let mut idx = current as i32;
    // Hard bound so an all-disabled ring can't spin forever.
    for _ in 0..len {
        idx = (idx + delta).rem_euclid(len);
        let i = idx as usize;
        if is_focusable(&rows[i]) {
            return Some(i);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_non_focusable_rows_forward() {
        let rows = [true, false, false, true, false, true];
        assert_eq!(next_focusable(&rows, 0, 1, |r| *r), Some(3));
        assert_eq!(next_focusable(&rows, 3, 1, |r| *r), Some(5));
    }

    #[test]
    fn returns_none_when_no_focusable_in_direction() {
        let rows = [true, false, false];
        assert_eq!(next_focusable(&rows, 0, 1, |r| *r), None);
        assert_eq!(next_focusable(&rows, 0, -1, |r| *r), None);
    }

    #[test]
    fn empty_rows_returns_none() {
        let rows: [bool; 0] = [];
        assert_eq!(next_focusable(&rows, 0, 1, |r| *r), None);
    }

    #[test]
    fn zero_delta_returns_none() {
        let rows = [true, true, true];
        assert_eq!(next_focusable(&rows, 1, 0, |r| *r), None);
    }

    #[test]
    fn skips_backward() {
        let rows = [true, false, false, true];
        assert_eq!(next_focusable(&rows, 3, -1, |r| *r), Some(0));
    }

    // ── next_focusable_wrapping ──────────────────────────────────────────

    #[test]
    fn wrapping_wraps_forward_off_the_end() {
        let rows = [true, true, true];
        assert_eq!(next_focusable_wrapping(&rows, 2, 1, |r| *r), Some(0));
    }

    #[test]
    fn wrapping_wraps_backward_off_the_start() {
        let rows = [true, true, true];
        assert_eq!(next_focusable_wrapping(&rows, 0, -1, |r| *r), Some(2));
    }

    #[test]
    fn wrapping_skips_non_focusable_then_wraps() {
        let rows = [true, false, true, false, false];
        assert_eq!(next_focusable_wrapping(&rows, 2, 1, |r| *r), Some(0));
        assert_eq!(next_focusable_wrapping(&rows, 0, -1, |r| *r), Some(2));
    }

    #[test]
    fn wrapping_lone_focusable_returns_itself() {
        // Only `current` is focusable: it wraps back to itself, not None.
        let rows = [false, true, false];
        assert_eq!(next_focusable_wrapping(&rows, 1, 1, |r| *r), Some(1));
        assert_eq!(next_focusable_wrapping(&rows, 1, -1, |r| *r), Some(1));
    }

    #[test]
    fn wrapping_all_non_focusable_returns_none() {
        let rows = [false, false, false];
        assert_eq!(next_focusable_wrapping(&rows, 0, 1, |r| *r), None);
    }

    #[test]
    fn wrapping_zero_delta_and_empty_return_none() {
        let rows = [true, true];
        assert_eq!(next_focusable_wrapping(&rows, 0, 0, |r| *r), None);
        let empty: [bool; 0] = [];
        assert_eq!(next_focusable_wrapping(&empty, 0, 1, |r| *r), None);
    }
}
