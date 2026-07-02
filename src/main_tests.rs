use super::*;
use std::fs;

#[test]
fn markdown_task_matches_default_prompt() {
    // The "markdown" preset is the no-flags default; the GUI's TASK_PROMPTS
    // map (gui/src/main.js) mirrors these strings. If the default prompt is
    // ever changed, this fails so the preset (and the JS copy) get updated too.
    assert_eq!(
        Task::Markdown.prompt(),
        unlocr::OcrOptions::default().prompt,
        "Task::Markdown must equal the OcrOptions default prompt"
    );
}

#[test]
fn expand_folder_list_dedup() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("a.pdf"), b"").unwrap();
    fs::write(root.join("UPPER.PDF"), b"").unwrap(); // is_pdf must be case-insensitive
    fs::write(root.join("not.txt"), b"").unwrap();
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("sub/b.pdf"), b"").unwrap();

    // non-recursive: top-level pdfs (any case), .txt excluded; sorted
    let flat = expand_inputs(&[root.to_path_buf()], None, false).unwrap();
    assert_eq!(flat, vec![root.join("UPPER.PDF"), root.join("a.pdf")]);

    // recursive: includes nested b.pdf
    let deep = expand_inputs(&[root.to_path_buf()], None, true).unwrap();
    assert_eq!(
        deep,
        vec![
            root.join("UPPER.PDF"),
            root.join("a.pdf"),
            root.join("sub/b.pdf")
        ]
    );

    // dedup: a.pdf via both folder and --from-list appears once
    let list = root.join("list.txt");
    fs::write(
        &list,
        format!("# comment\n\n{}\n", root.join("a.pdf").display()),
    )
    .unwrap();
    let merged = expand_inputs(&[root.to_path_buf()], Some(&list), false).unwrap();
    assert_eq!(merged, vec![root.join("UPPER.PDF"), root.join("a.pdf")]);

    // empty folder errors
    let empty = tempfile::tempdir().unwrap();
    assert!(expand_inputs(&[empty.path().to_path_buf()], None, false).is_err());
}

#[test]
fn expand_glob_and_literal() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("a.pdf"), b"").unwrap();
    fs::write(root.join("note.txt"), b"").unwrap();

    // glob pattern: matches a.pdf, skips note.txt (is_pdf filter)
    let pat = PathBuf::from(root.join("*.pdf").to_str().unwrap());
    assert_eq!(
        expand_inputs(&[pat], None, false).unwrap(),
        vec![root.join("a.pdf")]
    );

    // literal non-existent path passes through verbatim (run_pdf validates later)
    let lit = PathBuf::from("does-not-exist.pdf");
    assert_eq!(
        expand_inputs(std::slice::from_ref(&lit), None, false).unwrap(),
        vec![lit]
    );
}

#[test]
fn quality_quant_mapping() {
    assert_eq!(Quality::Best.quant(), "BF16");
    assert_eq!(Quality::Good.quant(), "Q8_0");
    assert_eq!(Quality::Less.quant(), "Q4_K_M");
}

#[test]
fn task_prompt_mapping() {
    assert_eq!(Task::Markdown.prompt(), "document parsing.");
    assert_eq!(
        Task::Grounding.prompt(),
        "<|grounding|>Convert the document to markdown."
    );
    assert_eq!(Task::Free.prompt(), "Free OCR.");
    assert_eq!(Task::Figure.prompt(), "Parse the figure.");
    // CLI parity: the default task's prompt must equal the no-flags default so
    // `unlocr file.pdf` behaves identically before and after presets existed.
    assert_eq!(
        Task::Markdown.prompt(),
        unlocr::OcrOptions::default().prompt
    );
}

#[test]
fn resolved_prompt_prefers_explicit_over_task() {
    let mut args = Args::parse_from(["unlocr", "x.pdf", "--task", "free"]);
    assert_eq!(args.resolved_prompt(), "Free OCR.");
    // Explicit --prompt wins over the task preset.
    args.prompt = Some("custom".to_string());
    assert_eq!(args.resolved_prompt(), "custom");
}

#[test]
fn parse_pages_accepts_single_and_range() {
    assert_eq!(parse_pages("5").unwrap(), (5, Some(5)));
    assert_eq!(parse_pages("5-9").unwrap(), (5, Some(9)));
    assert_eq!(parse_pages(" 5 - 9 ").unwrap(), (5, Some(9))); // whitespace tolerant
    assert_eq!(parse_pages("3-3").unwrap(), (3, Some(3))); // degenerate range
}

#[test]
fn parse_pages_rejects_bad_input() {
    assert!(parse_pages("0").is_err()); // 1-based
    assert!(parse_pages("9-5").is_err()); // reversed
    assert!(parse_pages("1-0").is_err()); // zero bound
    assert!(parse_pages("abc").is_err()); // non-numeric
    assert!(parse_pages("").is_err()); // empty
    assert!(parse_pages("5-").is_err()); // missing bound
}

#[test]
fn gpu_flag_fills_remote_defaults() {
    // --gpu with nothing else points the remote path at a local vLLM serving
    // the full Unlimited-OCR model. Mirrors the normalization in run().
    let mut args = Args::parse_from(["unlocr", "x.pdf", "--gpu"]);
    args.apply_gpu_defaults();
    assert_eq!(args.endpoint.as_deref(), Some("http://localhost:8000"));
    assert_eq!(args.endpoint_model.as_deref(), Some("baidu/Unlimited-OCR"));

    // An explicit --endpoint wins; --gpu only fills the model default.
    let mut args = Args::parse_from(["unlocr", "x.pdf", "--gpu", "--endpoint", "http://host:9000"]);
    args.apply_gpu_defaults();
    assert_eq!(args.endpoint.as_deref(), Some("http://host:9000"));
    assert_eq!(args.endpoint_model.as_deref(), Some("baidu/Unlimited-OCR"));

    // No --gpu = no remote defaults injected (stays local GGUF path).
    let mut args = Args::parse_from(["unlocr", "x.pdf"]);
    args.apply_gpu_defaults();
    assert_eq!(args.endpoint, None);
    assert_eq!(args.endpoint_model, None);
}

#[test]
fn resolved_pages_none_when_flag_absent() {
    let args = Args::parse_from(["unlocr", "x.pdf"]);
    assert_eq!(args.resolved_pages().unwrap(), None);
    let args = Args::parse_from(["unlocr", "x.pdf", "--pages", "2-3"]);
    assert_eq!(args.resolved_pages().unwrap(), Some((2, Some(3))));
}

#[test]
fn output_mode_parses_and_defaults_to_single() {
    // Default (no flag) is single, preserving the original single-file behaviour.
    let args = Args::parse_from(["unlocr", "x.pdf"]);
    assert_eq!(args.output_mode, OutputModeArg::Single);
    assert_eq!(args.output_mode.to_mode(), unlocr::OutputMode::Single);

    // Each accepted value maps through to the lib enum.
    let args = Args::parse_from(["unlocr", "x.pdf", "--output-mode", "pages"]);
    assert_eq!(args.output_mode, OutputModeArg::Pages);
    assert_eq!(args.output_mode.to_mode(), unlocr::OutputMode::Pages);

    let args = Args::parse_from(["unlocr", "x.pdf", "--output-mode", "both"]);
    assert_eq!(args.output_mode.to_mode(), unlocr::OutputMode::Both);

    // Unknown value is rejected by clap at parse time (not silently single).
    assert!(Args::try_parse_from(["unlocr", "x.pdf", "--output-mode", "bogus"]).is_err());
}
