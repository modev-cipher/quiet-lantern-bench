use txtcaster_lib::cache_gc;
use txtcaster_lib::storage::{self, settings, SqlitePool};

async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("open pool");
    (dir, pool)
}

#[tokio::test]
async fn an_unset_cap_reads_as_the_default_rather_than_as_no_limit() {
    let (_dir, pool) = test_pool().await;
    assert_eq!(
        cache_gc::read_cap_bytes(&pool).await.expect("read"),
        Some(cache_gc::DEFAULT_CAP_BYTES)
    );
}

#[tokio::test]
async fn a_written_cap_is_what_the_reader_returns() {
    let (_dir, pool) = test_pool().await;
    cache_gc::write_cap_bytes(&pool, 5 * 1024 * 1024 * 1024)
        .await
        .expect("write");
    assert_eq!(
        cache_gc::read_cap_bytes(&pool).await.expect("read"),
        Some(5 * 1024 * 1024 * 1024)
    );
}

#[tokio::test]
async fn writing_zero_means_no_limit_and_is_not_confused_with_unset() {
    let (_dir, pool) = test_pool().await;
    cache_gc::write_cap_bytes(&pool, 0).await.expect("write");
    assert_eq!(
        cache_gc::read_cap_bytes(&pool).await.expect("read"),
        None,
        "zero is the explicit no-limit choice"
    );
    assert_eq!(
        settings::get_setting(&pool, cache_gc::CAP_SETTING_KEY)
            .await
            .expect("raw"),
        Some("0".to_string()),
        "the choice is stored, not represented by an absent row"
    );
}

#[tokio::test]
async fn a_negative_cap_is_refused_and_leaves_the_stored_cap_untouched() {
    let (_dir, pool) = test_pool().await;
    cache_gc::write_cap_bytes(&pool, 1024).await.expect("write");

    let rejection = cache_gc::write_cap_bytes(&pool, -1)
        .await
        .expect_err("a negative cap must be refused");
    assert!(
        rejection.contains("no limit"),
        "the reason must name the consequence, got {rejection}"
    );

    assert_eq!(
        cache_gc::read_cap_bytes(&pool).await.expect("read"),
        Some(1024),
        "a refused write must not disturb the cap already in force"
    );
}

#[tokio::test]
async fn clearing_the_cap_restores_the_default_instead_of_removing_the_limit() {
    let (_dir, pool) = test_pool().await;
    cache_gc::write_cap_bytes(&pool, 0).await.expect("write");
    assert_eq!(cache_gc::read_cap_bytes(&pool).await.expect("read"), None);

    cache_gc::clear_cap(&pool).await.expect("clear");

    assert_eq!(
        cache_gc::read_cap_bytes(&pool).await.expect("read"),
        Some(cache_gc::DEFAULT_CAP_BYTES)
    );
}

#[tokio::test]
async fn a_hand_edited_unparsable_cap_reads_as_the_default_rather_than_as_no_limit() {
    let (_dir, pool) = test_pool().await;
    for raw in ["20 GB", "", "1.5", "nonsense"] {
        settings::set_setting(&pool, cache_gc::CAP_SETTING_KEY, raw)
            .await
            .expect("hand edit");
        assert_eq!(
            cache_gc::read_cap_bytes(&pool).await.expect("read"),
            Some(cache_gc::DEFAULT_CAP_BYTES),
            "raw {raw:?} must not disable the cap"
        );
    }
}

#[tokio::test]
async fn a_cap_survives_reopening_the_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db.sqlite");

    let pool = storage::open_pool(&db).await.expect("open");
    cache_gc::write_cap_bytes(&pool, 7 * 1024 * 1024 * 1024)
        .await
        .expect("write");
    pool.close().await;

    let reopened = storage::open_pool(&db).await.expect("reopen");
    assert_eq!(
        cache_gc::read_cap_bytes(&reopened).await.expect("read"),
        Some(7 * 1024 * 1024 * 1024)
    );
}

#[tokio::test]
async fn a_refused_cap_is_reported_as_the_default_in_force_not_as_a_chosen_size() {
    let (_dir, pool) = test_pool().await;
    settings::set_setting(&pool, cache_gc::CAP_SETTING_KEY, "20 GB")
        .await
        .expect("hand edit");

    let source = cache_gc::read_cap(&pool).await.expect("read");

    assert_eq!(
        source,
        cache_gc::CapSource::Refused {
            raw: "20 GB".to_string()
        },
        "a value the reader cannot use must not be presented as a deliberate custom size"
    );
    assert_eq!(source.effective_bytes(), Some(cache_gc::DEFAULT_CAP_BYTES));
}

#[tokio::test]
async fn every_stored_shape_classifies_to_exactly_one_source() {
    assert_eq!(cache_gc::classify_cap(None), cache_gc::CapSource::Default);
    assert_eq!(
        cache_gc::classify_cap(Some("0")),
        cache_gc::CapSource::Unlimited
    );
    assert_eq!(
        cache_gc::classify_cap(Some("-5")),
        cache_gc::CapSource::Unlimited,
        "the reader takes any non-positive value as no limit; the writer refuses negatives \
         so only a hand-edit reaches this"
    );
    assert_eq!(
        cache_gc::classify_cap(Some(" 1024 ")),
        cache_gc::CapSource::Configured(1024)
    );
    assert_eq!(
        cache_gc::classify_cap(Some("")),
        cache_gc::CapSource::Refused {
            raw: "".to_string()
        }
    );
}

#[tokio::test]
async fn the_classifier_and_the_byte_reader_can_never_disagree() {
    let (_dir, pool) = test_pool().await;
    for raw in ["0", "-5", "1024", "20 GB", "", "   ", "9223372036854775808"] {
        settings::set_setting(&pool, cache_gc::CAP_SETTING_KEY, raw)
            .await
            .expect("set");
        let source = cache_gc::read_cap(&pool).await.expect("source");
        let bytes = cache_gc::read_cap_bytes(&pool).await.expect("bytes");
        assert_eq!(source.effective_bytes(), bytes, "raw {raw:?}");
    }
}
