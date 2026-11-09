#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(raw) = std::str::from_utf8(data) else {
        return;
    };
    let _ = fluxplay::fuzz_api::parse_item_title(raw);
    let _ = fluxplay::fuzz_api::parse_category(raw);
    let _ = fluxplay::fuzz_api::parse_channel(raw);
    let _ = fluxplay::fuzz_api::display_title(raw);
    let _ = fluxplay::fuzz_api::display_channel(raw);
    let _ = fluxplay::fuzz_api::category_label(raw);
});
