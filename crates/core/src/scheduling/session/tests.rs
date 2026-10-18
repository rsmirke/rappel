use proptest::prelude::*;

use super::*;
use crate::domain::Schedule;
use crate::test_support::{DAY_MS, T0};

const MIN: i64 = 60 * 1000;

fn due(id: &str, due_at: i64) -> Due {
    Due {
        card_id: id.to_owned(),
        due_at,
    }
}

fn ids(names: &[&str]) -> Vec<String> {
    names.iter().map(|&n| n.to_owned()).collect()
}

fn state(phase: Phase, due_at: i64) -> CardState {
    CardState::Scheduled(Schedule {
        phase,
        stability: 1.0,
        difficulty: 5.0,
        due_at,
        last_review_at: T0,
        reps: 1,
        lapses: 0,
    })
}

/// Shows and answers cards at `now` until the session is over, answering
/// each with `answer` and the state it returns. Returns the cards shown.
fn run(
    session: &mut Session,
    now: i64,
    mut answer: impl FnMut(&str) -> (Grade, CardState),
) -> Vec<String> {
    let mut shown = Vec::new();
    while let Some(id) = session.next(now).map(str::to_owned) {
        assert!(shown.len() < 1_000, "the session does not end");
        let (grade, state) = answer(&id);
        session.graded(&id, "e", grade, Duration::from_secs(1), &state);
        shown.push(id);
    }
    shown
}

#[test]
fn interleave_spreads_new_cards_evenly() {
    let r = ids(&["r1", "r2", "r3", "r4", "r5", "r6"]);
    let n = ids(&["n1", "n2"]);
    assert_eq!(
        interleave(r, n),
        ids(&["r1", "r2", "n1", "r3", "r4", "n2", "r5", "r6"])
    );
    assert_eq!(interleave(ids(&["r"]), ids(&["n"])), ids(&["r", "n"]));
    assert_eq!(
        interleave(Vec::new(), ids(&["n1", "n2"])),
        ids(&["n1", "n2"])
    );
    assert_eq!(
        interleave(ids(&["r1", "r2"]), Vec::new()),
        ids(&["r1", "r2"])
    );
    assert_eq!(
        interleave::<String>(Vec::new(), Vec::new()),
        Vec::<String>::new()
    );
}

proptest! {
    #[test]
    fn interleave_keeps_everything_in_order(r in 0_usize..60, n in 0_usize..60) {
        let reviews: Vec<(bool, usize)> = (0..r).map(|j| (false, j)).collect();
        let new: Vec<(bool, usize)> = (0..n).map(|i| (true, i)).collect();
        let out = interleave(reviews.clone(), new.clone());
        prop_assert_eq!(out.len(), r + n);
        let out_reviews: Vec<_> = out.iter().copied().filter(|x| !x.0).collect();
        let out_new: Vec<_> = out.iter().copied().filter(|x| x.0).collect();
        prop_assert_eq!(out_reviews, reviews);
        prop_assert_eq!(out_new, new);
        if r >= n {
            prop_assert!(
                out.windows(2).all(|w| !(w[0].0 && w[1].0)),
                "two new cards side by side"
            );
        }
    }
}

#[test]
fn allowance_and_cap() {
    assert_eq!(allowance(20, 0), 20);
    assert_eq!(allowance(20, 5), 15);
    assert_eq!(allowance(20, 20), 0);
    assert_eq!(allowance(20, 23), 0);
    assert_eq!(allowance(0, 0), 0);
    assert_eq!(capped(Duration::from_secs(5)), Duration::from_secs(5));
    assert_eq!(capped(Duration::from_secs(3_600)), MAX_DURATION);
}

#[test]
fn due_steps_come_first_then_the_queue() {
    let input = StudyInput {
        reviews: vec![due("r1", T0 - MIN), due("r2", T0)],
        steps: vec![due("s-later", T0 + 5 * MIN), due("s-due", T0 - MIN)],
        new: ids(&["n1"]),
        new_today: 0,
        reviews_today: 0,
    };
    let mut session = Session::new(input);
    assert_eq!(session.remaining(), 5);
    let shown = run(&mut session, T0, |_| {
        (Grade::Pass, state(Phase::Review, T0 + 9 * 86_400_000))
    });
    // s-later is shown early, only once the queue is empty.
    assert_eq!(shown, ids(&["s-due", "r1", "n1", "r2", "s-later"]));
    assert_eq!(session.remaining(), 0);
}

#[test]
fn a_card_in_the_step_returns_in_the_same_session() {
    let input = StudyInput {
        new: ids(&["n1", "n2"]),
        ..StudyInput::default()
    };
    let mut session = Session::new(input);
    let mut answers = 0;
    let shown = run(&mut session, T0, |_| {
        answers += 1;
        // New cards enter the 10-minute step; their second answer
        // graduates them.
        if answers <= 2 {
            (Grade::Pass, state(Phase::Learning, T0 + 10 * MIN))
        } else {
            (Grade::Pass, state(Phase::Review, T0 + 86_400_000))
        }
    });
    assert_eq!(shown, ids(&["n1", "n2", "n1", "n2"]));
    let summary = session.summary();
    assert_eq!((summary.cards, summary.answers, summary.passes), (2, 4, 4));
}

#[test]
fn early_showing_is_at_most_twenty_minutes_and_otherwise_ends() {
    let input = StudyInput {
        steps: vec![due("far", T0 + EARLY_MS + 1), due("near", T0 + EARLY_MS)],
        ..StudyInput::default()
    };
    let mut session = Session::new(input);
    assert_eq!(session.next(T0), Some("near"));
    session.graded(
        "near",
        "e",
        Grade::Pass,
        Duration::ZERO,
        &state(Phase::Review, T0 + 86_400_000),
    );
    // Only a card more than 20 minutes away is left: the session is over,
    // and the card stays for a later session.
    assert_eq!(session.next(T0), None);
    assert_eq!(session.remaining(), 1);
    // A millisecond later it is within the window.
    assert_eq!(session.next(T0 + 1), Some("far"));
}

#[test]
fn a_failed_review_relearns_in_the_session() {
    let input = StudyInput {
        reviews: vec![due("r1", T0)],
        ..StudyInput::default()
    };
    let mut session = Session::new(input);
    session.graded(
        "r1",
        "e",
        Grade::Fail,
        Duration::ZERO,
        &state(Phase::Relearning, T0 + 10 * MIN),
    );
    assert_eq!(
        session.next(T0),
        Some("r1"),
        "shown early: nothing else left"
    );
    session.graded(
        "r1",
        "e",
        Grade::Pass,
        Duration::ZERO,
        &state(Phase::Review, T0 + 86_400_000),
    );
    assert_eq!(session.next(T0), None);
}

#[test]
fn summary_counts_answers_passes_and_capped_time() {
    let mut session = Session::new(StudyInput {
        reviews: vec![due("a", T0), due("b", T0), due("c", T0)],
        ..StudyInput::default()
    });
    let review = state(Phase::Review, T0 + 86_400_000);
    session.graded("a", "e", Grade::Pass, Duration::from_secs(10), &review);
    session.graded(
        "b",
        "e",
        Grade::Fail,
        Duration::from_secs(600),
        &state(Phase::Relearning, T0),
    );
    session.graded("b", "e", Grade::Pass, Duration::from_millis(2_500), &review);
    let summary = session.summary();
    assert_eq!(summary.cards, 2);
    assert_eq!(summary.answers, 3);
    assert_eq!(summary.passes, 2);
    assert_eq!(summary.time, Duration::from_millis(72_500));
    assert_eq!(summary.pass_percent(), Some(67));
    assert_eq!(Summary::default().pass_percent(), None);
    assert_eq!(session.next(T0), Some("c"));
}

#[test]
fn empty_input_is_empty() {
    assert!(StudyInput::default().is_empty());
    let input = StudyInput {
        new: ids(&["n"]),
        ..StudyInput::default()
    };
    assert!(!input.is_empty());
    assert_eq!(Session::new(StudyInput::default()).next(T0), None);
}

#[test]
fn a_skipped_card_is_dropped_and_not_counted() {
    let mut session = Session::new(StudyInput {
        reviews: vec![due("a", T0), due("b", T0)],
        steps: vec![due("s", T0 - MIN)],
        ..StudyInput::default()
    });
    session.skip("s");
    session.skip("a");
    assert_eq!(session.next(T0), Some("b"));
    assert_eq!(session.remaining(), 1);
    assert_eq!(session.summary(), Summary::default());
}

#[test]
fn a_card_put_back_is_the_next_one_and_only_once() {
    let mut session = Session::new(StudyInput {
        reviews: vec![due("a", T0), due("b", T0)],
        steps: vec![due("s", T0 + 5 * MIN)],
        ..StudyInput::default()
    });
    session.skip("a");
    session.skip("s");
    assert_eq!(session.remaining(), 1);

    session.put_back("a");
    assert_eq!((session.next(T0), session.remaining()), (Some("a"), 2));
    // Already queued: nothing changes.
    session.put_back("a");
    session.put_back("b");
    assert_eq!((session.next(T0), session.remaining()), (Some("a"), 2));
    // A step card comes back too, as the next one.
    session.put_back("s");
    assert_eq!((session.next(T0), session.remaining()), (Some("s"), 3));
    // The summary is untouched.
    assert_eq!(session.summary(), Summary::default());
}

#[test]
fn a_card_an_undo_brought_back_is_not_put_back_twice() {
    let mut session = session_of(&["a", "b"], &[]);
    session.graded(
        "a",
        "e1",
        Grade::Pass,
        Duration::ZERO,
        &state(Phase::Review, T0 + DAY_MS),
    );
    assert_eq!(session.undo_last().as_deref(), Some("a"));
    let before = session.remaining();
    session.put_back("a");
    assert_eq!(session.remaining(), before);
    assert_eq!(session.next(T0), Some("a"));
}

#[test]
fn undoing_an_answer_does_not_queue_a_card_put_back_twice() {
    let mut session = session_of(&["a"], &[]);
    session.graded(
        "a",
        "e1",
        Grade::Fail,
        Duration::ZERO,
        &state(Phase::Relearning, T0),
    );
    // Failed: out of the queue, waiting in a step. Dropped, then put back.
    session.skip("a");
    session.put_back("a");
    assert_eq!(session.remaining(), 1);
    // Undoing the answer puts the card where it was, but it is queued once.
    assert_eq!(session.undo_last().as_deref(), Some("a"));
    assert_eq!(session.remaining(), 1);
    assert_eq!(session.next(T0), Some("a"));
}

#[test]
fn a_due_step_card_still_comes_before_a_card_put_back() {
    let mut session = Session::new(StudyInput {
        reviews: vec![due("a", T0)],
        steps: vec![due("s", T0 - MIN)],
        ..StudyInput::default()
    });
    session.skip("a");
    session.put_back("a");
    // "s" is due and comes first; "a" is next after it.
    assert_eq!(session.next(T0), Some("s"));
    session.graded(
        "s",
        "e1",
        Grade::Pass,
        Duration::ZERO,
        &state(Phase::Review, T0 + DAY_MS),
    );
    assert_eq!(session.next(T0), Some("a"));
}

/// A session of reviews `a..` and new cards, with every card still to come.
fn session_of(reviews: &[&str], new: &[&str]) -> Session {
    Session::new(StudyInput {
        reviews: reviews.iter().map(|id| due(id, T0)).collect(),
        new: ids(new),
        ..StudyInput::default()
    })
}

#[test]
fn undoing_the_only_answer_gives_back_the_session() {
    let fresh = session_of(&["a", "b"], &["n"]);
    for (grade, phase) in [
        (Grade::Pass, Phase::Review),
        (Grade::Fail, Phase::Relearning),
    ] {
        let mut session = fresh.clone();
        let id = session.next(T0).unwrap().to_owned();
        session.graded(
            &id,
            "e1",
            grade,
            Duration::from_secs(5),
            &state(phase, T0 + MIN),
        );
        assert_eq!(session.last_answer(), Some((id.as_str(), "e1")));
        assert_eq!(session.undo_last().as_deref(), Some(id.as_str()));
        // Only the pin differs from the session before the answer.
        assert_eq!(session.next(T0), Some(id.as_str()));
        session.pinned = None;
        assert_eq!(session, fresh);
    }
}

#[test]
fn undo_goes_back_to_the_first_answer_and_then_stops() {
    let mut session = session_of(&["a", "b", "c"], &[]);
    let review = state(Phase::Review, T0 + 86_400_000);
    for (id, entry) in [("a", "e1"), ("b", "e2"), ("c", "e3")] {
        session.graded(id, entry, Grade::Pass, Duration::from_secs(1), &review);
    }
    assert_eq!(session.next(T0), None);
    for (id, entry) in [("c", "e3"), ("b", "e2"), ("a", "e1")] {
        assert_eq!(session.last_answer(), Some((id, entry)));
        assert_eq!(session.undo_last().as_deref(), Some(id));
        assert_eq!(session.next(T0), Some(id));
    }
    // Repeating Ctrl+Z past the first answer does nothing.
    assert_eq!(session.last_answer(), None);
    assert_eq!(session.undo_last(), None);
    assert_eq!(session.summary(), Summary::default());
    assert_eq!(session.remaining(), 3);
}

#[test]
fn an_undone_card_is_shown_before_a_step_card_that_became_due() {
    let mut session = session_of(&["a", "b"], &[]);
    session.graded(
        "a",
        "e1",
        Grade::Fail,
        Duration::ZERO,
        &state(Phase::Relearning, T0 + MIN),
    );
    session.graded(
        "b",
        "e2",
        Grade::Pass,
        Duration::ZERO,
        &state(Phase::Review, T0 + DAY_MS),
    );
    // "a" is in the step and due; undoing "b" still shows "b".
    assert_eq!(session.next(T0 + 2 * MIN), Some("a"));
    session.undo_last();
    assert_eq!(session.next(T0 + 2 * MIN), Some("b"));
    // Answering it clears the pin.
    session.graded(
        "b",
        "e3",
        Grade::Pass,
        Duration::ZERO,
        &state(Phase::Review, T0 + DAY_MS),
    );
    assert_eq!(session.next(T0 + 2 * MIN), Some("a"));
}

#[test]
fn undoing_a_second_answer_of_a_step_card_puts_it_back_in_the_step() {
    let mut session = session_of(&["a"], &[]);
    let step = state(Phase::Relearning, T0 + 10 * MIN);
    session.graded("a", "e1", Grade::Fail, Duration::ZERO, &step);
    let after_first = session.clone();
    session.graded(
        "a",
        "e2",
        Grade::Fail,
        Duration::ZERO,
        &state(Phase::Relearning, T0 + 20 * MIN),
    );
    session.undo_last();
    session.pinned = None;
    session.answers = after_first.answers.clone();
    assert_eq!(session, after_first);
}

#[test]
fn forgetting_the_answers_turns_undo_off_and_keeps_the_rest() {
    let mut session = session_of(&["a", "b"], &[]);
    session.graded(
        "a",
        "e1",
        Grade::Pass,
        Duration::ZERO,
        &state(Phase::Review, T0 + DAY_MS),
    );
    session.forget_answers();
    assert_eq!(session.last_answer(), None);
    assert_eq!(session.undo_last(), None);
    assert_eq!(session.summary().answers, 1);
}

proptest! {
    /// Any run of answers and undos leaves the session equal to one that
    /// only got the answers that survive: the whole session, queue order,
    /// steps, seen cards, summary and the answers left to undo included
    /// (#57), except the card the last undo pinned, which the plain
    /// session never had.
    #[test]
    fn undos_leave_the_surviving_answers(
        ops in proptest::collection::vec((any::<bool>(), any::<bool>(), any::<bool>()), 0..40),
    ) {
        let make = || session_of(&["r1", "r2", "r3", "r4"], &["n1", "n2"]);
        let (mut session, mut plain) = (make(), make());
        let mut kept: Vec<(String, String, Grade, CardState)> = Vec::new();
        let mut last_undone = None;
        for (i, (undo, pass, learning)) in ops.into_iter().enumerate() {
            if undo {
                if let Some(id) = session.undo_last() {
                    let (answered, ..) = kept.pop().expect("an undo takes back a kept answer");
                    prop_assert_eq!(&id, &answered);
                    last_undone = Some(id);
                }
                continue;
            }
            let Some(id) = session.next(T0).map(str::to_owned) else { continue };
            let grade = if pass { Grade::Pass } else { Grade::Fail };
            let phase = if learning { Phase::Relearning } else { Phase::Review };
            let st = state(phase, T0 + MIN + i64::try_from(i).unwrap());
            let entry = format!("e{i}");
            session.graded(&id, &entry, grade, Duration::from_secs(2), &st);
            // Answering the card an undo brought back unpins it.
            if last_undone.as_deref() == Some(id.as_str()) {
                last_undone = None;
            }
            kept.push((id, entry, grade, st));
        }
        // Replaying the surviving answers on a plain session, in order,
        // each time answering the card the first session had on screen,
        // with the same entry and the same new state.
        for (id, entry, grade, st) in &kept {
            plain.graded(id, entry, *grade, Duration::from_secs(2), st);
        }
        // The pin is what an undo adds: the card it brought back comes next.
        prop_assert_eq!(session.pinned.take(), last_undone);
        prop_assert_eq!(session, plain);
    }
}
