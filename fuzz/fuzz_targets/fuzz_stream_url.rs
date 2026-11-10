#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(raw) = std::str::from_utf8(data) else {
        return;
    };
    let scheme = fluxplay_core::protocol::StreamScheme::parse(raw);
    let _ = scheme.label();
    let _ = scheme.default_port();
    let _ = scheme.is_playback_ready();

    match fluxplay_core::protocol::StreamUrl::parse(raw) {
        Ok(url) => {
            // Debug pretty-print can be larger than a tiny input; keep a hard
            // ceiling so a pathological parse still cannot explode memory.
            let s = format!("{url:?}");
            assert!(s.len() <= 64 * 1024, "StreamUrl Debug blew up: {}", s.len());
            assert!(!url.raw.contains('\0'));
            assert!(url.raw.len() <= raw.trim().len());
            if let Some(host) = &url.host {
                assert!(!host.contains('\0'));
                assert!(host.len() < 8 * 1024);
            }
        }
        Err(_) => {}
    }
});
