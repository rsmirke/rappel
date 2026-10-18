//! The memory an Anki import's plan needs at its peak, against packages
//! made to make it large (ADR 0061), measured as `import_memory.rs` does:
//! each case in a process of its own (this test binary again), its peak
//! from `/proc/self/status`. Linux only.
//!
//! - `json`: a legacy collection whose note types are 64 MiB of JSON
//!   declaring 20 million fields (it deflates to about 64 KiB). Read with
//!   bounds, it is refused at the 1,001st field; read whole, it took 1.4 GB.
//! - `field`: a note whose fields are 64 MiB, over the 8 MiB a note may
//!   have. It is measured by `octet_length` and never loaded: the peak stays
//!   far under the value's size.

#![cfg(target_os = "linux")]

#[cfg(test)]
mod measure {
    use std::fs;
    use std::io::{self, Write};
    use std::path::Path;
    use std::process::Command;
    use std::sync::atomic::AtomicBool;

    use rappel_core::storage::Store;
    use rusqlite::{Connection, params};
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    const CASE: &str = "RAPPEL_ANKI_MEMORY_CASE";
    /// The size of the hostile value.
    const BIG: usize = 64 << 20;

    fn status_bytes(field: &str) -> io::Result<u64> {
        let status = fs::read_to_string("/proc/self/status")?;
        let kib = status
            .lines()
            .find_map(|l| l.strip_prefix(field))
            .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
            .ok_or_else(|| io::Error::other(format!("no {field} in /proc/self/status")))?;
        Ok(kib * 1024)
    }

    /// Writes a legacy package to `path`: a collection with one Basic note
    /// whose fields are `fields`, and `models` as its note types.
    fn write_package(path: &Path, models: &str, fields: &str) {
        let dir = path.parent().unwrap();
        let db = dir.join("collection.anki21");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE col (id integer primary key, crt, mod, scm, ver, dty, usn, ls,
                               conf, models, decks, dconf, tags);
             CREATE TABLE notes (id integer primary key, guid, mid, mod, usn, tags, flds,
                                 sfld, csum, flags, data);
             CREATE TABLE cards (id integer primary key, nid, did, ord, mod, usn, type, queue,
                                 due, ivl, factor, reps, lapses, left, odue, odid, flags, data);
             CREATE TABLE revlog (id integer primary key, cid, usn, ease, ivl, lastIvl, factor,
                                  time, type);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO col VALUES (1, 0, 0, 0, 11, 0, 0, 0, '{}', ?1, ?2, '{}', '{}')",
            params![models, r#"{"1": {"name": "Default"}}"#],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO notes VALUES (10, 'g', 1, 0, 0, '', ?1, '', 0, 0, '')",
            [fields],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards VALUES (100, 10, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, '')",
            [],
        )
        .unwrap();
        drop(conn);
        let mut zip = ZipWriter::new(fs::File::create(path).unwrap());
        let deflated = SimpleFileOptions::DEFAULT
            .compression_method(CompressionMethod::Deflated)
            .large_file(true);
        zip.start_file("collection.anki21", deflated).unwrap();
        io::copy(&mut fs::File::open(&db).unwrap(), &mut zip).unwrap();
        zip.start_file("media", deflated).unwrap();
        zip.write_all(b"{}").unwrap();
        zip.finish().unwrap();
        fs::remove_file(db).unwrap();
    }

    const BASIC: &str = r#"{"1": {"name": "Basic", "type": 0,
        "flds": [{"name": "Front", "ord": 0}, {"name": "Back", "ord": 1}],
        "tmpls": [{"name": "Card 1", "ord": 0, "qfmt": "{{Front}}", "afmt": "{{Back}}"}]}}"#;

    /// Measures one case in this process; prints `PEAK <bytes>`.
    fn measure(case: &str) {
        let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let store = Store::open(&tmp.path().join("data")).unwrap();
        let package = tmp.path().join("hostile.apkg");
        if case == "json" {
            let mut models = String::with_capacity(BIG + 64);
            models.push_str(r#"{"1": {"name": "Many", "flds": ["#);
            while models.len() < BIG - 4096 {
                models.push_str("{},");
            }
            models.push_str("{}]}}");
            write_package(&package, &models, "a\u{1f}b");
        } else {
            let mut fields = "x".repeat(BIG);
            fields.push_str("\u{1f}b");
            write_package(&package, BASIC, &fields);
        }
        fs::write("/proc/self/clear_refs", "5").unwrap();
        let before = status_bytes("VmRSS:").unwrap();
        let result = store.plan_anki_import(&package, &AtomicBool::new(false));
        let peak = status_bytes("VmHWM:").unwrap().saturating_sub(before);
        if case == "json" {
            let err = result.expect_err("too many fields are refused");
            assert!(
                err.to_string().contains("more fields or templates"),
                "{err}"
            );
        } else {
            let plan = result.unwrap();
            assert_eq!(plan.summary().cards, 0);
        }
        println!("PEAK {peak}");
    }

    fn run(case: &str) -> u64 {
        let exe = std::env::current_exe().expect("the test binary has a path");
        let output = Command::new(exe)
            .args([
                "--exact",
                "measure::an_anki_plan_stays_within_its_memory",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CASE, case)
            .output()
            .expect("the test binary runs again");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{case} failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        stdout
            .lines()
            .find_map(|l| l.split_once("PEAK ").map(|(_, p)| p))
            .and_then(|p| p.split_whitespace().next()?.parse().ok())
            .unwrap_or_else(|| panic!("{case} printed no peak:\n{stdout}"))
    }

    #[test]
    fn an_anki_plan_stays_within_its_memory() {
        if let Some(case) = std::env::var_os(CASE) {
            measure(&case.to_string_lossy());
            return;
        }
        // The JSON is read in place from SQLite (64 MiB), and what is kept
        // of it is bounded.
        let json = run("json");
        eprintln!("json: peak {} MiB", json >> 20);
        assert!(json <= 128 << 20, "json: {} MiB", json >> 20);
        // The 64 MiB value is never loaded.
        let field = run("field");
        eprintln!("field: peak {} MiB", field >> 20);
        assert!(field <= 24 << 20, "field: {} MiB", field >> 20);
    }
}
