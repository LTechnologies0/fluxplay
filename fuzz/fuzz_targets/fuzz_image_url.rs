#![no_main]
//! Image URL policy — no file/ftp/data/javascript, no SVG decoder hole.
use libfuzzer_sys::fuzz_target;

use fluxplay::fuzz_api::is_fetchable_image_url;

fn path_looks_svg(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    // Strip query/fragment for extension check.
    let path = lower.split(['?', '#']).next().unwrap_or(&lower);
    path.ends_with(".svg") || path.ends_with(".svgz")
}

fuzz_target!(|data: &[u8]| {
    let Ok(raw) = std::str::from_utf8(data) else {
        return;
    };
    let ok = is_fetchable_image_url(raw);
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("file:")
        || lower.starts_with("ftp:")
        || lower.starts_with("data:")
        || lower.starts_with("javascript:")
        || lower.starts_with("blob:")
        || lower.starts_with("about:")
    {
        assert!(!ok, "scheme must be rejected: {raw}");
    }
    // Policy applies after trim — leading/trailing whitespace is fine.
    let trimmed = raw.trim();
    if trimmed.chars().any(|c| c.is_control()) {
        assert!(!ok, "control chars must be rejected: {trimmed:?}");
    }
    if (lower.starts_with("http://") || lower.starts_with("https://")) && path_looks_svg(raw) {
        assert!(!ok, "svg must be rejected: {raw:?}");
    }
});
