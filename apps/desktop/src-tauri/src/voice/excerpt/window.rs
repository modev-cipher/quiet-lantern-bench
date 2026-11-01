//! The segment window around the playback position (VOICE_AGENT §4: context is
//! ±N segments around the playback position, never whole chapters).
//!
//! The window is a pure function of `(snapshot, pos_ms, policy)`. Position
//! arrives in milliseconds on the reading timeline — the same clock the VA1
//! audio core exposes as "frames pulled → `pulled_ms`" (audio is its own clock)
//! and the VA2 seam delivers as `ContextUpdate::PlaybackPosition { pos_ms }`.
//! Consuming plain milliseconds keeps this provider-neutral and testable with
//! no devices and no wall-clock reads.

use super::source::SessionSnapshot;

/// How many segments to include on each side of the anchor. The spec fixes no
/// N — the working bound is the per-provider token cap (VA4 U4); this policy
/// only shapes the candidate window the token cap then trims.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowPolicy {
    pub before: usize,
    pub after: usize,
}

/// An inclusive segment-idx window, plus the anchor it was built around.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowSelection {
    pub anchor_idx: usize,
    pub first_idx: usize,
    pub last_idx: usize,
}

/// Resolve the window around `pos_ms`. `None` only on an empty snapshot.
///
/// Anchor resolution: the segment whose `[start_ms, end_ms)` span contains the
/// position; a position inside a gap (chapter gaps exist on the timeline)
/// anchors on the **next upcoming** segment (the listener is about to hear it,
/// so it is the relevant context); a position at or past the final segment's
/// end anchors on the last segment. Seeks in both directions resolve the same
/// way — the function has no memory.
pub fn window_around(
    snapshot: &SessionSnapshot,
    pos_ms: u64,
    policy: WindowPolicy,
) -> Option<WindowSelection> {
    let segments = snapshot.segments();
    if segments.is_empty() {
        return None;
    }
    let top = segments.len() - 1;
    let started = segments.partition_point(|segment| segment.start_ms <= pos_ms);
    let anchor_idx = match started.checked_sub(1) {
        // Before the first segment starts: the first segment is upcoming.
        None => 0,
        Some(candidate) => {
            if pos_ms < segments[candidate].end_ms {
                candidate
            } else {
                // In the gap after `candidate`: anchor the next upcoming
                // segment, unless the book is over — then stay on the last.
                candidate.saturating_add(1).min(top)
            }
        }
    };
    Some(WindowSelection {
        anchor_idx,
        first_idx: anchor_idx.saturating_sub(policy.before),
        last_idx: anchor_idx.saturating_add(policy.after).min(top),
    })
}

pub fn page_window_around(snapshot: &SessionSnapshot, pos_ms: u64) -> Option<WindowSelection> {
    let anchor = window_around(
        snapshot,
        pos_ms,
        WindowPolicy {
            before: 0,
            after: 0,
        },
    )?;
    let segments = snapshot.segments();
    let page = segments[anchor.anchor_idx].page_no;
    let mut first = anchor.anchor_idx;
    while first > 0 && segments[first - 1].page_no == page {
        first -= 1;
    }
    let mut last = anchor.anchor_idx;
    while last + 1 < segments.len() && segments[last + 1].page_no == page {
        last += 1;
    }
    Some(WindowSelection {
        anchor_idx: anchor.anchor_idx,
        first_idx: first,
        last_idx: last,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_window_spans_exactly_the_anchor_page() {
        let snapshot = snapshot();
        assert_eq!(
            page_window_around(&snapshot, 4700),
            Some(WindowSelection {
                anchor_idx: 4,
                first_idx: 3,
                last_idx: 4,
            })
        );
        assert_eq!(
            page_window_around(&snapshot, 2500),
            Some(WindowSelection {
                anchor_idx: 2,
                first_idx: 2,
                last_idx: 2,
            })
        );
    }

    // Six 1-second segments with a 600 ms chapter gap between idx 2 and 3
    // (mirrors the CHAPTER_GAP_MS layout the reading timeline produces).
    fn snapshot() -> SessionSnapshot {
        SessionSnapshot::from_test_parts(vec![
            (1, 0, 1000, "s0"),
            (1, 1000, 2000, "s1"),
            (2, 2000, 3000, "s2"),
            (3, 3600, 4600, "s3"),
            (3, 4600, 5600, "s4"),
            (4, 5600, 6600, "s5"),
        ])
    }

    const ONE_EACH_SIDE: WindowPolicy = WindowPolicy {
        before: 1,
        after: 1,
    };

    fn window(pos_ms: u64) -> (usize, usize, usize) {
        let sel = window_around(&snapshot(), pos_ms, ONE_EACH_SIDE).expect("non-empty");
        (sel.first_idx, sel.anchor_idx, sel.last_idx)
    }

    #[test]
    fn a_position_inside_a_segment_anchors_that_segment() {
        assert_eq!(window(1500), (0, 1, 2));
    }

    #[test]
    fn the_left_edge_clamps_at_the_first_segment() {
        assert_eq!(window(0), (0, 0, 1));
    }

    #[test]
    fn the_right_edge_clamps_at_the_last_segment() {
        assert_eq!(window(6000), (4, 5, 5));
    }

    #[test]
    fn a_gap_position_anchors_the_upcoming_segment() {
        // 3200 ms sits in the 3000..3600 chapter gap; the listener is about to
        // hear s3, so s3 is the context that matters.
        assert_eq!(window(3200), (2, 3, 4));
    }

    #[test]
    fn a_segment_end_boundary_belongs_to_the_next_segment() {
        // end_ms is exclusive: at exactly 1000 ms segment 0 has finished.
        assert_eq!(window(1000), (0, 1, 2));
    }

    #[test]
    fn past_the_end_of_the_book_anchors_the_last_segment() {
        assert_eq!(window(999_999), (4, 5, 5));
    }

    #[test]
    fn an_asymmetric_policy_is_honoured() {
        let sel = window_around(
            &snapshot(),
            4700,
            WindowPolicy {
                before: 3,
                after: 0,
            },
        )
        .expect("non-empty");
        assert_eq!(
            (sel.first_idx, sel.anchor_idx, sel.last_idx),
            (1, 4, 4),
            "before/after are independent bounds, not a radius"
        );
    }

    #[test]
    fn an_empty_snapshot_has_no_window() {
        let empty = SessionSnapshot::from_test_parts(vec![]);
        assert_eq!(window_around(&empty, 0, ONE_EACH_SIDE), None);
    }
}
