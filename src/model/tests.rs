use super::download::stream_to_part;
use super::*;

#[test]
fn list_cached_quants_matches_model_files_only() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Empty dir = empty list.
    assert!(list_cached_quants(root).is_empty());

    // Matches files following Unlimited-OCR-{quant}.gguf.
    fs::write(root.join("Unlimited-OCR-Q8_0.gguf"), b"").unwrap();
    fs::write(root.join("Unlimited-OCR-BF16.gguf"), b"").unwrap();

    // Ignores mmproj prefix and non-gguf extensions.
    fs::write(root.join("mmproj-Unlimited-OCR-F16.gguf"), b"").unwrap();
    fs::write(root.join("Unlimited-OCR-Q4_K_M.part"), b"").unwrap();
    fs::write(root.join("unrelated.txt"), b"").unwrap();

    // Returns sorted quants.
    assert_eq!(
        list_cached_quants(root),
        vec!["BF16".to_string(), "Q8_0".to_string()]
    );
}

#[test]
fn check_digest_match_mismatch_and_unpinned() {
    // Stock/pinned model matches.
    assert!(matches!(
        check_digest(
            "Unlimited-OCR-Q8_0.gguf",
            "234c36f679a3768f5564e9e02c2c1deacbd5677b9c8558a57133f1813f6dd3b8"
        ),
        DigestCheck::Match
    ));
    // Case-tolerant hex check.
    assert!(matches!(
        check_digest(
            "Unlimited-OCR-Q8_0.gguf",
            "234C36F679A3768F5564E9E02C2C1DEACBD5677B9C8558A57133F1813F6DD3B8"
        ),
        DigestCheck::Match
    ));
    // Content mismatch.
    let bad_hash = "0000000000000000000000000000000000000000000000000000000000000000";
    let check = check_digest("Unlimited-OCR-Q8_0.gguf", bad_hash);
    if let DigestCheck::Mismatch { expected } = check {
        assert_eq!(
            expected,
            "234c36f679a3768f5564e9e02c2c1deacbd5677b9c8558a57133f1813f6dd3b8"
        );
    } else {
        panic!("expected Mismatch, got {check:?}");
    }
    // Unpinned quant (custom name) proceeds without digest matching.
    assert!(matches!(
        check_digest("Unlimited-OCR-Custom.gguf", bad_hash),
        DigestCheck::Unpinned
    ));
}

#[test]
fn file_sha256_matches_known_vector() {
    // sha256("abc") is a fixed NIST test vector; proves the sha2 wiring + hex
    // encoding are correct end to end (the streaming read path included).
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("abc.bin");
    std::fs::write(&p, b"abc").unwrap();
    assert_eq!(
        file_sha256(&p).unwrap(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn stream_to_part_rejects_truncated_download() {
    let tmp = tempfile::tempdir().unwrap();
    let part = tmp.path().join("file.part");
    let mut progress = |_: &str, _: Option<u8>, _: u64, _: u64| {};

    // Source terminates at 5 bytes when Content-Length promised 10.
    let src = b"12345".as_slice();
    let err = stream_to_part(&part, "test", 10, 0, src, &mut progress);
    assert!(err.is_err());
    assert!(err.unwrap_err().to_string().contains("truncated"));
}

#[test]
fn stream_to_part_rejects_missing_content_length() {
    let tmp = tempfile::tempdir().unwrap();
    let part = tmp.path().join("file.part");
    let mut progress = |_: &str, _: Option<u8>, _: u64, _: u64| {};

    // ureq response with no content-length (0) fails to verification.
    let src = b"12345".as_slice();
    let err = stream_to_part(&part, "test", 0, 0, src, &mut progress);
    assert!(err.is_err());
    assert!(err.unwrap_err().to_string().contains("no Content-Length"));
}

#[test]
fn stream_to_part_accepts_full_download() {
    let tmp = tempfile::tempdir().unwrap();
    let part = tmp.path().join("file.part");
    let mut progress = |_: &str, _: Option<u8>, _: u64, _: u64| {};

    let src = b"12345".as_slice();
    stream_to_part(&part, "test", 5, 0, src, &mut progress).unwrap();
    assert_eq!(fs::read(&part).unwrap(), b"12345");
}

#[test]
fn stream_to_part_resumes_appends_from_offset() {
    let tmp = tempfile::tempdir().unwrap();
    let part = tmp.path().join("file.part");
    let mut progress = |_: &str, _: Option<u8>, _: u64, _: u64| {};

    // Seed the partial with initial bytes.
    fs::write(&part, b"123").unwrap();

    // Resume from offset 3 (starts writing there, does not overwrite).
    let src = b"45".as_slice();
    stream_to_part(&part, "test", 5, 3, src, &mut progress).unwrap();
    assert_eq!(fs::read(&part).unwrap(), b"12345");
}

#[test]
fn filename_format() {
    // quant tag maps directly into the stock filename format.
    assert_eq!(model_filename("Q8_0"), "Unlimited-OCR-Q8_0.gguf");
    assert_eq!(model_filename("BF16"), "Unlimited-OCR-BF16.gguf");
}

#[test]
fn validate_quant_blocks_traversal() {
    // Alnum + safe symbols pass.
    assert!(validate_quant("Q8_0").is_ok());
    assert!(validate_quant("Q4_K_M").is_ok());
    assert!(validate_quant("BF16").is_ok());
    assert!(validate_quant("custom-quant.123").is_ok());

    // Path traversal triggers failure.
    assert!(validate_quant("../evil").is_err());
    assert!(validate_quant("evil/..").is_err());
    assert!(validate_quant("evil/../../sub").is_err());
    assert!(validate_quant("/absolute/path").is_err());

    // Forbidden characters.
    assert!(validate_quant("Q8_0+gpu").is_err());
    assert!(validate_quant("").is_err());
}

#[test]
fn check_presence_rejects_traversal_quant() {
    let cache = Path::new("/cache");
    let err = check_presence(cache, "../evil");
    assert!(err.is_err());
    assert!(err.unwrap_err().to_string().contains("invalid quant"));
}

#[test]
fn ensure_rejects_traversal_quant() {
    let cache = Path::new("/cache");
    let err = ensure(cache, "../evil");
    assert!(err.is_err());
    assert!(err.unwrap_err().to_string().contains("invalid quant"));
}

#[test]
fn cache_dir_override_used_and_created() {
    let tmp = tempfile::tempdir().unwrap();
    let custom = tmp.path().join("my-custom-cache");
    assert!(!custom.exists());

    let resolved = cache_dir(Some(custom.clone())).unwrap();
    assert_eq!(resolved, custom);
    assert!(custom.exists());
}

#[test]
fn ensure_with_progress_no_events_when_files_present() {
    // Cache hits: if both GGUFs exist, ensure must return immediately and
    // fire zero download events.
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path();
    fs::write(cache.join("Unlimited-OCR-Q8_0.gguf"), b"").unwrap();
    fs::write(cache.join("mmproj-Unlimited-OCR-F16.gguf"), b"").unwrap();

    let mut events = Vec::new();
    let files = ensure_with_progress(cache, "Q8_0", &mut |p| events.push(p)).unwrap();
    assert_eq!(files.model, cache.join("Unlimited-OCR-Q8_0.gguf"));
    assert_eq!(files.mmproj, cache.join("mmproj-Unlimited-OCR-F16.gguf"));
    assert!(events.is_empty(), "cache hit should emit no events");
}

#[test]
fn ensure_with_overrides_uses_model_path_and_falls_back_to_cached_mmproj() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path();
    fs::write(cache.join("mmproj-Unlimited-OCR-F16.gguf"), b"").unwrap();

    let override_model = tmp.path().join("my-custom-model.gguf");
    fs::write(&override_model, b"").unwrap();

    let mut events = Vec::new();
    let files = ensure_with_overrides(cache, "Q8_0", Some(&override_model), None, &mut |p| {
        events.push(p)
    })
    .unwrap();
    assert_eq!(files.model, override_model);
    assert_eq!(files.mmproj, cache.join("mmproj-Unlimited-OCR-F16.gguf"));
    assert!(events.is_empty());
}

#[test]
fn ensure_with_overrides_errors_on_missing_model_path() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path();
    let missing_model = tmp.path().join("missing.gguf");

    let mut events = Vec::new();
    let err = ensure_with_overrides(cache, "Q8_0", Some(&missing_model), None, &mut |p| {
        events.push(p)
    });
    assert!(err.is_err());
    assert!(err
        .unwrap_err()
        .to_string()
        .contains("model file not found"));
}

#[test]
fn ensure_with_overrides_uses_mmproj_override_path() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path();
    fs::write(cache.join("Unlimited-OCR-Q8_0.gguf"), b"").unwrap();

    let override_mmproj = tmp.path().join("my-custom-mmproj.gguf");
    fs::write(&override_mmproj, b"").unwrap();

    let mut events = Vec::new();
    let files = ensure_with_overrides(cache, "Q8_0", None, Some(&override_mmproj), &mut |p| {
        events.push(p)
    })
    .unwrap();
    assert_eq!(files.model, cache.join("Unlimited-OCR-Q8_0.gguf"));
    assert_eq!(files.mmproj, override_mmproj);
    assert!(events.is_empty());
}
