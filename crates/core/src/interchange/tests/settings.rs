//! Settings and the exchange format (ADR 0027): version 2 carries the
//! stored settings; an import applies them only into a database without
//! decks.

use super::*;

fn set_all(store: &mut Store) {
    store.set_desired_retention(0.85).unwrap();
    store.set_max_new_per_day(3).unwrap();
    store.set_max_reviews_per_day(4).unwrap();
    store.set_day_start_hour(6).unwrap();
}

#[test]
fn only_the_settings_the_user_changed_reach_the_export() {
    let (mut env, _, _) = sample_env();
    assert_eq!(json_of(&env.export())["settings"], json!({}));
    env.store.set_max_new_per_day(20).unwrap();
    // The default, but written: it is exported, as the user chose it.
    assert_eq!(
        json_of(&env.export())["settings"],
        json!({"max_new_per_day": 20})
    );
    set_all(&mut env.store);
    assert_eq!(
        json_of(&env.export())["settings"],
        json!({
            "desired_retention": 0.85,
            "max_new_per_day": 3,
            "max_reviews_per_day": 4,
            "day_start_hour": 6
        })
    );
}

#[test]
fn settings_move_into_a_database_without_decks() {
    let (mut source, _, card) = sample_env();
    set_all(&mut source.store);
    let bytes = source.export();
    let mut target = Env::new();
    // Settings written in the empty target are replaced, keys the file
    // does not have included.
    target.store.set_max_new_per_day(7).unwrap();
    let plan = target.plan(&bytes).unwrap();
    assert_eq!(plan.summary().settings, SettingsImport::Applied);
    let report = target.store.apply_import_at(&plan, T_EXPORT).unwrap();
    assert!(report.settings_applied);
    assert_eq!(
        target.store.settings().unwrap(),
        source.store.settings().unwrap()
    );
    // The schedules were rebuilt with the file's retention.
    assert_eq!(schedule_table(&target.store), schedule_table(&source.store));
    assert_eq!(target.export(), bytes);
    let events = target.store.review_events(&card).unwrap();
    assert_eq!(
        target.store.card_state(&card).unwrap(),
        Scheduler::new(0.85)
            .unwrap()
            .rebuild_state(&card, &events)
            .unwrap()
    );
    // Applied once, the same settings are unchanged the next time.
    let again = target.plan(&bytes).unwrap();
    assert_eq!(again.summary().settings, SettingsImport::Unchanged);
    assert!(again.summary().is_noop());
}

#[test]
fn settings_alone_are_applied_into_an_empty_database() {
    let mut source = Env::new();
    source.store.set_day_start_hour(2).unwrap();
    let mut target = Env::new();
    let plan = target.plan(&source.export()).unwrap();
    assert!(!plan.summary().is_noop());
    target.store.apply_import_at(&plan, T_EXPORT).unwrap();
    assert_eq!(target.store.settings().unwrap().day_start_hour, 2);
    assert_eq!(target.backups(), 1);
}

#[test]
fn a_database_with_decks_keeps_its_own_settings() {
    let (mut source, _, _) = sample_env();
    set_all(&mut source.store);
    let mut target = Env::new();
    // A deleted deck counts: the database is not new.
    let deck = target.store.create_deck("Kept").unwrap().id;
    target.store.delete_deck(&deck).unwrap();
    target.store.set_max_new_per_day(9).unwrap();
    let plan = target.plan(&source.export()).unwrap();
    assert_eq!(plan.summary().settings, SettingsImport::Kept);
    let report = target.store.apply_import_at(&plan, T_EXPORT).unwrap();
    assert!(!report.settings_applied);
    let settings = target.store.settings().unwrap();
    assert_eq!(
        (settings.max_new_per_day, settings.desired_retention),
        (9, 0.90)
    );
}

/// One rule whatever the file holds: applied, the database ends with
/// the file's settings. A file without settings had all defaults.
#[test]
fn a_file_without_settings_brings_the_defaults_into_an_empty_database() {
    let (source, _, _) = sample_env();
    let mut target = Env::new();
    target.store.set_max_new_per_day(3).unwrap();
    let plan = target.plan(&source.export()).unwrap();
    assert_eq!(plan.summary().settings, SettingsImport::Applied);
    target.store.apply_import_at(&plan, T_EXPORT).unwrap();
    assert_eq!(
        target.store.settings().unwrap(),
        crate::domain::Settings::default()
    );
    assert_eq!(json_of(&target.export())["settings"], json!({}));

    // With decks, the same file leaves them alone.
    let mut with_decks = Env::new();
    with_decks.store.create_deck("Mine").unwrap();
    with_decks.store.set_max_new_per_day(3).unwrap();
    let plan = with_decks.plan(&source.export()).unwrap();
    assert_eq!(plan.summary().settings, SettingsImport::Kept);
    with_decks.store.apply_import_at(&plan, T_EXPORT).unwrap();
    assert_eq!(with_decks.store.settings().unwrap().max_new_per_day, 3);
}

#[test]
fn an_import_schedules_with_the_stored_retention() {
    let (source, _, card) = sample_env();
    let mut target = Env::new();
    // A deck of its own, so the database keeps its settings.
    target.store.create_deck("Mine").unwrap();
    target.store.set_desired_retention(0.80).unwrap();
    // On the worker's own connection, as the app imports.
    let mut worker = target.concurrent();
    let plan = worker
        .plan_import_bytes(&source.export(), &Limits::DEFAULT)
        .unwrap();
    worker.apply_import_at(&plan, T_EXPORT).unwrap();

    let events = target.store.review_events(&card).unwrap();
    let at_80 = Scheduler::new(0.80)
        .unwrap()
        .rebuild_state(&card, &events)
        .unwrap();
    assert_ne!(
        at_80,
        Scheduler::default().rebuild_state(&card, &events).unwrap(),
        "the test needs the retentions to differ"
    );
    assert_eq!(target.store.card_state(&card).unwrap(), at_80);
}

/// A deck created between plan and apply turns "applied" into "kept":
/// the plan is stale.
#[test]
fn a_plan_whose_database_got_a_deck_meanwhile_is_stale() {
    let mut source = Env::new();
    source.store.set_day_start_hour(2).unwrap();
    let mut target = Env::new();
    let plan = target.plan(&source.export()).unwrap();
    target.store.create_deck("Meanwhile").unwrap();
    assert!(matches!(
        target.store.apply_import_at(&plan, T_EXPORT),
        Err(Error::ImportPlanStale)
    ));
    assert_eq!(target.store.settings().unwrap().day_start_hour, 4);
}

#[test]
fn settings_out_of_range_or_unknown_are_refused() {
    let base = json_of(&Env::new().export());
    let top = |field| ImportProblem::InvalidTopLevel {
        field,
        reason: FieldProblem::OutOfRange,
    };
    for (settings, expected) in [
        (
            json!({"desired_retention": 0.5}),
            top("settings.desired_retention"),
        ),
        (
            json!({"max_new_per_day": 10_000}),
            top("settings.max_new_per_day"),
        ),
        (
            json!({"max_reviews_per_day": 10_000}),
            top("settings.max_reviews_per_day"),
        ),
        (
            json!({"day_start_hour": 24}),
            top("settings.day_start_hour"),
        ),
        // Hostile values (ADR 0050): too large for an f32 (infinity once
        // read), too small (zero), and the limits' edges.
        (
            json!({"desired_retention": 1e39}),
            top("settings.desired_retention"),
        ),
        (
            json!({"desired_retention": -1e39}),
            top("settings.desired_retention"),
        ),
        (
            json!({"desired_retention": 1e-50}),
            top("settings.desired_retention"),
        ),
        (
            json!({"max_new_per_day": u32::MAX}),
            top("settings.max_new_per_day"),
        ),
        (
            json!({"day_start_hour": 255}),
            top("settings.day_start_hour"),
        ),
    ] {
        let mut doc = base.clone();
        doc["settings"] = settings.clone();
        assert_eq!(
            refused(Env::new().plan(&archive_of(&doc))),
            expected,
            "{settings}"
        );
    }
    for settings in [
        json!({"theme": "dark"}),
        json!({"desired_retention": "0.9"}),
        json!({"day_start_hour": -1}),
        json!({"max_new_per_day": 1.5}),
        // Wrong types and sizes: never a setting, always a JSON error.
        json!({"max_new_per_day": u64::MAX}),
        json!({"max_reviews_per_day": -1}),
        json!({"day_start_hour": 256}),
        json!({"day_start_hour": "4"}),
        json!({"desired_retention": [0.9]}),
        json!({"desired_retention": {"value": 0.9}}),
        json!({"desired_retention": true}),
        json!("settings"),
    ] {
        let mut doc = base.clone();
        doc["settings"] = settings.clone();
        assert!(
            matches!(
                refused(Env::new().plan(&archive_of(&doc))),
                ImportProblem::Json(_)
            ),
            "{settings}"
        );
    }
}
