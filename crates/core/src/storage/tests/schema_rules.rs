//! Schema rules.

use super::*;

#[test]
fn review_events_are_append_only() {
    let e = env();
    let store = e.open(DAY0).unwrap();
    insert_sample(&store.conn);

    for sql in [
        "UPDATE review_events SET grade = 'fail'",
        "DELETE FROM review_events",
    ] {
        let err = store.conn.execute(sql, []).unwrap_err();
        assert!(err.to_string().contains("append-only"), "{sql}: {err}");
    }
    assert_eq!(count(&store.conn, "review_events"), 1);
}

#[test]
fn card_with_reviews_cannot_be_deleted() {
    let e = env();
    let store = e.open(DAY0).unwrap();
    insert_sample(&store.conn);
    assert!(
        store
            .conn
            .execute("DELETE FROM cards WHERE id = ?1", [CARD])
            .is_err()
    );
    assert!(
        store
            .conn
            .execute("DELETE FROM decks WHERE id = ?1", [DECK])
            .is_err(),
        "deck with cards"
    );
}

#[test]
fn deleting_a_card_without_reviews_drops_its_cached_schedule() {
    let e = env();
    let store = e.open(DAY0).unwrap();
    insert_sample(&store.conn);
    store
        .conn
        .execute_batch(&format!(
            "INSERT INTO cards (id, deck_id, front_md, back_md, created_at, updated_at)
                 VALUES ('{CARD2}', '{DECK}', 'f', 'b', 1, 1);
             INSERT INTO card_schedule VALUES ('{CARD2}', 'learning', 3, 1.0, 5.0, 2, 1, 0);
             DELETE FROM cards WHERE id = '{CARD2}';"
        ))
        .unwrap();
    assert_eq!(count(&store.conn, "card_schedule"), 1);
}

#[test]
fn schema_rejects_bad_values() {
    let e = env();
    let store = e.open(DAY0).unwrap();
    insert_sample(&store.conn);
    let event = |values: &str| {
        format!(
            "INSERT INTO review_events (id, card_id, kind, reviewed_at, grade, duration_ms, undoes)
             VALUES ({values})"
        )
    };
    let new_id = "'01920000-0000-7000-8000-000000000009'";
    for sql in [
        // Grade outside Fail/Pass.
        event(&format!("{new_id}, '{CARD}', 'review', 5, 'good', 0, NULL")),
        // Negative duration.
        event(&format!("{new_id}, '{CARD}', 'review', 5, 'pass', -1, NULL")),
        // Unknown kind.
        event(&format!("{new_id}, '{CARD}', 'again', 5, 'pass', 0, NULL")),
        // A review without a grade, a reset with one.
        event(&format!("{new_id}, '{CARD}', 'review', 5, NULL, 0, NULL")),
        event(&format!("{new_id}, '{CARD}', 'reset', 5, 'pass', 0, NULL")),
        // A reset that took time.
        event(&format!("{new_id}, '{CARD}', 'reset', 5, NULL, 10, NULL")),
        // An undo without a target, or a review with one.
        event(&format!("{new_id}, '{CARD}', 'undo', 5, NULL, 0, NULL")),
        event(&format!("{new_id}, '{CARD}', 'review', 5, 'pass', 0, '{EVENT}'")),
        // An undo of a missing entry.
        event(&format!(
            "{new_id}, '{CARD}', 'undo', 5, NULL, 0, '01920000-0000-7000-8000-0000000000ff'"
        )),
        // Id not a lowercase 36-character UUID.
        "INSERT INTO decks (id, name, name_key, created_at, updated_at) VALUES ('short', 'x', 'x', 1, 1)".to_owned(),
        "INSERT INTO decks (id, name, name_key, created_at, updated_at) VALUES ('01920000-0000-7000-8000-00000000000A', 'x', 'x', 1, 1)".to_owned(),
        // STRICT: text in an integer column.
        "INSERT INTO decks (id, name, name_key, created_at, updated_at) VALUES ('01920000-0000-7000-8000-00000000000b', 'x', 'x', 'now', 1)"
            .to_owned(),
        // Unknown card.
        event(&format!(
            "{new_id}, '01920000-0000-7000-8000-0000000000ff', 'review', 5, 'pass', 0, NULL"
        )),
    ] {
        assert!(store.conn.execute(&sql, []).is_err(), "{sql}");
    }
    assert_eq!(count(&store.conn, "review_events"), 1);
}

/// The trigger of migration 0007: an undo cancels an earlier review or
/// reset of its own card, once (ADR 0028).
#[test]
fn an_undo_must_cancel_one_earlier_entry_of_its_card() {
    let e = env();
    let store = e.open(DAY0).unwrap();
    insert_sample(&store.conn);
    store
        .conn
        .execute_batch(&format!(
            "INSERT INTO cards (id, deck_id, front_md, back_md, created_at, updated_at)
                 VALUES ('{CARD2}', '{DECK}', 'f', 'b', 1, 1);"
        ))
        .unwrap();
    // EVENT is a review of CARD at 2.
    let undo = |id: &str, card: &str, at: i64, target: &str| {
        store.conn.execute(
            "INSERT INTO review_events (id, card_id, kind, reviewed_at, duration_ms, undoes)
             VALUES (?1, ?2, 'undo', ?3, 0, ?4)",
            rusqlite::params![id, card, at, target],
        )
    };
    let id = |n: u32| format!("01920000-0000-7000-8000-2{n:011}");
    // Another card's entry.
    assert!(undo(&id(1), CARD2, 5, EVENT).is_err());
    // Dated before its target.
    assert!(undo(&id(1), CARD, 1, EVENT).is_err());
    // An id that sorts before its target's.
    assert!(undo("01920000-0000-7000-8000-000000000000", CARD, 5, EVENT).is_err());
    undo(&id(1), CARD, 5, EVENT).unwrap();
    // Twice.
    assert!(undo(&id(2), CARD, 5, EVENT).is_err());
    // An undo of an undo.
    assert!(undo(&id(3), CARD, 5, &id(1)).is_err());
    assert_eq!(count(&store.conn, "review_events"), 2);
}

#[test]
fn resets_and_undos_are_append_only_too() {
    let e = env();
    let mut store = e.open(DAY0).unwrap();
    let deck = store.create_deck("d").unwrap().id;
    let card = store.add_card(&deck, "f", "b").unwrap().id;
    let review = store
        .record_review(
            &card,
            crate::domain::Grade::Pass,
            std::time::Duration::ZERO,
            DAY0,
        )
        .unwrap();
    store.undo(&card, &review.id, DAY0).unwrap();
    // The undo made the card new, so it needs a review to be reset.
    store
        .record_review(
            &card,
            crate::domain::Grade::Pass,
            std::time::Duration::ZERO,
            DAY0,
        )
        .unwrap();
    store.reset_card(&card, DAY0).unwrap();
    for sql in [
        "UPDATE review_events SET undoes = NULL WHERE kind = 'undo'",
        "UPDATE review_events SET reviewed_at = 0 WHERE kind = 'reset'",
        "DELETE FROM review_events WHERE kind = 'undo'",
        "DELETE FROM review_events WHERE kind = 'reset'",
    ] {
        let err = store.conn.execute(sql, []).unwrap_err();
        assert!(err.to_string().contains("append-only"), "{sql}: {err}");
    }
    // Review, undo, review, reset.
    assert_eq!(count(&store.conn, "review_events"), 4);
}
