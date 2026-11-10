#![no_main]
//! Download helpers — path injection, opaque keys, progress text.
use libfuzzer_sys::fuzz_target;

use fluxplay::fuzz_api::{
    downloads_dir, episode_title, extension_for, human_size, progress_text, sanitize_file_name,
    url_key, validate_downloads_dir,
};

fuzz_target!(|data: &[u8]| {
    let Ok(raw) = std::str::from_utf8(data) else {
        return;
    };

    let s = sanitize_file_name(raw);
    assert!(!s.is_empty());
    assert!(!s.contains('/'));
    assert!(!s.contains('\\'));
    assert!(!s.contains('%'));
    assert!(!s.contains('\0'));
    assert!(!s.chars().any(|c| c.is_control()));
    assert!(s.chars().count() <= 150);
    assert!(s.len() <= 240, "utf-8 bytes exploded: {}", s.len());
    assert!(s != ".." && s != ".");

    let key = url_key(raw);
    assert_eq!(key.len(), 32);
    assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(!key.contains('@'));
    assert!(!key.contains(':'));

    let ext = extension_for(raw, raw);
    assert!(!ext.is_empty());
    assert!(ext.len() <= 8);
    assert!(ext.chars().all(|c| c.is_ascii_alphanumeric()));

    if data.len() >= 16 {
        let done = u64::from_le_bytes(data[0..8].try_into().unwrap());
        let total = u64::from_le_bytes(data[8..16].try_into().unwrap());
        let h = human_size(done);
        assert!(!h.is_empty());
        assert!(h.len() < 64);
        let p = progress_text(done, Some(total.max(1)));
        assert!(!p.is_empty());
        assert!(p.len() < 128);
        // Degenerate: total < done must still render without panic/NaN text.
        let p2 = progress_text(done, Some(total.min(done.saturating_sub(1)).max(1)));
        assert!(!p2.is_empty());
    }

    // Display title may keep URL punctuation; the path stem must still sanitize.
    let ep = episode_title(raw, (data.first().copied().unwrap_or(1) as u32) % 99 + 1, 1, raw);
    assert!(ep.len() < raw.len().saturating_mul(4).max(512) + 64);
    let stem = sanitize_file_name(&ep);
    assert!(!stem.contains('/'));
    assert!(!stem.contains('\\'));
    assert!(!stem.contains('\0'));

    // Relative / traversal download dirs must not resolve into absolute escapes
    // without going through validate (Err is fine).
    let resolved = downloads_dir(raw);
    let _ = validate_downloads_dir(raw);
    let disp = resolved.to_string_lossy();
    assert!(!disp.contains('\0'));
});
