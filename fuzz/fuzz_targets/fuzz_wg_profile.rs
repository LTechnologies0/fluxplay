#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(conf) = std::str::from_utf8(data) else {
        return;
    };
    match fluxplay::fuzz_api::parse_wg_profile(conf) {
        Ok(profile) => {
            // Usable profiles must expose a finite MTU (DoS / bogus MTU=0).
            let mtu = profile.mtu();
            assert!((576..=9001).contains(&mtu), "mtu out of range: {mtu}");
        }
        Err(_) => {}
    }
});
