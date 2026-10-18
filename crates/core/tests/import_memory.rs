//! The memory an import needs at its peak, measured (ADR 0025, ADR 0053).
//!
//! Each measurement runs in a process of its own: this test binary again,
//! with `RAPPEL_MEMORY_CASE` saying what to measure. The peak is the
//! process's (`VmHWM` in `/proc/self/status`), so it is that case's alone:
//! no other test runs beside it, and no memory an earlier case left with
//! the allocator hides part of it. Linux only.
//!
//! An import streams the file and keeps compact notes of the rows it has
//! read, so its memory grows with the number of rows, the most per byte
//! with the smallest rows. Each kind of small row is measured at two
//! sizes, and the line through them is extrapolated to the limit (256 MiB
//! of JSON), where the budget is 200 MiB. An archive that declares more
//! than the limits is refused from its end record, and costs nothing.

#![cfg(target_os = "linux")]

// In a test module, so its helpers may unwrap and print as tests do.
#[cfg(test)]
mod measure {
    use std::fs::{self, File};
    use std::io::{self, BufWriter, Write};
    use std::path::Path;
    use std::process::{Command, Stdio};

    use rappel_core::storage::Store;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    /// The JSON size limit of an import (`Limits::DEFAULT.json`).
    const JSON_LIMIT: u64 = 256 << 20;
    /// The memory budget at that limit.
    const BUDGET: u64 = 200 << 20;
    /// The budget for an archive refused from its end record.
    const REFUSED_BUDGET: u64 = 8 << 20;
    /// What the child process reads to know what to measure.
    const CASE: &str = "RAPPEL_MEMORY_CASE";

    /// A valid lowercase `UUIDv7` for number `n`.
    fn id(n: u64) -> String {
        format!("01920000-0000-7000-8000-{n:012x}")
    }

    /// What fills a test file.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Fill {
        /// Review events.
        Reviews,
        /// Pairs of a reset and the undo that cancels it (ADR 0028).
        ResetUndo,
        /// Card tags over `TAGS` tags (ADR 0029): the smallest rows.
        CardTags,
        /// Tags: small rows with two notes each (id and name).
        Tags,
        /// Tags whose second half repeats the names of the first: a
        /// refused file whose repeats are confirmed by a second read.
        DuplicateTags,
        /// Live decks.
        Decks,
        /// Cards with long fronts: few, large strings.
        LongCards,
    }

    /// How many tags a `Fill::CardTags` file has.
    const TAGS: u64 = 1_000;
    /// How many cards a file has, unless it is filled with cards.
    const CARDS: u64 = 5_000;

    /// One measurement: what fills the file, how it is stored, whether the
    /// plan is also applied, and the two sizes of JSON measured.
    #[derive(Debug, Clone, Copy)]
    struct Case {
        what: &'static str,
        fill: Fill,
        stored: bool,
        apply: bool,
        sizes: [u64; 2],
    }

    fn cases() -> Vec<Case> {
        let plan = [JSON_LIMIT / 32, JSON_LIMIT / 16];
        let case = |what, fill, stored| Case {
            what,
            fill,
            stored,
            apply: false,
            sizes: plan,
        };
        vec![
            case("review events, stored", Fill::Reviews, true),
            case("resets and undos, stored", Fill::ResetUndo, true),
            case("card tags, stored", Fill::CardTags, true),
            case("tags, stored", Fill::Tags, true),
            case(
                "repeated tag names (refused), stored",
                Fill::DuplicateTags,
                true,
            ),
            case("decks, stored", Fill::Decks, true),
            case("review events, deflated", Fill::Reviews, false),
            case("long cards, deflated", Fill::LongCards, false),
            // Applying writes every row: smaller files, to keep it quick.
            Case {
                what: "resets and undos, planned and applied",
                fill: Fill::ResetUndo,
                stored: true,
                apply: true,
                sizes: [JSON_LIMIT / 64, JSON_LIMIT / 32],
            },
            Case {
                what: "tags, planned and applied",
                fill: Fill::Tags,
                stored: true,
                apply: true,
                sizes: [JSON_LIMIT / 64, JSON_LIMIT / 32],
            },
        ]
    }

    /// A writer that counts what goes through it.
    struct Counting<'a, W: Write>(&'a mut W, u64);

    impl<W: Write> Write for Counting<'_, W> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let n = self.0.write(buf)?;
            self.1 += n as u64;
            Ok(n)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    /// Writes rows with `row(n, comma)` until the JSON is `json_bytes` long.
    fn fill_until<W: Write>(
        json: &mut Counting<'_, W>,
        json_bytes: u64,
        mut row: impl FnMut(&mut Counting<'_, W>, u64, &str) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut n = 0;
        while json.1 < json_bytes {
            row(json, n, if n == 0 { "" } else { "," })?;
            n += 1;
        }
        Ok(())
    }

    /// Writes a `.rappel` file whose `cards.json` is about `json_bytes` long,
    /// filled with `fill`. Streamed, so building it costs no memory to speak
    /// of. Returns the JSON's length.
    fn write_export(
        path: &Path,
        json_bytes: u64,
        fill: Fill,
        stored: bool,
    ) -> zip::result::ZipResult<u64> {
        const T: i64 = 1_792_067_696_789;
        // Ids: the deck 0, cards from 1, tags after the cards, the rest after.
        let tag = |t: u64| id(1_000_000_000 + t);
        let deck = id(0);
        let mut zip = ZipWriter::new(BufWriter::new(File::create(path)?));
        let method = if stored {
            CompressionMethod::Stored
        } else {
            CompressionMethod::Deflated
        };
        zip.start_file(
            "cards.json",
            SimpleFileOptions::DEFAULT.compression_method(method),
        )?;
        let mut json = Counting(&mut zip, 0);
        write!(
            json,
            r#"{{"format_version":2,"exported_at":{T},"decks":[{{"id":"{deck}","name":"Deck","created_at":{T},"updated_at":{T},"deleted_at":null}}"#
        )?;
        if fill == Fill::Decks {
            fill_until(&mut json, json_bytes, |json, n, _| {
                write!(
                    json,
                    r#",{{"id":"{}","name":"d{n}","created_at":{T},"updated_at":{T},"deleted_at":null}}"#,
                    id(2_000_000_000 + n)
                )
            })?;
        }
        json.write_all(br#"],"cards":["#)?;
        let front = if fill == Fill::LongCards {
            "x".repeat(8_000)
        } else {
            "q".to_owned()
        };
        let cards = if fill == Fill::LongCards {
            json_bytes / 8_100
        } else {
            CARDS
        };
        for c in 1..=cards {
            let comma = if c == 1 { "" } else { "," };
            write!(
                json,
                r#"{comma}{{"id":"{}","deck_id":"{deck}","front_md":"{front}","back_md":"","created_at":{T},"updated_at":{T},"deleted_at":null}}"#,
                id(c)
            )?;
        }
        json.write_all(br#"],"tags":["#)?;
        match fill {
            Fill::CardTags => {
                for t in 0..TAGS {
                    let comma = if t == 0 { "" } else { "," };
                    write!(json, r#"{comma}{{"id":"{}","name":"tag {t}"}}"#, tag(t))?;
                }
            }
            Fill::Tags => fill_until(&mut json, json_bytes, |json, n, comma| {
                write!(json, r#"{comma}{{"id":"{}","name":"t{n}"}}"#, tag(n))
            })?,
            Fill::DuplicateTags => {
                // The names of the first half, again, under other ids.
                let mut half = 0;
                fill_until(&mut json, json_bytes / 2, |json, n, comma| {
                    half = n + 1;
                    write!(json, r#"{comma}{{"id":"{}","name":"t{n:08}"}}"#, tag(n))
                })?;
                for n in 0..half {
                    write!(json, r#",{{"id":"{}","name":"t{n:08}"}}"#, tag(half + n))?;
                }
            }
            _ => {}
        }
        json.write_all(br#"],"card_tags":["#)?;
        if fill == Fill::CardTags {
            // By card, then tag, as an export writes them.
            'cards: for c in 1..=cards {
                for t in 0..TAGS {
                    if json.1 >= json_bytes {
                        break 'cards;
                    }
                    let comma = if c == 1 && t == 0 { "" } else { "," };
                    write!(
                        json,
                        r#"{comma}{{"card_id":"{}","tag_id":"{}"}}"#,
                        id(c),
                        tag(t)
                    )?;
                }
            }
        }
        json.write_all(br#"],"review_events":["#)?;
        write_events(&mut json, json_bytes, fill, cards)?;
        json.write_all(br#"],"settings":{},"media":[]}"#)?;
        let len = json.1;
        zip.add_directory("media/", SimpleFileOptions::DEFAULT)?;
        zip.finish()?.flush()?;
        Ok(len)
    }

    /// The log of a test file: reviews, or resets and the undos that
    /// cancel them, until the JSON is `json_bytes` long.
    fn write_events<W: Write>(
        json: &mut Counting<'_, W>,
        json_bytes: u64,
        fill: Fill,
        cards: u64,
    ) -> io::Result<()> {
        const T: i64 = 1_792_067_696_789;
        let first_event = 3_000_000_000;
        match fill {
            Fill::Reviews | Fill::LongCards => {
                fill_until(json, json_bytes, |json, n, comma| {
                    let card = id(1 + n % cards);
                    let at = T + i64::try_from(n).map_err(io::Error::other)?;
                    write!(
                        json,
                        r#"{comma}{{"id":"{}","card_id":"{card}","kind":"review","reviewed_at":{at},"grade":"pass","duration_ms":1200,"undoes":null}}"#,
                        id(first_event + n)
                    )
                })?;
            }
            Fill::ResetUndo => fill_until(json, json_bytes, |json, n, comma| {
                let card = id(1 + n % cards);
                let at = T + i64::try_from(n).map_err(io::Error::other)?;
                let (reset, undo) = (id(first_event + 2 * n), id(first_event + 2 * n + 1));
                write!(
                    json,
                    r#"{comma}{{"id":"{reset}","card_id":"{card}","kind":"reset","reviewed_at":{at},"grade":null,"duration_ms":0,"undoes":null}},{{"id":"{undo}","card_id":"{card}","kind":"undo","reviewed_at":{at},"grade":null,"duration_ms":0,"undoes":"{reset}"}}"#
                )
            })?,
            Fill::CardTags | Fill::Tags | Fill::DuplicateTags | Fill::Decks => {}
        }
        Ok(())
    }

    /// A field of `/proc/self/status`, in bytes.
    fn status_bytes(field: &str) -> io::Result<u64> {
        let status = fs::read_to_string("/proc/self/status")?;
        let kib = status
            .lines()
            .find_map(|l| l.strip_prefix(field))
            .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
            .ok_or_else(|| io::Error::other(format!("no {field} in /proc/self/status")))?;
        Ok(kib * 1024)
    }

    /// The peak memory of `f` above what the process held before it, in
    /// bytes: the peak resident set is reset to the current one first.
    fn peak_of<T>(f: impl FnOnce() -> T) -> io::Result<(T, u64)> {
        // Resets VmHWM to the current resident size (proc(5)). Fails in some
        // sandboxes; the test then fails rather than measuring nothing.
        fs::write("/proc/self/clear_refs", "5")?;
        let before = status_bytes("VmRSS:")?;
        let value = f();
        Ok((value, status_bytes("VmHWM:")?.saturating_sub(before)))
    }

    /// The measurement a child process makes, as `RAPPEL_MEMORY_CASE` gives
    /// it: a case and a size (`3:16777216`), or `refused:<name>`. Prints
    /// `PEAK <bytes> <JSON bytes>`.
    fn measure(spec: &str) {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = Store::open(&tmp.path().join("data")).unwrap();
        let path = tmp.path().join("big.rappel");
        if let Some(name) = spec.strip_prefix("refused:") {
            write_refused(&path, name);
            let (result, peak) = peak_of(|| store.plan_import(&path)).unwrap();
            let err = result.expect_err("the archive is refused");
            assert!(err.to_string().contains("cannot import"), "{err}");
            if name == "hundred-thousand-entries" {
                // Read through its zip64 end record, up to the names.
                assert!(err.to_string().contains("unexpected entry"), "{err}");
            }
            println!("PEAK {peak} 0");
            return;
        }
        let (index, size) = spec.split_once(':').unwrap();
        let case = cases()[index.parse::<usize>().unwrap()];
        let json = write_export(&path, size.parse().unwrap(), case.fill, case.stored).unwrap();
        if case.fill == Fill::DuplicateTags {
            let (result, peak) = peak_of(|| store.plan_import(&path)).unwrap();
            let err = result.expect_err("repeated names are refused");
            assert!(err.to_string().contains("same name"), "{err}");
            println!("PEAK {peak} {json}");
            return;
        }
        let (summary, peak) = peak_of(|| {
            let plan = store.plan_import(&path).unwrap();
            if case.apply {
                store.apply_import(&plan).unwrap();
            }
            plan.summary().clone()
        })
        .unwrap();
        assert!(
            summary.decks.new >= 1 && summary.conflicts.is_empty(),
            "{summary:?}"
        );
        println!("PEAK {peak} {json}");
    }

    /// Hostile archives, each refused while reading only what its last end
    /// record declares, and measured as they are: a billion entries
    /// declared; a list of entries declared nearly 4 GiB long; a file over
    /// 2 GiB; 100,000 entries (a zip64 end record, read up to the first
    /// name Rappel does not write); central records with 64 KiB comments
    /// behind a directory size that lies; 400 end records after a damaged
    /// directory; a zip64 end record with a million entries behind a
    /// classic one that names nothing; and `cards.json` nested 16 million
    /// levels deep inside a field another version would have.
    const REFUSED: [&str; 8] = [
        "billion-entries",
        "huge-directory",
        "over-two-gib",
        "hundred-thousand-entries",
        "long-comments",
        "many-end-records",
        "hidden-zip64",
        "deep-nesting",
    ];

    /// A local header and its data for `name` (stored).
    fn local(name: &[u8], data: &[u8]) -> Vec<u8> {
        let mut out = b"PK\x03\x04".to_vec();
        out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0x21, 0]);
        out.extend_from_slice(&crc32fast::hash(data).to_le_bytes());
        out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        out.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(data);
        out
    }

    /// A central record for `name` with a comment of `comment` bytes.
    fn central(name: &[u8], comment: usize) -> Vec<u8> {
        let mut out = b"PK\x01\x02".to_vec();
        out.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0x21, 0]);
        out.extend_from_slice(&crc32fast::hash(b"{}").to_le_bytes());
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&u16::try_from(comment).unwrap().to_le_bytes());
        out.extend_from_slice(&[0; 12]);
        out.extend_from_slice(name);
        out.extend(std::iter::repeat_n(0xb0, comment));
        out
    }

    /// A classic end record.
    fn end(entries: u16, size: u32, offset: u32) -> Vec<u8> {
        let mut out = b"PK\x05\x06\0\0\0\0".to_vec();
        out.extend_from_slice(&entries.to_le_bytes());
        out.extend_from_slice(&entries.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    /// The crafted archives of [`REFUSED`] that are not changed exports.
    fn crafted(name: &str) -> Option<Vec<u8>> {
        let mut bytes = local(b"cards.json", b"{}");
        let offset = u32::try_from(bytes.len()).unwrap();
        match name {
            "long-comments" => {
                for i in 0..1_000 {
                    bytes.extend(central(format!("x{i:07}").as_bytes(), 65_535));
                }
                bytes.extend(end(1_000, 1_000, offset));
            }
            "many-end-records" => {
                let record = central(b"", 0);
                for _ in 0..65_533 {
                    bytes.extend_from_slice(&record);
                }
                bytes.extend_from_slice(b"XXXX");
                bytes.extend_from_slice(&[0; 42]);
                for _ in 0..400 {
                    bytes.extend(end(65_534, 1_000, offset));
                }
            }
            "hidden-zip64" => {
                let records: u64 = 1_000_000;
                let record = central(b"", 0);
                for _ in 0..records {
                    bytes.extend_from_slice(&record);
                }
                let size = bytes.len() as u64 - u64::from(offset);
                let zip64_at = bytes.len() as u64;
                bytes.extend_from_slice(b"PK\x06\x06");
                bytes.extend_from_slice(&44u64.to_le_bytes());
                bytes.extend_from_slice(&[45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
                for value in [records, records, size, u64::from(offset)] {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                bytes.extend_from_slice(b"PK\x06\x07\0\0\0\0");
                bytes.extend_from_slice(&zip64_at.to_le_bytes());
                bytes.extend_from_slice(&1u32.to_le_bytes());
                bytes.extend(end(u16::MAX, u32::MAX, u32::MAX));
                let classic = u32::try_from(bytes.len()).unwrap();
                bytes.extend_from_slice(&[0; 64]);
                bytes.extend(end(2, 100, classic));
            }
            _ => return None,
        }
        Some(bytes)
    }

    fn write_refused(path: &Path, name: &str) {
        if let Some(bytes) = crafted(name) {
            fs::write(path, bytes).unwrap();
            return;
        }
        if name == "deep-nesting" {
            let mut zip = ZipWriter::new(BufWriter::new(File::create(path).unwrap()));
            let stored = SimpleFileOptions::DEFAULT.compression_method(CompressionMethod::Stored);
            zip.start_file("cards.json", stored).unwrap();
            zip.write_all(br#"{"format_version":3,"folders":"#).unwrap();
            let open = vec![b'['; 1 << 20];
            for _ in 0..16 {
                zip.write_all(&open).unwrap();
            }
            zip.add_directory("media/", SimpleFileOptions::DEFAULT)
                .unwrap();
            zip.finish().unwrap().flush().unwrap();
            return;
        }
        if name == "over-two-gib" {
            // Sparse: it takes no space.
            File::create(path).unwrap().set_len((2 << 30) + 1).unwrap();
            return;
        }
        if name == "hundred-thousand-entries" {
            let mut zip = ZipWriter::new(BufWriter::new(File::create(path).unwrap()));
            zip.start_file("cards.json", SimpleFileOptions::DEFAULT)
                .unwrap();
            zip.write_all(b"{}").unwrap();
            zip.add_directory("media/", SimpleFileOptions::DEFAULT)
                .unwrap();
            let stored = SimpleFileOptions::DEFAULT.compression_method(CompressionMethod::Stored);
            for n in 2..100_000 {
                // A media name as an export will write it.
                let name = format!("media/{n:064x}.webp");
                zip.start_file(name, stored).unwrap();
            }
            zip.finish().unwrap().flush().unwrap();
            return;
        }
        let mut zip = ZipWriter::new(io::Cursor::new(Vec::new()));
        zip.start_file("cards.json", SimpleFileOptions::DEFAULT)
            .unwrap();
        zip.write_all(b"{}").unwrap();
        zip.add_directory("media/", SimpleFileOptions::DEFAULT)
            .unwrap();
        let mut bytes = zip.finish().unwrap().into_inner();
        let n = bytes.len();
        if name == "billion-entries" {
            // A zip64 end record declaring a billion entries before the
            // classic one, which says "see zip64".
            let eocd = bytes.split_off(n - 22);
            let record_at = bytes.len() as u64;
            let mut record = b"PK\x06\x06".to_vec();
            record.extend_from_slice(&44u64.to_le_bytes());
            record.extend_from_slice(&[45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            record.extend_from_slice(&1_000_000_000u64.to_le_bytes());
            record.extend_from_slice(&1_000_000_000u64.to_le_bytes());
            record.extend_from_slice(
                &u64::from(u32::from_le_bytes(eocd[12..16].try_into().unwrap())).to_le_bytes(),
            );
            record.extend_from_slice(
                &u64::from(u32::from_le_bytes(eocd[16..20].try_into().unwrap())).to_le_bytes(),
            );
            record.extend_from_slice(b"PK\x06\x07\0\0\0\0");
            record.extend_from_slice(&record_at.to_le_bytes());
            record.extend_from_slice(&1u32.to_le_bytes());
            bytes.extend_from_slice(&record);
            let mut eocd = eocd;
            eocd[8..12].copy_from_slice(&[0xff; 4]);
            bytes.extend_from_slice(&eocd);
        } else {
            // A list of entries declared nearly 4 GiB long.
            bytes[n - 10..n - 6].copy_from_slice(&(u32::MAX - 1).to_le_bytes());
        }
        fs::write(path, bytes).unwrap();
    }

    /// Runs this test binary again for `spec`; the peak and the JSON size.
    fn spawn(spec: String) -> std::process::Child {
        let exe = std::env::current_exe().expect("the test binary has a path");
        Command::new(exe)
            .args([
                "--exact",
                "measure::an_import_stays_within_the_memory_budget",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CASE, spec)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the test binary runs again")
    }

    fn result_of(child: std::process::Child, spec: &str) -> (u64, u64) {
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{spec} failed ({}):\n{stdout}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        let line = stdout
            .lines()
            .find_map(|l| l.split_once("PEAK ").map(|(_, rest)| rest))
            .unwrap_or_else(|| panic!("{spec} printed no peak:\n{stdout}"));
        let (peak, json) = line.split_once(' ').unwrap();
        (peak.parse().unwrap(), json.parse().unwrap())
    }

    #[test]
    fn an_import_stays_within_the_memory_budget() {
        if let Some(spec) = std::env::var_os(CASE) {
            measure(&spec.to_string_lossy());
            return;
        }
        let cases = cases();
        // Every measurement in a process of its own, all at once.
        let mut children = Vec::new();
        for (i, case) in cases.iter().enumerate() {
            for size in case.sizes {
                let spec = format!("{i}:{size}");
                children.push((i, spec.clone(), spawn(spec)));
            }
        }
        let refused: Vec<_> = REFUSED
            .iter()
            .map(|name| {
                let spec = format!("refused:{name}");
                (spec.clone(), spawn(spec))
            })
            .collect();
        let mut points: Vec<Vec<(u64, u64)>> = vec![Vec::new(); cases.len()];
        for (i, spec, child) in children {
            let (peak, json) = result_of(child, &spec);
            points[i].push((json, peak));
        }
        for (case, points) in cases.iter().zip(&points) {
            let [(x1, p1), (x2, p2)] = [points[0], points[1]];
            // The line through both points, at the limit; a peak that did not
            // grow counts as flat.
            let growth = p2.saturating_sub(p1);
            let at_limit = p2 + growth * (JSON_LIMIT - x2) / (x2 - x1);
            eprintln!(
                "{}: {} MiB JSON peak {} MiB, {} MiB JSON peak {} MiB \
                 ({} bytes per 100 JSON bytes), {} MiB at the limit",
                case.what,
                x1 >> 20,
                p1 >> 20,
                x2 >> 20,
                p2 >> 20,
                growth * 100 / (x2 - x1),
                at_limit >> 20
            );
            assert!(
                at_limit <= BUDGET,
                "{}: {} MiB at the limit is over the {} MiB budget",
                case.what,
                at_limit >> 20,
                BUDGET >> 20
            );
        }
        for (spec, child) in refused {
            let (peak, _) = result_of(child, &spec);
            eprintln!("{spec}: refused, peak {} KiB", peak >> 10);
            assert!(peak <= REFUSED_BUDGET, "{spec}: {} KiB", peak >> 10);
        }
    }
}
