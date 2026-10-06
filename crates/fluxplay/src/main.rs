// Release builds on Windows are GUI apps: no console window behind FluxPlay.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(not(target_os = "android"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> iced::Result {
    #[cfg(not(target_os = "android"))]
    tune_allocator();
    fluxplay::run()
}

/// Give freed memory back to the OS right away. With mimalloc's default delayed purge
/// the catalogue load peak (tens of MB of JSON → structs, on worker threads) stayed
/// committed: ~70–130 MB more private memory at idle on a 100k-title catalogue.
#[cfg(not(target_os = "android"))]
fn tune_allocator() {
    // `mi_option_e` (mimalloc v3 `mimalloc.h`).
    const MI_OPTION_ARENA_EAGER_COMMIT: std::os::raw::c_int = 4;
    const MI_OPTION_PURGE_DELAY: std::os::raw::c_int = 15;
    extern "C" {
        fn mi_option_set(option: std::os::raw::c_int, value: std::os::raw::c_long);
    }
    // SAFETY: plain option setters of the linked mimalloc; no allocator state is touched.
    unsafe {
        mi_option_set(MI_OPTION_ARENA_EAGER_COMMIT, 0);
        mi_option_set(MI_OPTION_PURGE_DELAY, 0);
    }
}
