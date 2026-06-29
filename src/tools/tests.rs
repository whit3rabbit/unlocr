use super::*;
use std::io::Write;

/// Build a tiny in-memory zip with the given (path, bytes) entries.
fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
    }
    buf
}

#[test]
fn extract_then_find_exe_in_subdir() {
    let tmp = tempfile::tempdir().unwrap();
    // Mirror poppler's layout: exe nested under a versioned dir + Library/bin.
    let zip = make_zip(&[
        ("poppler-26/Library/bin/pdftoppm.exe", b"MZ binary"),
        ("poppler-26/Library/bin/libfoo.dll", b"dll"),
        ("poppler-26/share/readme.txt", b"hi"),
    ]);
    let dest = tmp.path().join("pdftoppm");
    let zip_path = tmp.path().join("test.zip");
    fs::write(&zip_path, &zip).unwrap();
    extract_zip(&zip_path, &dest).unwrap();
    let found = find_exe(&dest, "pdftoppm.exe").expect("exe resolved in nested dir");
    assert!(found.ends_with("Library/bin/pdftoppm.exe"));
    // The DLL beside it survives (directory tree preserved).
    assert!(found.with_file_name("libfoo.dll").exists());
}

/// On unix the extracted binary must keep its exec bit (mode 0755 in the archive),
/// or the spawned tool won't run. Guards the macOS pandoc path.
#[cfg(unix)]
#[test]
fn extract_preserves_exec_bit() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let mut buf = Vec::new();
    {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o755);
        w.start_file("pandoc-3.10/bin/pandoc", opts).unwrap();
        w.write_all(b"#!/bin/sh\n").unwrap();
        w.finish().unwrap();
    }
    let dest = tmp.path().join("pandoc");
    let zip_path = tmp.path().join("test.zip");
    fs::write(&zip_path, &buf).unwrap();
    extract_zip(&zip_path, &dest).unwrap();
    let exe = find_exe(&dest, "pandoc").expect("pandoc resolved");
    let mode = fs::metadata(&exe).unwrap().permissions().mode();
    assert!(
        mode & 0o111 != 0,
        "extracted binary not executable: {mode:o}"
    );
}

#[test]
fn extract_rejects_zip_slip() {
    let tmp = tempfile::tempdir().unwrap();
    let zip = make_zip(&[("../escape.exe", b"evil")]);
    let dest = tmp.path().join("t");
    let zip_path = tmp.path().join("test.zip");
    fs::write(&zip_path, &zip).unwrap();
    let err = extract_zip(&zip_path, &dest).unwrap_err();
    assert!(
        err.to_string().contains("unsafe path"),
        "expected zip-slip rejection, got: {err}"
    );
    // Nothing was written outside dest.
    assert!(!tmp.path().join("escape.exe").exists());
}

/// Every pin compiled for THIS build is well-formed: https url, 64-hex sha256, a
/// non-empty exe whose extension matches the OS convention (.exe on Windows, bare
/// elsewhere). Runs per-host, so each platform's CI validates its own `PINS`.
#[test]
fn pins_are_well_formed() {
    for t in PINS {
        assert!(t.url.starts_with("https://"), "{} url not https", t.name);
        assert_eq!(t.sha256.len(), 64, "{} sha256 not 64 hex chars", t.name);
        assert!(
            t.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
            "{} sha256 not hex",
            t.name
        );
        assert!(!t.exe.is_empty(), "{} has empty exe", t.name);
        if cfg!(target_os = "windows") {
            assert!(t.exe.ends_with(".exe"), "{} exe should be .exe", t.name);
        } else {
            assert!(!t.exe.ends_with(".exe"), "{} exe should be bare", t.name);
        }
    }
}

#[test]
fn find_exe_missing_dir_is_none() {
    let missing = std::env::temp_dir().join("unlocr-no-such-dir-xyz-123");
    assert!(find_exe(&missing, "pandoc").is_none());
}

/// `downloadable()` exactly mirrors the pins compiled for this build (Windows: three;
/// macOS: pandoc; Linux/other: none). Per-host, so each OS's CI asserts its own set.
#[test]
fn downloadable_matches_pins() {
    let got = downloadable();
    let want: Vec<&str> = PINS.iter().map(|t| t.name).collect();
    assert_eq!(got, want);
    if cfg!(not(any(target_os = "windows", target_os = "macos"))) {
        assert!(got.is_empty(), "Linux/other must not offer auto-download");
    }
}

/// A tool with no pin for this platform is refused BEFORE any network IO, with a
/// package-manager hint. Uses a name that is never pinned, so it is platform-agnostic
/// (won't trigger a real download on Windows/macOS where real tools are pinned).
#[test]
fn ensure_tool_unknown_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let err = ensure_tool(tmp.path(), "definitely-not-a-tool", &mut |_| {}).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("not available") && msg.contains("package manager"),
        "expected a package-manager hint, got: {msg}"
    );
}

#[test]
fn ensure_tool_is_atomic() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_path = tmp.path();

    // 1. Mock a successful download with a valid zip containing the executable
    let exe_name = if cfg!(target_os = "windows") {
        "pandoc.exe"
    } else {
        "pandoc"
    };
    let zip_bytes = make_zip(&[(exe_name, b"MZ dummy bin")]);

    let zip_bytes_clone = zip_bytes.clone();
    MOCK_DOWNLOAD.with(|m| {
        *m.borrow_mut() = Some(Box::new(move |_url, _name, _expected| {
            Ok(zip_bytes_clone.clone())
        }));
    });

    // Calling ensure_tool should succeed
    let res = ensure_tool(cache_path, "pandoc", &mut |_| {});
    assert!(res.is_ok(), "ensure_tool should succeed: {:?}", res.err());
    let exe_path = res.unwrap();
    assert!(exe_path.exists());

    // Verify that the tools directory contains only "pandoc" (no leftover temp dirs)
    let entries: Vec<_> = fs::read_dir(tools_dir(cache_path))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, vec!["pandoc".to_string()]);

    // Clean up the target directory for the next step of testing
    let target_dir = tools_dir(cache_path).join("pandoc");
    fs::remove_dir_all(&target_dir).unwrap();

    // 2. Mock a failed download/extraction: download returns invalid zip bytes
    MOCK_DOWNLOAD.with(|m| {
        *m.borrow_mut() = Some(Box::new(|_url, _name, _expected| {
            Ok(vec![1, 2, 3, 4]) // invalid zip header, extract_zip will fail
        }));
    });

    let res = ensure_tool(cache_path, "pandoc", &mut |_| {});
    assert!(res.is_err(), "ensure_tool should fail on invalid zip");

    // Verify that neither the final directory nor any temp directory exists under tools
    assert!(
        !target_dir.exists(),
        "Target directory should not exist after failed extraction"
    );
    let tools_path = tools_dir(cache_path);
    if tools_path.exists() {
        let entries: Vec<_> = fs::read_dir(&tools_path)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(entries.is_empty(), "Leftover entries found: {:?}", entries);
    }

    // 3. Clear mock download
    MOCK_DOWNLOAD.with(|m| {
        *m.borrow_mut() = None;
    });
}
