//! Format.

use super::*;

#[test]
fn the_fixture_round_trips_to_the_same_text() {
    let mut env = Env::new();
    let report = env
        .import(&write_archive(FIXTURE.as_bytes()).unwrap())
        .unwrap();
    assert_eq!(
        (
            report.decks,
            report.cards,
            report.tags,
            report.card_tags,
            report.review_events,
            report.settings_applied
        ),
        (2, 2, 2, 2, 4, true)
    );
    let json = read_cards_json(&env.export(), &Limits::DEFAULT).unwrap();
    assert_eq!(String::from_utf8(json).unwrap(), FIXTURE);
    // The live card got its schedule (its reset is undone), the deleted
    // one too, with the file's retention.
    assert_eq!(schedule_table(&env.store).len(), 2);
    let card = "01920000-0000-7000-8000-000000000003";
    let events = env.store.review_events(card).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(
        env.store.card_state(card).unwrap(),
        Scheduler::new(0.85)
            .unwrap()
            .rebuild_state(card, &events[..1])
            .unwrap()
    );
    let settings = env.store.settings().unwrap();
    assert_eq!(
        (
            settings.desired_retention,
            settings.max_new_per_day,
            settings.max_reviews_per_day,
            settings.day_start_hour
        ),
        (0.85, 10, 200, 5)
    );
    let tags: Vec<String> = env
        .store
        .tags()
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(tags, ["Algebra", "Unused"]);
    assert_eq!(env.store.card_tags(card).unwrap().len(), 1);
    // Deleted rows stay hidden.
    let decks = env.store.list_decks(T_EXPORT, T_EXPORT).unwrap();
    assert_eq!(decks.len(), 1);
    assert_eq!(decks[0].deck.name, "Maths");
}

#[test]
fn an_export_is_a_zip_with_exactly_cards_json_and_media() {
    let (env, _, _) = sample_env();
    let bytes = env.export();
    let mut zip = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
    let names: Vec<String> = zip.file_names().map(|n| n.unwrap().into_owned()).collect();
    assert_eq!(names, ["cards.json", "media/"]);
    assert!(zip.by_index(1).unwrap().is_dir());
    assert_eq!(
        zip.by_index(0).unwrap().compression(),
        CompressionMethod::Deflated
    );
    let doc = json_of(&bytes);
    assert_eq!(doc["format_version"], 2);
    assert_eq!(doc["exported_at"], T_EXPORT);
    assert_eq!(doc["decks"].as_array().unwrap().len(), 2);
    assert_eq!(doc["cards"].as_array().unwrap().len(), 4);
    assert_eq!(doc["tags"].as_array().unwrap().len(), 3);
    assert_eq!(doc["card_tags"].as_array().unwrap().len(), 3);
    assert_eq!(doc["review_events"].as_array().unwrap().len(), 8);
    assert_eq!(doc["settings"], json!({}));
    assert_eq!(doc["media"], json!([]));
    // card_tags are sorted by card, then tag.
    let pairs: Vec<(&str, &str)> = doc["card_tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["card_id"].as_str().unwrap(),
                r["tag_id"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(pairs.is_sorted());
    // Lists are sorted by id.
    for table in ["decks", "cards", "tags", "review_events"] {
        let ids: Vec<&str> = doc[table]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert!(ids.is_sorted(), "{table}");
    }
}

#[test]
fn the_same_database_gives_the_same_bytes() {
    let (env, _, _) = sample_env();
    assert_eq!(env.export(), env.export());
    // Only the export time differs between two exports.
    let later = env.store.export_bytes_at(T_EXPORT + 1).unwrap().0;
    let (mut a, b) = (json_of(&env.export()), json_of(&later));
    a["exported_at"] = b["exported_at"].clone();
    assert_eq!(a, b);
}

#[test]
fn export_to_writes_a_private_file_that_plan_import_reads() {
    let (env, _, _) = sample_env();
    let path = env.tmp.path().join("all.rappel");
    fs::write(&path, b"old").unwrap();
    let summary = env.store.export_to(&path).unwrap();
    assert_eq!(
        summary,
        ExportSummary {
            decks: 2,
            cards: 4,
            tags: 3,
            review_events: 8,
            media: 0,
            missing_media: 0,
        }
    );
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert!(!partial(&path).exists());
    let plan = Env::new().store.plan_import(&path).unwrap();
    assert_eq!(plan.summary().cards.new, 4);
}

fn partial(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".partial");
    PathBuf::from(name)
}

#[test]
fn a_leftover_partial_file_does_not_weaken_the_export() {
    let (env, _, _) = sample_env();
    let path = env.tmp.path().join("all.rappel");
    let elsewhere = env.tmp.path().join("elsewhere");
    fs::write(&elsewhere, b"keep").unwrap();
    std::os::unix::fs::symlink(&elsewhere, partial(&path)).unwrap();
    env.store.export_to(&path).unwrap();
    assert_eq!(fs::read(&elsewhere).unwrap(), b"keep");
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn a_failed_export_leaves_the_target_alone() {
    let (env, _, _) = sample_env();
    let path = env.tmp.path().join("missing-dir").join("all.rappel");
    assert!(matches!(env.store.export_to(&path), Err(Error::Io { .. })));
    assert!(!path.exists());
}

/// Format version 1 was written only by early test versions (0.2.0-alpha,
/// and the `.cards` files from before the rename): it is refused with its
/// own message, before anything else in the file is looked at, and
/// nothing is written (ADR 0017).
#[test]
fn files_of_early_test_versions_are_refused() {
    let env = Env::new();
    let v1 = write_archive(FIXTURE_V1.as_bytes()).unwrap();
    assert_eq!(refused(env.plan(&v1)), ImportProblem::EarlyTestVersion);
    // Even when the rest would not be a valid version 2 file either.
    let mut doc: Value = serde_json::from_str(FIXTURE).unwrap();
    doc["format_version"] = json!(1);
    assert_eq!(
        refused(env.plan(&archive_of(&doc))),
        ImportProblem::EarlyTestVersion
    );
    assert_eq!(
        ImportProblem::EarlyTestVersion.to_string(),
        "the file was made by an early test version of Rappel and cannot be imported"
    );
    assert_eq!(env.store.decks().unwrap(), []);
    assert_eq!(env.backups(), 0);
}

/// The settings are written with the type of each setting: the retention
/// as the `f32` it is, so 0.85 reads `0.85`, not `0.8500000238418579`.
#[test]
fn settings_are_written_as_typed_values() {
    let mut env = Env::new();
    env.store.set_desired_retention(0.85).unwrap();
    env.store.set_max_reviews_per_day(150).unwrap();
    let text =
        String::from_utf8(read_cards_json(&env.export(), &Limits::DEFAULT).unwrap()).unwrap();
    assert!(
        text.contains(
            "  \"settings\": {\n    \"desired_retention\": 0.85,\n    \"max_reviews_per_day\": 150\n  },"
        ),
        "{text}"
    );
}
