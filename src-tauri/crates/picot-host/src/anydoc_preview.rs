// ABOUTME: Native Office preview adapter over the pinned anydoc crate.
// ABOUTME: Owns the strict candidate list, closed error codes, and size caps.

use anydoc::ConvertError;
use std::path::Path;

/// Only a filename with one of these suffixes enters the Office preview
/// branch (spec 2026-09-17). Content detection then decides the parser.
const CANDIDATE_SUFFIXES: [&str; 10] = [
    "doc", "docx", "rtf", "odt", "ppt", "pptx", "odp", "xls", "xlsx", "ods",
];

/// Candidate Office reads may use a dedicated 32 MiB input cap; every other
/// read stays at the generic host_files cap.
pub const PREVIEW_INPUT_CAP_BYTES: u64 = 32 * 1024 * 1024;

/// Converted Markdown is capped at 2 MiB of UTF-8 — far below the WebSocket
/// response cap even with worst-case JSON control-character escaping.
pub const PREVIEW_OUTPUT_CAP_BYTES: usize = 2 * 1024 * 1024;

/// Closed, detail-free failure codes. Nothing from anydoc (parts, limits,
/// paths, messages, bytes) may leave this module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewErrorCode {
    Unsupported,
    InputTooLarge,
    Malformed,
    Encrypted,
    ResourceLimit,
    MissingPart,
    OutputTooLarge,
    Internal,
}

impl PreviewErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            PreviewErrorCode::Unsupported => "unsupported",
            PreviewErrorCode::InputTooLarge => "input_too_large",
            PreviewErrorCode::Malformed => "malformed",
            PreviewErrorCode::Encrypted => "encrypted",
            PreviewErrorCode::ResourceLimit => "resource_limit",
            PreviewErrorCode::MissingPart => "missing_part",
            PreviewErrorCode::OutputTooLarge => "output_too_large",
            PreviewErrorCode::Internal => "internal",
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum PreviewOutcome {
    Ready(String),
    Failed(PreviewErrorCode),
}

/// True when the filename's suffix is one of the ten candidate suffixes.
/// This is the only gate that may raise a read cap; it never scans
/// arbitrary binaries.
pub fn is_candidate(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            CANDIDATE_SUFFIXES
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(ext))
        })
}

/// The strict Office set this adapter serves. Detected formats outside it
/// (PDF, EPUB, CSV, …) fail closed as Unsupported.
fn is_strict_office(format: anydoc::Format) -> bool {
    matches!(
        format,
        anydoc::Format::Doc
            | anydoc::Format::Docx
            | anydoc::Format::Rtf
            | anydoc::Format::Odt
            | anydoc::Format::Ppt
            | anydoc::Format::Pptx
            | anydoc::Format::Odp
            | anydoc::Format::Excel
            | anydoc::Format::Ods
    )
}

/// Convert authorized candidate bytes. Content detection wins; the
/// candidate suffix is only the fallback for signature-less inputs.
pub fn convert_candidate(bytes: &[u8], relative_path: &str) -> PreviewOutcome {
    let suffix_format = Path::new(relative_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(anydoc::Format::from_extension);
    let Some(format) = anydoc::Format::from_bytes(bytes).or(suffix_format) else {
        return PreviewOutcome::Failed(PreviewErrorCode::Unsupported);
    };
    if !is_strict_office(format) {
        // Includes a candidate filename whose bytes detect as PDF: fail
        // closed, never reroute to the raw PDF path.
        return PreviewOutcome::Failed(PreviewErrorCode::Unsupported);
    }
    match anydoc::to_markdown_bytes(bytes, format) {
        Ok(markdown) => cap_markdown(markdown),
        Err(error) => PreviewOutcome::Failed(map_error(error)),
    }
}

fn cap_markdown(markdown: String) -> PreviewOutcome {
    if markdown.len() > PREVIEW_OUTPUT_CAP_BYTES {
        return PreviewOutcome::Failed(PreviewErrorCode::OutputTooLarge);
    }
    PreviewOutcome::Ready(markdown)
}

fn map_error(error: ConvertError) -> PreviewErrorCode {
    match error {
        ConvertError::Unsupported(_) => PreviewErrorCode::Unsupported,
        // PDF is excluded from this surface entirely, so OCR-needing inputs
        // are simply unsupported here.
        ConvertError::NeedsOcr { .. } => PreviewErrorCode::Unsupported,
        ConvertError::Malformed { .. } => PreviewErrorCode::Malformed,
        ConvertError::Encrypted => PreviewErrorCode::Encrypted,
        ConvertError::ResourceLimit { .. } => PreviewErrorCode::ResourceLimit,
        ConvertError::MissingPart { .. } => PreviewErrorCode::MissingPart,
        ConvertError::Io(_) => PreviewErrorCode::Internal,
        // ConvertError is #[non_exhaustive]; the wildcard maps to the fixed
        // Internal code without inspecting or forwarding the source error.
        _ => PreviewErrorCode::Internal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let path = [
            env!("PICOT_SRC_TAURI_DIR"),
            "../extensions/fixtures/anydoc",
            name,
        ]
        .iter()
        .collect::<std::path::PathBuf>();
        std::fs::read(path).unwrap_or_else(|error| panic!("fixture {name}: {error}"))
    }

    #[test]
    fn candidate_gating_matches_only_the_ten_office_suffixes() {
        for name in [
            "a.doc",
            "a.docx",
            "a.rtf",
            "a.odt",
            "a.ppt",
            "a.pptx",
            "a.odp",
            "a.xls",
            "a.xlsx",
            "a.ods",
            "UPPER.DOCX",
        ] {
            assert!(is_candidate(name), "{name} must be a candidate");
        }
        for name in [
            "a.txt",
            "a.md",
            "a.pdf",
            "a.csv",
            "a.msg",
            "a",
            "a.docx.bak",
        ] {
            assert!(!is_candidate(name), "{name} must not be a candidate");
        }
    }

    #[test]
    fn every_strict_fixture_converts_to_non_empty_markdown() {
        for (file, name) in [
            ("text.doc", "report.doc"),
            ("text.docx", "report.docx"),
            ("text.rtf", "report.rtf"),
            ("text.odt", "report.odt"),
            ("pres.ppt", "deck.ppt"),
            ("pres.pptx", "deck.pptx"),
            ("pres.odp", "deck.odp"),
            ("sheet.xls", "sheet.xls"),
            ("sheet.xlsx", "sheet.xlsx"),
            ("sheet.ods", "sheet.ods"),
        ] {
            match convert_candidate(&fixture(file), name) {
                PreviewOutcome::Ready(markdown) => {
                    assert!(!markdown.trim().is_empty(), "{file} converted empty");
                }
                outcome => panic!("{file} failed: {outcome:?}"),
            }
        }
    }

    #[test]
    fn candidate_pdf_bytes_fail_closed_as_unsupported() {
        let bytes = fixture("text.pdf");
        assert_eq!(
            convert_candidate(&bytes, "mislabeled.docx"),
            PreviewOutcome::Failed(PreviewErrorCode::Unsupported)
        );
    }

    #[test]
    fn non_office_detected_formats_reject_as_unsupported() {
        let bytes = fixture("text.pdf");
        assert_eq!(
            convert_candidate(&bytes, "book.odt"),
            PreviewOutcome::Failed(PreviewErrorCode::Unsupported)
        );
    }

    #[test]
    fn encrypted_fixture_maps_to_the_closed_encrypted_code() {
        let bytes = fixture("encrypted--errors.odt");
        assert_eq!(
            convert_candidate(&bytes, "locked.odt"),
            PreviewOutcome::Failed(PreviewErrorCode::Encrypted)
        );
    }

    #[test]
    fn truncated_fixture_maps_to_a_closed_code() {
        let bytes = fixture("truncated--errors.docx");
        match convert_candidate(&bytes, "broken.docx") {
            PreviewOutcome::Failed(
                PreviewErrorCode::Malformed
                | PreviewErrorCode::MissingPart
                | PreviewErrorCode::ResourceLimit
                | PreviewErrorCode::Unsupported,
            ) => {}
            outcome => panic!("truncated docx must fail closed, got {outcome:?}"),
        }
    }

    #[test]
    fn signatureless_garbage_with_candidate_suffix_fails_closed() {
        // Detection cannot identify it; the suffix fallback hands it to the
        // parser, which rejects it as structurally unusable.
        let bytes = b"not a real office document at all".repeat(10);
        match convert_candidate(&bytes, "garbage.docx") {
            PreviewOutcome::Failed(_) => {}
            PreviewOutcome::Ready(_) => panic!("garbage must not convert"),
        }
    }

    #[test]
    fn output_cap_rejects_oversized_markdown() {
        let oversized = "x".repeat(PREVIEW_OUTPUT_CAP_BYTES + 1);
        assert_eq!(
            cap_markdown(oversized),
            PreviewOutcome::Failed(PreviewErrorCode::OutputTooLarge)
        );
        let within = "x".repeat(PREVIEW_OUTPUT_CAP_BYTES);
        assert!(matches!(cap_markdown(within), PreviewOutcome::Ready(_)));
    }
}
