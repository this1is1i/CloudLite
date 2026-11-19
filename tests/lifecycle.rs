use cloudlite::model::{PlaybackIntent, SearchPager};

#[test]
fn paging_keeps_original_query_and_rejects_duplicate_or_stale_responses() {
    let mut pager = SearchPager::default();
    assert_eq!(pager.begin(1, "original".into()), ("original".into(), 0));
    assert!(pager.accept(1, 0, 100, 250));
    assert_eq!(pager.next_page(), Some(("original".into(), 100)));
    assert!(
        pager.next_page().is_none(),
        "double click must not request the same offset twice"
    );
    pager.begin(2, "new query".into());
    assert!(
        !pager.accept(1, 100, 100, 250),
        "old query cannot contaminate a new one"
    );
    assert!(pager.accept(2, 0, 5, 5));
    assert!(
        !pager.accept(2, 0, 5, 5),
        "duplicate response must be ignored"
    );
    assert!(!pager.has_more());
}

#[test]
fn pending_track_respects_pause_and_ignores_previous_track_completion() {
    let mut intent = PlaybackIntent::default();
    intent.begin(1);
    assert_eq!(intent.ready(1), Some(true));
    intent.begin(2);
    intent.set_playing(false);
    assert!(
        !intent.ended(1),
        "old source ending cannot skip the pending selection"
    );
    assert_eq!(intent.ready(1), None);
    assert_eq!(
        intent.ready(2),
        Some(false),
        "loading must not override a requested pause"
    );
    intent.set_playing(true);
    intent.set_playing(true);
    assert!(intent.playing, "system Play must be idempotent");
    intent.set_playing(false);
    intent.set_playing(false);
    assert!(!intent.playing, "system Pause must be idempotent");
    assert!(intent.ended(2));
    assert!(!intent.ended(2), "duplicate end must advance only once");
}
