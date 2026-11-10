#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(body) = std::str::from_utf8(data) else {
        return;
    };
    // Must never panic — Err is fine.
    match fluxplay_core::m3u::parse_m3u(body, None) {
        Ok(bundle) => {
            // Amplification / DoS ceilings — a tiny input must not allocate a
            // pathological catalog (zip-bomb style #EXTINF spam still bounded
            // by input length × small constant).
            let n = bundle.channels.len()
                + bundle.vod.len()
                + bundle.series.len()
                + bundle.categories.len();
            assert!(
                n <= body.len().saturating_add(64),
                "m3u amplification: {n} items from {} bytes",
                body.len()
            );
            for ch in bundle.channels.iter().take(128) {
                assert!(!ch.name.contains('\0'));
                assert!(!ch.stream_url.contains('\0'));
                assert!(ch.name.len() < 64 * 1024);
                assert!(ch.stream_url.len() < 256 * 1024);
            }
            for v in bundle.vod.iter().take(64) {
                assert!(!v.name.contains('\0'));
                assert!(v.name.len() < 64 * 1024);
            }
            for s in bundle.series.iter().take(64) {
                assert!(!s.name.contains('\0'));
                assert!(s.name.len() < 64 * 1024);
            }
        }
        Err(_) => {}
    }
});
