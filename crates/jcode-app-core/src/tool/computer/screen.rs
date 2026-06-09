//! Screen observation: full-screen + per-window screenshots and OCR.

use super::osa;
use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use core_graphics::display::CGDisplay;
use jcode_tool_types::ToolOutput;
use serde_json::json;
use std::process::Command;

/// Read width/height from a PNG IHDR chunk. Returns None if not a PNG.
pub fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.len() < 24 || bytes[..8] != PNG_SIG {
        return None;
    }
    let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    Some((w, h))
}

fn capture_to_temp(extra_args: &[&str]) -> Result<Vec<u8>> {
    let tmp = std::env::temp_dir().join(format!(
        "jcode_computer_{}_{}.png",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    let mut cmd = Command::new("/usr/sbin/screencapture");
    cmd.arg("-x");
    cmd.args(extra_args);
    cmd.arg(&tmp);
    let status = cmd.status().context("failed to run screencapture")?;
    if !status.success() {
        bail!("screencapture failed (exit {:?})", status.code());
    }
    let bytes = std::fs::read(&tmp).context("failed to read screenshot file")?;
    let _ = std::fs::remove_file(&tmp);
    if bytes.is_empty() {
        bail!(
            "screenshot was empty. Grant Screen Recording permission (run the `setup` action), or \
             in System Settings > Privacy & Security > Screen Recording."
        );
    }
    Ok(bytes)
}

pub fn screenshot() -> Result<ToolOutput> {
    let bytes = capture_to_temp(&[])?;
    let bounds = CGDisplay::main().bounds();
    let point_w = bounds.size.width;
    let point_h = bounds.size.height;
    let (pixel_w, pixel_h) = png_dimensions(&bytes).unwrap_or((point_w as u32, point_h as u32));
    let scale = if point_w > 0.0 {
        pixel_w as f64 / point_w
    } else {
        1.0
    };
    let summary = format!(
        "Captured main display: {pixel_w}x{pixel_h} pixels = {point_w:.0}x{point_h:.0} points \
         (scale {scale:.2}x). Click/move coordinates are in POINTS: for a feature at image pixel \
         (px, py), use x = px / {scale:.2}, y = py / {scale:.2}.",
    );
    Ok(ToolOutput::new(summary)
        .with_title("screenshot")
        .with_labeled_image("image/png", STANDARD.encode(&bytes), "screen")
        .with_metadata(json!({
            "width_points": point_w, "height_points": point_h,
            "width_pixels": pixel_w, "height_pixels": pixel_h, "scale": scale,
        })))
}

/// Screenshot a single window by its CoreGraphics window id, even if occluded.
pub fn window_screenshot(window_id: i64) -> Result<ToolOutput> {
    let bytes = capture_to_temp(&["-o", "-l", &window_id.to_string()])?;
    let (pixel_w, pixel_h) = png_dimensions(&bytes).unwrap_or((0, 0));
    Ok(ToolOutput::new(format!(
        "Captured window {window_id}: {pixel_w}x{pixel_h} pixels."
    ))
    .with_title("window screenshot")
    .with_labeled_image("image/png", STANDARD.encode(&bytes), "window")
    .with_metadata(json!({ "window_id": window_id, "width_pixels": pixel_w, "height_pixels": pixel_h })))
}

/// OCR a region (or the whole screen) using the macOS Vision framework via a
/// small inline Swift/JXA bridge. Returns recognized strings with bounding
/// boxes so the model can click located text in apps with no Accessibility.
pub fn ocr(region: Option<[f64; 4]>) -> Result<ToolOutput> {
    // Capture (optionally a region) to a temp file, then OCR it.
    let region_args: Vec<String> = if let Some([x, y, w, h]) = region {
        vec!["-R".to_string(), format!("{x},{y},{w},{h}")]
    } else {
        vec![]
    };
    let tmp = std::env::temp_dir().join(format!("jcode_ocr_{}.png", std::process::id()));
    let mut cmd = Command::new("/usr/sbin/screencapture");
    cmd.arg("-x");
    for a in &region_args {
        cmd.arg(a);
    }
    cmd.arg(&tmp);
    let status = cmd.status().context("failed to run screencapture for OCR")?;
    if !status.success() {
        bail!("screencapture failed for OCR (exit {:?})", status.code());
    }

    let img_path = tmp.to_string_lossy().to_string();
    let result = run_vision_ocr(&img_path);
    let _ = std::fs::remove_file(&tmp);
    let text = result?;
    let summary = if text.trim().is_empty() {
        "OCR found no text.".to_string()
    } else {
        text
    };
    Ok(ToolOutput::new(summary).with_title("ocr"))
}

/// Run the Vision OCR via the `osascript`-launched Swift one-liner is not viable;
/// instead use a tiny Swift program through `swift` if available, falling back to
/// a clear message. Vision has no AppleScript binding, so we shell to Swift.
fn run_vision_ocr(image_path: &str) -> Result<String> {
    // Prefer a compiled helper if present; otherwise use `swift` to run inline.
    let swift_src = format!(
        r#"
import Foundation
import Vision
import AppKit

let url = URL(fileURLWithPath: "{path}")
guard let img = NSImage(contentsOf: url), let cg = img.cgImage(forProposedRect: nil, context: nil, hints: nil) else {{
    FileHandle.standardError.write("could not load image\n".data(using: .utf8)!)
    exit(2)
}}
let req = VNRecognizeTextRequest {{ request, error in
    guard let obs = request.results as? [VNRecognizedTextObservation] else {{ return }}
    for o in obs {{
        guard let top = o.topCandidates(1).first else {{ continue }}
        let b = o.boundingBox // normalized, origin bottom-left
        print("\(b.origin.x),\(b.origin.y),\(b.size.width),\(b.size.height)\t\(top.string)")
    }}
}}
req.recognitionLevel = .accurate
req.usesLanguageCorrection = true
let handler = VNImageRequestHandler(cgImage: cg, options: [:])
try? handler.perform([req])
"#,
        path = image_path
    );

    let swift = which_swift();
    let Some(swift) = swift else {
        bail!(
            "OCR needs the Swift toolchain (Vision framework has no scripting bridge). \
             Install Xcode command line tools: xcode-select --install"
        );
    };

    let out = Command::new(&swift)
        .arg("-")
        .arg(image_path)
        .env("JCODE_OCR_IMG", image_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(swift_src.as_bytes())?;
            }
            child.wait_with_output()
        })
        .context("failed to run swift for OCR")?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("Vision OCR failed: {}", err.trim());
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    // Each line: x,y,w,h\ttext (bbox normalized, origin bottom-left).
    let mut lines = vec![
        "Recognized text (bbox normalized 0..1, origin bottom-left; multiply by image size):"
            .to_string(),
    ];
    for line in raw.lines() {
        lines.push(line.to_string());
    }
    Ok(lines.join("\n"))
}

fn which_swift() -> Option<String> {
    for p in ["/usr/bin/swift", "/usr/local/bin/swift"] {
        if std::path::Path::new(p).exists() {
            return Some(p.to_string());
        }
    }
    // try PATH
    let out = Command::new("/usr/bin/which").arg("swift").output().ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

/// Used by `osa`-based callers that just need a quick AX-free description.
#[allow(dead_code)]
pub fn frontmost_app() -> Result<String> {
    osa::run_applescript(
        "tell application \"System Events\" to get name of first application process whose frontmost is true",
    )
}
