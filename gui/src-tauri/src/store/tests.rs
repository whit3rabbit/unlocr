use super::*;
use rusqlite::Connection;

/// Fresh in-memory DB with the schema applied; tests drive `db::*` against it.
fn mem_db() -> Connection {
    crate::db::mem_db()
}

fn job(id: &str, out: &str) -> Job {
    Job {
        id: id.into(),
        input_path: format!("/tmp/{id}.pdf"),
        options: JobOptions::from_opts("Q8_0", 4096, 144, "prompt", false),
        status: "done".into(),
        output_path: out.into(),
        error: String::new(),
        created_at: 100,
        updated_at: 200,
        page_count: None,
        duration_ms: None,
        backend: String::new(),
        output_mode: String::new(),
    }
}

/// Insert one job, list returns it, every flattened JobOptions field survives
/// the round-trip (the row-to-struct mapping the frontend's wire shape relies on).
#[test]
fn job_insert_then_list_roundtrips() {
    let conn = mem_db();
    let j = job("1-s", "/tmp/x.md");
    db::insert(&conn, &j).unwrap();
    let got = db::list(&conn).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, "1-s");
    assert_eq!(got[0].input_path, "/tmp/1-s.pdf");
    assert_eq!(got[0].options.quant, "Q8_0");
    assert_eq!(got[0].options.max_tokens, 4096);
    assert_eq!(got[0].options.dpi, 144);
    assert_eq!(got[0].options.prompt, "prompt");
    assert!(!got[0].options.keep_images);
    assert_eq!(got[0].status, "done");
    assert_eq!(got[0].output_path, "/tmp/x.md");
    assert_eq!(got[0].created_at, 100);
    assert_eq!(got[0].updated_at, 200);
}

/// INSERT OR REPLACE: a second write with the same id updates, never duplicates.
#[test]
fn job_upsert_no_duplicate() {
    let conn = mem_db();
    let mut j = job("x", "/tmp/x.md");
    db::insert(&conn, &j).unwrap();
    j.status = "failed".into();
    j.error = "boom".into();
    db::insert(&conn, &j).unwrap();
    let got = db::list(&conn).unwrap();
    assert_eq!(got.len(), 1, "same id must not duplicate");
    assert_eq!(got[0].status, "failed");
    assert_eq!(got[0].error, "boom");
}

/// update_status flips the row to its terminal state, advancing updated_at,
/// WITHOUT touching created_at (the start->finish lifecycle relies on this).
#[test]
fn job_update_status_preserves_created_at() {
    let conn = mem_db();
    let mut j = job("u", "");
    j.status = "running".into();
    j.created_at = 100;
    j.updated_at = 100;
    db::insert(&conn, &j).unwrap();
    db::update_status(
        &conn,
        "u",
        "done",
        "/tmp/u.md",
        "",
        200,
        &JobMetrics::default(),
    )
    .unwrap();
    let got = db::list(&conn).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].status, "done");
    assert_eq!(got[0].output_path, "/tmp/u.md");
    assert_eq!(got[0].updated_at, 200);
    assert_eq!(got[0].created_at, 100, "created_at must survive an update");
}

/// finish_job's metrics (page count, duration, backend, output layout) persist
/// through update_status and read back on list -- the run-detail dialog's data.
#[test]
fn job_update_status_records_metrics() {
    let conn = mem_db();
    let mut j = job("m", "");
    j.status = "running".into();
    db::insert(&conn, &j).unwrap();
    let metrics = JobMetrics {
        page_count: Some(7),
        duration_ms: Some(4200),
        backend: "local".into(),
        output_mode: "pages".into(),
    };
    db::update_status(&conn, "m", "done", "/tmp/m.md", "", 300, &metrics).unwrap();
    let got = db::list(&conn).unwrap();
    assert_eq!(got[0].page_count, Some(7));
    assert_eq!(got[0].duration_ms, Some(4200));
    assert_eq!(got[0].backend, "local");
    assert_eq!(got[0].output_mode, "pages");
}

/// update_status on a missing id is a silent no-op (0 rows), not an error.
#[test]
fn job_update_status_unknown_is_noop() {
    let conn = mem_db();
    db::update_status(&conn, "nope", "done", "", "", 1, &JobMetrics::default()).unwrap();
    assert!(db::list(&conn).unwrap().is_empty());
}

/// reconcile_interrupted flips ONLY `running` rows to failed, stamps updated_at,
/// and leaves done/failed rows untouched (startup crash recovery).
#[test]
fn reconcile_flips_only_running() {
    let conn = mem_db();
    let mut r1 = job("r1", "");
    r1.status = "running".into();
    let mut r2 = job("r2", "");
    r2.status = "running".into();
    db::insert(&conn, &r1).unwrap();
    db::insert(&conn, &r2).unwrap();
    db::insert(&conn, &job("d", "/tmp/d.md")).unwrap(); // status "done"
    let n = db::reconcile_interrupted(&conn, 999).unwrap();
    assert_eq!(n, 2, "only the two running rows flip");
    let got = db::list(&conn).unwrap();
    let failed: Vec<_> = got.iter().filter(|j| j.status == "failed").collect();
    assert_eq!(failed.len(), 2);
    for f in &failed {
        assert_eq!(f.updated_at, 999);
        assert!(f.error.contains("interrupted"));
    }
    assert!(got.iter().any(|j| j.status == "done"), "done row preserved");
}

/// delete returns the removed job's output_path and actually drops the row.
#[test]
fn job_delete_returns_output_path() {
    let conn = mem_db();
    db::insert(&conn, &job("a", "/tmp/a.md")).unwrap();
    db::insert(&conn, &job("b", "")).unwrap();
    let out = db::delete(&conn, "a").unwrap();
    assert_eq!(out.as_deref(), Some("/tmp/a.md"));
    let got = db::list(&conn).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, "b");
}

/// Unknown id is a no-op: returns None, leaves the row count unchanged.
#[test]
fn job_delete_unknown_is_noop() {
    let conn = mem_db();
    db::insert(&conn, &job("a", "/tmp/a.md")).unwrap();
    assert_eq!(db::delete(&conn, "zzz").unwrap(), None);
    assert_eq!(db::list(&conn).unwrap().len(), 1);
}

/// delete_many drops only the listed ids in one statement; output_paths_for
/// returns just the (id, output_path) pairs for the requested subset (the per-id
/// file-delete path the multi-select `delete_jobs` command relies on to keep only
/// failed-file records). Empty id slices and unknown ids are no-ops.
#[test]
fn job_delete_many_and_output_paths_for() {
    let conn = mem_db();
    db::insert(&conn, &job("a", "/tmp/a.md")).unwrap();
    db::insert(&conn, &job("b", "")).unwrap();
    db::insert(&conn, &job("c", "/tmp/c.md")).unwrap();
    db::insert(&conn, &job("d", "/tmp/d.md")).unwrap();

    // Empty id slice: no-op on both.
    assert!(db::output_paths_for(&conn, &[]).unwrap().is_empty());
    db::delete_many(&conn, &[]).unwrap();
    assert_eq!(db::list(&conn).unwrap().len(), 4);

    // Subset peek: only a + c have outputs (b is empty), d is outside the subset.
    // The id is paired with its path so delete_jobs can keep failed-file records.
    let subset = ["a".to_string(), "b".to_string(), "c".to_string()];
    let outs = db::output_paths_for(&conn, &subset).unwrap();
    assert_eq!(outs.len(), 2, "only non-empty outputs of the subset");
    assert!(outs.contains(&("a".to_string(), "/tmp/a.md".to_string())));
    assert!(outs.contains(&("c".to_string(), "/tmp/c.md".to_string())));

    // Remove the subset; d survives untouched.
    db::delete_many(&conn, &subset).unwrap();
    let got = db::list(&conn).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, "d");

    // delete_many tolerates ids not present (no error, no over-delete).
    db::delete_many(&conn, &["zzz".to_string(), "d".to_string()]).unwrap();
    assert!(db::list(&conn).unwrap().is_empty());
}

/// The old 500-row cap is GONE: 501 jobs all survive. The explicit proof the
/// user asked for when dropping the JSON file limits.
#[test]
fn over_500_jobs_all_survive() {
    let conn = mem_db();
    for i in 0..501u64 {
        let j = Job {
            id: format!("{i}-x"),
            input_path: format!("/tmp/{i}.pdf"),
            options: JobOptions::from_opts("Q8_0", 4096, 144, "p", false),
            status: "done".into(),
            output_path: String::new(),
            error: String::new(),
            created_at: i,
            updated_at: i,
            page_count: None,
            duration_ms: None,
            backend: String::new(),
            output_mode: String::new(),
        };
        db::insert(&conn, &j).unwrap();
    }
    assert_eq!(db::list(&conn).unwrap().len(), 501);
}

/// `make_id` must be filesystem-safe, and two runs of the same file in the same
/// second must get DISTINCT ids (the seq disambiguator), so the second run no
/// longer upserts over the first.
#[test]
fn make_id_is_safe_and_unique() {
    let a = make_id("/some path/My Report #1.pdf", 12345);
    let b = make_id("/some path/My Report #1.pdf", 12345);
    assert_ne!(a, b, "two runs (same input+time) must get distinct ids");
    assert!(
        a.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "id must be fs-safe (ASCII): {a}"
    );
    assert!(a.starts_with("12345-"));
    assert!(
        a.contains("My_Report"),
        "stem kept, unsafe chars mapped: {a}"
    );
}

/// Non-ASCII stems must be sanitized to `_` (the old is_alphanumeric leaked CJK).
#[test]
fn make_id_sanitizes_non_ascii() {
    let id = make_id("/docs/報告.pdf", 999);
    assert!(
        id.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "non-ASCII must be mapped to '_': {id}"
    );
}

/// Two same-stem inputs from different folders in the same second must NOT
/// collide (the old <secs>-<stem> scheme overwrote one via upsert).
#[test]
fn make_id_disambiguates_same_stem_different_path() {
    let a = make_id("/a/report.pdf", 100);
    let b = make_id("/b/report.pdf", 100);
    assert_ne!(
        a, b,
        "different paths, same stem+time must differ: {a} == {b}"
    );
}

/// `JobOptions::from_opts` must echo each field verbatim.
#[test]
fn from_opts_echoes_fields() {
    let o = JobOptions::from_opts("Q4_K_M", 8192, 300, "<|grounding|>x", true);
    assert_eq!(o.quant, "Q4_K_M");
    assert_eq!(o.max_tokens, 8192);
    assert_eq!(o.dpi, 300);
    assert_eq!(o.prompt, "<|grounding|>x");
    assert!(o.keep_images);
}

/// now_secs is monotonic-ish and non-zero on a normal clock.
#[test]
fn now_secs_is_positive() {
    let n = now_secs();
    assert!(n > 1_700_000_000, "epoch seconds implausibly small: {n}");
}

/// delete_output_file refuses anything that is not a `.md` file and leaves it
/// on disk; deletes a real `.md`; and is Ok on a missing/empty path.
#[test]
fn delete_output_file_guards_and_deletes() {
    // Empty path: no-op success.
    assert!(delete_output_file("").is_ok());
    // Missing .md: idempotent success.
    assert!(delete_output_file("/no/such/file.md").is_ok());

    let dir = tempfile::tempdir().unwrap();

    // Non-.md is rejected and the file survives.
    let txt = dir.path().join("keep.txt");
    std::fs::write(&txt, b"data").unwrap();
    assert!(delete_output_file(txt.to_str().unwrap()).is_err());
    assert!(txt.exists(), "non-.md must not be deleted");

    // Real .md is deleted.
    let md = dir.path().join("out.md");
    std::fs::write(&md, b"# hi").unwrap();
    assert!(delete_output_file(md.to_str().unwrap()).is_ok());
    assert!(!md.exists(), ".md should be removed");
}
