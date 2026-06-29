use super::*;

#[test]
fn ocroptions_default_matches_cli() {
    // Defaults must mirror the CLI flags so a no-op caller matches `unlocr`.
    let o = OcrOptions::default();
    assert_eq!(o.quant, "Q8_0");
    assert_eq!(o.max_tokens, 4096);
    assert_eq!(o.dpi, 144);
    assert_eq!(o.prompt, "<|grounding|>Convert the document to markdown.");
    assert_eq!(o.port, 0);
    assert!(o.model_dir.is_none());
    assert!(!o.keep_images);
    assert!(o.pages.is_none());
}

#[test]
fn resolve_output_path_cases() {
    let dir = Path::new("/out");
    // Default: {stem}.md under out_dir.
    assert_eq!(
        resolve_output_path(dir, None, "doc"),
        Path::new("/out/doc.md")
    );
    // Relative name, no extension -> append .md, joined under out_dir.
    assert_eq!(
        resolve_output_path(dir, Some(Path::new("report")), "doc"),
        Path::new("/out/report.md")
    );
    // Relative name with .md -> preserved.
    assert_eq!(
        resolve_output_path(dir, Some(Path::new("report.md")), "doc"),
        Path::new("/out/report.md")
    );
    // Non-.md extension -> left as typed (caller's choice).
    assert_eq!(
        resolve_output_path(dir, Some(Path::new("report.txt")), "doc"),
        Path::new("/out/report.txt")
    );
    // Absolute path -> used verbatim, ignoring out_dir (ext appended when missing).
    assert_eq!(
        resolve_output_path(dir, Some(Path::new("/tmp/x")), "doc"),
        Path::new("/tmp/x.md")
    );
    assert_eq!(
        resolve_output_path(dir, Some(Path::new("/tmp/x.md")), "doc"),
        Path::new("/tmp/x.md")
    );
}

#[test]
fn push_page_assembles_delimiters() {
    let mut md = String::new();
    push_page(&mut md, 0, "first");
    push_page(&mut md, 1, "second");
    assert_eq!(
        md.trim_start(),
        "<!-- page 1 -->\n\nfirst\n\n<!-- page 2 -->\n\nsecond"
    );
}

#[test]
fn parse_output_mode_cases() {
    assert_eq!(parse_output_mode("single").unwrap(), OutputMode::Single);
    assert_eq!(parse_output_mode("PAGES").unwrap(), OutputMode::Pages);
    assert_eq!(parse_output_mode(" Both ").unwrap(), OutputMode::Both);
    assert!(parse_output_mode("bogus").is_err());
    assert_eq!(OutputMode::default(), OutputMode::Single);
}

#[test]
fn write_markdown_output_modes() {
    let tmp = tempfile::tempdir().expect("tmp");
    let out_dir = tmp.path();
    let output = OcrOutput {
        combined: "<!-- page 1 -->\n\nA\n\n<!-- page 2 -->\n\nB".to_string(),
        pages: vec![(1, "A".to_string()), (2, "B".to_string())],
        kept_images: None,
    };

    // Single: one combined file, no folder.
    let single =
        write_markdown_output(OutputMode::Single, out_dir, None, "doc", &output).expect("single");
    assert_eq!(single.len(), 1);
    let combined_path = out_dir.join("doc.md");
    assert_eq!(single[0], combined_path);
    assert_eq!(
        std::fs::read_to_string(&combined_path).unwrap(),
        output.combined
    );
    assert!(
        !out_dir.join("doc").exists(),
        "single must not create a folder"
    );

    // Pages: a folder of per-page files, no combined file. Names zero-padded so
    // page-01 sorts before page-02.
    let pages =
        write_markdown_output(OutputMode::Pages, out_dir, None, "doc", &output).expect("pages");
    assert_eq!(pages.len(), 2, "one path per page");
    let p1 = out_dir.join("doc").join("page-01.md");
    let p2 = out_dir.join("doc").join("page-02.md");
    assert_eq!(pages[0], p1);
    assert_eq!(pages[1], p2);
    assert!(
        p1.to_string_lossy() < p2.to_string_lossy(),
        "must sort lexically"
    );
    assert_eq!(std::fs::read_to_string(&p1).unwrap(), "A");
    assert_eq!(std::fs::read_to_string(&p2).unwrap(), "B");

    // Both: combined file AND the per-page folder (combined path first).
    let both =
        write_markdown_output(OutputMode::Both, out_dir, None, "doc", &output).expect("both");
    assert_eq!(both.len(), 3, "1 combined + 2 pages");
    assert_eq!(both[0], combined_path);
    assert_eq!(both[1], p1);
    assert_eq!(both[2], p2);
}

#[test]
fn write_markdown_output_zero_pads_large_page_counts() {
    // >99 pages: width grows to 3 so page-001 < page-010 < page-100 (lexical sort).
    let tmp = tempfile::tempdir().expect("tmp");
    let pages_text: Vec<(usize, String)> =
        (1..=150).map(|n| (n, format!("page {n} text"))).collect();
    let output = OcrOutput {
        combined: "combined".to_string(),
        pages: pages_text,
        kept_images: None,
    };
    let written =
        write_markdown_output(OutputMode::Pages, tmp.path(), None, "big", &output).expect("pages");
    assert_eq!(written.len(), 150);
    let names: Vec<String> = written
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    // Sorted lexicographically because zero-padded to width 3.
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "page file names must sort lexicographically");
    assert_eq!(names[0], "page-001.md");
    assert_eq!(names[9], "page-010.md");
    assert_eq!(names[99], "page-100.md");
    assert_eq!(names[149], "page-150.md");
}

#[test]
fn write_markdown_output_pages_clears_stale_pages() {
    // Re-running pages mode over the SAME folder with a SHORTER document must not
    // leave the prior run's higher-numbered pages behind (silent doc mixing).
    let tmp = tempfile::tempdir().expect("tmp");
    let out_dir = tmp.path();

    let long = OcrOutput {
        combined: "long".to_string(),
        pages: (1..=3).map(|n| (n, format!("old {n}"))).collect(),
        kept_images: None,
    };
    write_markdown_output(OutputMode::Pages, out_dir, None, "doc", &long).expect("first");
    assert!(out_dir.join("doc").join("page-03.md").exists());

    let short = OcrOutput {
        combined: "short".to_string(),
        pages: vec![(1, "new 1".to_string())],
        kept_images: None,
    };
    let written =
        write_markdown_output(OutputMode::Pages, out_dir, None, "doc", &short).expect("second");
    assert_eq!(written.len(), 1);
    assert_eq!(
        std::fs::read_to_string(out_dir.join("doc").join("page-01.md")).unwrap(),
        "new 1"
    );
    // The stale page-02/page-03 from the longer run must be gone.
    assert!(
        !out_dir.join("doc").join("page-02.md").exists(),
        "stale page-02 must be cleared"
    );
    assert!(
        !out_dir.join("doc").join("page-03.md").exists(),
        "stale page-03 must be cleared"
    );
}

#[test]
fn duplicate_stems_flags_same_stem_inputs() {
    use std::path::PathBuf;
    let inputs = [
        PathBuf::from("/a/report.pdf"),
        PathBuf::from("/b/report.pdf"),
        PathBuf::from("/c/unique.pdf"),
    ];
    assert_eq!(duplicate_stems(&inputs), vec!["report".to_string()]);
    // No collision when every stem is distinct.
    let distinct = [PathBuf::from("/a/x.pdf"), PathBuf::from("/b/y.pdf")];
    assert!(duplicate_stems(&distinct).is_empty());
}

#[test]
fn run_ocr_job_rejects_missing_file() {
    // Non-network path: run_ocr_job must fail fast on a non-existent input
    // before touching preflight/network. Locks the early validation.
    let mut progress = |_: Progress| {};
    let err = run_ocr_job(
        Path::new("/nonexistent/ferrum-bite-1.pdf"),
        None,
        &OcrOptions::default(),
        &mut progress,
    );
    assert!(err.is_err());
    let msg = err.unwrap_err().to_string();
    assert!(msg.contains("not a file"), "unexpected error: {msg}");
}

#[test]
fn preview_cache_dir_is_deterministic_and_dpi_keyed() {
    // Same (file state, dpi) -> same dir; a different dpi -> different dir.
    // No pdftoppm: this locks the cache keying that decides hit vs re-render.
    let tmp = tempfile::tempdir().expect("tmp");
    let pdf = tmp.path().join("doc.pdf");
    std::fs::write(&pdf, b"%PDF-1.4 stub").expect("write stub");
    let root = tmp.path();

    let a = preview_cache_dir(&pdf, 144, root);
    let b = preview_cache_dir(&pdf, 144, root);
    let c = preview_cache_dir(&pdf, 72, root);
    assert_eq!(a, b, "same inputs must key to the same dir");
    assert_ne!(a, c, "different dpi must key to a different dir");
    assert!(a.starts_with(root.join("previews")));
}

/// EH-0003 acceptance 2: exercise the ocr:// state sequence (ServerReady ->
/// Page 1 -> Page 2 ...) without a live desktop session.
///
/// Strategy:
///   1. Spin up a minimal HTTP stub on a free port that returns a valid
///      OpenAI-style chat-completion response for any POST request.
///   2. Create a Server::for_test(port) pointing at it (dummy child, real port).
///   3. Rasterize a two-page fixture PDF via pdftoppm (skip on hosts without it).
///   4. Run ocr_pages and capture every Progress event in order.
///   5. Assert: exactly 2 Page events, page numbers 1 then 2, total always 2.
///
/// This proves: listeners would receive events in the correct state order
/// (download events are emitted by model::ensure_with_progress before
/// run_ocr_job calls on_progress(ServerReady); the ServerReady -> Page
/// subsequence is fully exercised here).
#[test]
fn ocr_state_sequence_ordering() {
    // Skip on hosts where pdftoppm is not installed (CI without poppler, etc.).
    if std::process::Command::new("pdftoppm")
        .arg("-v")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("skipping ocr_state_sequence_ordering: pdftoppm not on PATH");
        return;
    }

    // --- 1. Stub HTTP server + 2. Server::for_test ------------------------
    let stub_port = spawn_stub_ocr_server();
    let srv = server::Server::for_test(stub_port).expect("for_test");

    // --- 3. Fixture PDF + pdftoppm ----------------------------------------
    let pdf_dir = tempfile::tempdir().expect("tmp pdf dir");
    let pdf_path = pdf_dir.path().join("fixture.pdf");
    std::fs::write(&pdf_path, two_page_pdf_bytes()).expect("write fixture pdf");

    // --- 4. Run ocr_pages + capture Progress events -----------------------
    let pdftoppm_bin = std::path::Path::new("pdftoppm");
    let opts = OcrOptions::default();
    let mut events: Vec<Progress> = Vec::new();
    let result = ocr_pages(
        &srv,
        pdftoppm_bin,
        &pdf_path,
        &opts,
        &mut |p: Progress| {
            events.push(p);
        },
        &|| false,
    );
    assert!(result.is_ok(), "ocr_pages failed: {:?}", result.err());

    // OcrOutput.pages must mirror the combined string: one entry per page,
    // each carrying the real 1-based page number (not the loop index).
    let out = result.unwrap();
    assert_eq!(
        out.pages.len(),
        2,
        "OcrOutput.pages must hold one entry per page"
    );
    assert_eq!(out.pages[0].0, 1, "first entry carries real page number 1");
    assert_eq!(out.pages[1].0, 2, "second entry carries real page number 2");

    // --- 5. Assert ordering -----------------------------------------------
    // Filter to Page events only (Download events are from model::ensure_with_progress
    // which is not called by ocr_pages; ServerReady is emitted by run_ocr_job
    // before calling ocr_pages). From ocr_pages we expect exactly 2 Page events.
    let page_events: Vec<(usize, usize)> = events
        .iter()
        .filter_map(|e| {
            if let Progress::Page { page, total } = e {
                Some((*page, *total))
            } else {
                None
            }
        })
        .collect();

    assert_eq!(
        page_events.len(),
        2,
        "expected 2 Page events, got {:?}",
        page_events
    );
    assert_eq!(
        page_events[0],
        (1, 2),
        "first event should be Page{{page:1, total:2}}"
    );
    assert_eq!(
        page_events[1],
        (2, 2),
        "second event should be Page{{page:2, total:2}}"
    );
    // ocr_pages emits Page plus PartialText (streamed OCR text). The stub
    // replies with a plain JSON completion (no SSE framing); ocr_via_stream's
    // non-streaming fallback delivers that text via on_token, so we expect one
    // PartialText per page carrying the stub's content. No other variant.
    for ev in &events {
        assert!(
            matches!(ev, Progress::Page { .. } | Progress::PartialText { .. }),
            "ocr_pages emitted unexpected event variant: {ev:?}"
        );
    }
    let partials: Vec<(usize, &str)> = events
        .iter()
        .filter_map(|e| match e {
            Progress::PartialText { page, chunk } => Some((*page, chunk.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        partials,
        vec![(1, "# page text"), (2, "# page text")],
        "expected one PartialText per page from the non-SSE fallback"
    );
}

#[test]
fn render_pages_returns_cached_pngs_without_pdftoppm() {
    // Cache-hit path: seed the keyed dir with page PNGs, then render_pages must
    // return them sorted by page number and never run pdftoppm (a bogus binary
    // path proves it is not invoked on a hit).
    let tmp = tempfile::tempdir().expect("tmp");
    let pdf = tmp.path().join("doc.pdf");
    std::fs::write(&pdf, b"%PDF-1.4 stub").expect("write stub");
    let root = tmp.path();

    let dir = preview_cache_dir(&pdf, 144, root);
    std::fs::create_dir_all(&dir).expect("mk cache dir");
    // Out of order on disk; render_pages must return them 1,2,10.
    for n in [10u32, 1, 2] {
        std::fs::write(dir.join(format!("page-{n}.png")), b"\x89PNG").expect("seed png");
    }

    let bogus = Path::new("/nonexistent/pdftoppm");
    let pages = render_pages(bogus, &pdf, 144, root).expect("cache hit");
    let names: Vec<_> = pages
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["page-1.png", "page-2.png", "page-10.png"]);
}

/// A page range OCRs only the selected pages AND labels them with the real page
/// number, not the loop index. Selecting page 2 of a 2-page PDF must produce a
/// single `<!-- page 2 -->` block (regression guard for the base+i numbering).
#[test]
fn ocr_pages_honors_page_range() {
    if std::process::Command::new("pdftoppm")
        .arg("-v")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("skipping ocr_pages_honors_page_range: pdftoppm not on PATH");
        return;
    }

    let stub_port = spawn_stub_ocr_server();
    let srv = server::Server::for_test(stub_port).expect("for_test");

    let pdf_dir = tempfile::tempdir().expect("tmp pdf dir");
    let pdf_path = pdf_dir.path().join("fixture.pdf");
    std::fs::write(&pdf_path, two_page_pdf_bytes()).expect("write fixture pdf");

    let opts = OcrOptions {
        pages: Some((2, Some(2))),
        ..OcrOptions::default()
    };
    let mut events: Vec<Progress> = Vec::new();
    let out = ocr_pages(
        &srv,
        std::path::Path::new("pdftoppm"),
        &pdf_path,
        &opts,
        &mut |p: Progress| events.push(p),
        &|| false,
    )
    .expect("ocr_pages with range");

    // Exactly one page OCR'd, and it is reported/labeled as page 2.
    let page_events: Vec<(usize, usize)> = events
        .iter()
        .filter_map(|e| match e {
            Progress::Page { page, total } => Some((*page, *total)),
            _ => None,
        })
        .collect();
    assert_eq!(
        page_events,
        vec![(2, 1)],
        "should OCR only page 2 of 1 selected"
    );
    assert!(
        out.combined.contains("<!-- page 2 -->"),
        "markdown must carry the real page number: {}",
        out.combined
    );
    assert!(
        !out.combined.contains("<!-- page 1 -->"),
        "page 1 must not be OCR'd: {}",
        out.combined
    );
    // Per-page capture mirrors the combined string: one entry carrying the real
    // page number (2), not the loop index (1). Locks the regression this test guards.
    assert_eq!(out.pages.len(), 1, "one-page range -> one per-page entry");
    assert_eq!(
        out.pages[0].0, 2,
        "per-page entry must carry the real page number"
    );
}

/// A `should_cancel` that is true on entry aborts before OCRing any page: no Page
/// events, and an Err the GUI's run_ocr remaps to "stopped". Guards the page-loop
/// cancellation check that makes Stop responsive (esp. for the remote backend,
/// which has no llama-server pid to kill).
#[test]
fn ocr_pages_aborts_when_cancelled() {
    if std::process::Command::new("pdftoppm")
        .arg("-v")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("skipping ocr_pages_aborts_when_cancelled: pdftoppm not on PATH");
        return;
    }

    let stub_port = spawn_stub_ocr_server();
    let srv = server::Server::for_test(stub_port).expect("for_test");

    let pdf_dir = tempfile::tempdir().expect("tmp pdf dir");
    let pdf_path = pdf_dir.path().join("fixture.pdf");
    std::fs::write(&pdf_path, two_page_pdf_bytes()).expect("write fixture pdf");

    let opts = OcrOptions::default();
    let mut events: Vec<Progress> = Vec::new();
    let result = ocr_pages(
        &srv,
        std::path::Path::new("pdftoppm"),
        &pdf_path,
        &opts,
        &mut |p: Progress| events.push(p),
        &|| true,
    );

    assert!(
        result.is_err(),
        "cancelled run must return Err, got {result:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(e, Progress::Page { .. })),
        "no page should be OCR'd when cancelled on entry: {events:?}"
    );
}

/// Spawn a throwaway HTTP server that returns a valid OpenAI chat-completion for
/// any POST and return its port. Used by the ocr_pages tests so they exercise the
/// real rasterize+request loop without a live llama-server. Drains the request
/// body before replying so ureq never sees a partial read.
fn spawn_stub_ocr_server() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub server");
    let port = listener.local_addr().expect("local_addr").port();
    let resp_body = serde_json::json!({
        "choices": [{ "message": { "content": "# page text" } }]
    })
    .to_string();
    let http_response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        resp_body.len(),
        resp_body,
    );
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};
        for stream in listener.incoming() {
            let Ok(s) = stream else { break };
            let mut reader = BufReader::new(s.try_clone().expect("clone socket"));
            let mut writer = s;
            let mut content_length: usize = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let trimmed = line.trim_end_matches(['\r', '\n']);
                if trimmed.is_empty() {
                    break;
                }
                if trimmed.to_ascii_lowercase().starts_with("content-length:") {
                    if let Some(v) = trimmed.split_once(':').map(|x| x.1) {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            let mut body = vec![0u8; content_length];
            let _ = std::io::Read::read_exact(&mut reader, &mut body);
            let _ = Write::write_all(&mut writer, http_response.as_bytes());
        }
    });
    port
}

/// Minimal valid 2-page PDF (Catalog -> Pages with two text pages + computed
/// xref). Inlined so the tests add no binary fixture to the repo.
fn two_page_pdf_bytes() -> Vec<u8> {
    let p1 = "<</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Contents 4 0 R/Resources<</Font<</F1 7 0 R>>>>>>";
    let p2 = "<</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Contents 6 0 R/Resources<</Font<</F1 7 0 R>>>>>>";
    let objs: [&str; 7] = [
        "<</Type/Catalog/Pages 2 0 R>>",
        "<</Type/Pages/Kids[3 0 R 5 0 R]/Count 2>>",
        p1,
        "<</Length 38>>stream\nBT /F1 12 Tf 10 80 Td (Page one) Tj ET\nendstream",
        p2,
        "<</Length 38>>stream\nBT /F1 12 Tf 10 80 Td (Page two) Tj ET\nendstream",
        "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>",
    ];
    let mut buf = String::from("%PDF-1.4\n");
    let mut offsets: Vec<usize> = Vec::with_capacity(objs.len());
    for (i, obj) in objs.iter().enumerate() {
        offsets.push(buf.len());
        buf.push_str(&format!("{} 0 obj{}\nendobj\n", i + 1, obj));
    }
    let xref_start = buf.len();
    buf.push_str("xref\n0 8\n0000000000 65535 f \n");
    for off in &offsets {
        buf.push_str(&format!("{:010} 00000 n \n", off));
    }
    buf.push_str(&format!(
        "trailer<</Size 8/Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n",
    ));
    buf.into_bytes()
}
