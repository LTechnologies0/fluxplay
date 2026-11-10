#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(body) = std::str::from_utf8(data) else {
        return;
    };
    match fluxplay_core::xmltv::parse_xmltv(body) {
        Ok(progs) => {
            // Billion-laughs style: refuse pathological programme fan-out.
            assert!(
                progs.len() <= body.len().saturating_add(32),
                "xmltv amplification: {} programmes from {} bytes",
                progs.len(),
                body.len()
            );
            for p in progs.iter().take(128) {
                assert!(p.title.len() < 64 * 1024);
                assert!(p.channel_id.len() < 8 * 1024);
                assert!(!p.title.contains('\0'));
                assert!(!p.channel_id.contains('\0'));
                // Start/stop must stay ordered (open-ended filled by parser).
                assert!(
                    p.start <= p.stop,
                    "xmltv start after stop: {:?} > {:?}",
                    p.start,
                    p.stop
                );
            }
        }
        Err(_) => {}
    }
});
