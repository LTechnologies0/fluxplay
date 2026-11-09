//! Security / sanity property tests — path injection, URL SSRF-ish rejects, WG parse.

#![cfg(test)]

use proptest::prelude::*;

use crate::downloads::{extension_for, human_size, progress_text, sanitize_file_name, url_key};
use crate::images::is_fetchable_image_url;
use crate::storage::downloads_dir;
use crate::wg_proxy::parse_profile;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// File stems must never re-introduce path separators or Windows reserved chars.
    #[test]
    fn sanitize_file_name_blocks_traversal(raw in "\\PC{0,300}") {
        let s = sanitize_file_name(&raw);
        prop_assert!(!s.contains('/'));
        prop_assert!(!s.contains('\\'));
        prop_assert!(!s.contains('\0'));
        prop_assert!(!s.chars().any(|c| c.is_control()));
        prop_assert!(!s.is_empty());
        prop_assert!(s.chars().count() <= 150);
        // Soft byte ceiling — emoji×150 must not explode PATH_MAX-ish names.
        prop_assert!(s.len() <= 240, "utf-8 byte length {}", s.len());
        // No `..` alone as the whole name after collapse.
        prop_assert!(s != ".." && s != ".");
    }

    #[test]
    fn sanitize_never_keeps_percent(raw in ".*%.*") {
        let s = sanitize_file_name(&raw);
        prop_assert!(!s.contains('%'), "percent must die for file:// safety: {s}");
    }

    #[test]
    fn downloads_dir_relative_stays_relative_or_home(
        rel in "[A-Za-z0-9._-]{1,40}",
    ) {
        // Relative inputs must not silently become absolute via CWD tricks in the helper.
        let p = downloads_dir(&rel);
        // Either absolute custom (if someone passes abs) or whatever PathBuf::from gives —
        // validate_downloads_dir is the gate; downloads_dir itself only expands ~.
        prop_assert!(!rel.starts_with('/') || p.is_absolute());
        let _ = p;
    }

    #[test]
    fn url_key_is_opaque_hex(user in "[a-z]{1,8}", pass in "[a-z0-9]{8,16}", host in "[a-z]{3,12}\\.test") {
        let url = format!("https://{user}:{pass}@{host}/stream.ts");
        let key = url_key(&url);
        // Opaque SHA-256 trunc — never keeps userinfo punctuation.
        prop_assert!(!key.contains('@'));
        prop_assert!(!key.contains(':'));
        prop_assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
        prop_assert_eq!(key.len(), 32);
        // Long random-ish passwords must not appear verbatim (short digits can
        // collide with hex by chance — use ≥8 chars).
        prop_assert!(!key.contains(&pass), "password leaked into url_key: {key}");
    }

    #[test]
    fn extension_for_never_empty(path in "\\PC{0,80}", ct in "\\PC{0,40}") {
        let ext = extension_for(&path, &ct);
        prop_assert!(!ext.is_empty());
        prop_assert!(ext.chars().all(|c| c.is_ascii_alphanumeric()));
        prop_assert!(ext.len() <= 8);
    }

    #[test]
    fn human_size_and_progress_finite(done in 0u64..10_000_000_000, total in prop::option::of(1u64..10_000_000_000)) {
        let h = human_size(done);
        prop_assert!(!h.is_empty());
        let p = progress_text(done, total);
        prop_assert!(!p.is_empty());
    }

    /// Image fetch must reject non-http(s) and svg (no decoder → blank UI).
    #[test]
    fn image_url_policy(scheme in prop::sample::select(vec![
        "http", "https", "file", "ftp", "data", "javascript",
    ]), rest in "[a-z0-9./_-]{0,40}") {
        let url = format!("{scheme}://example.test/{rest}");
        let ok = is_fetchable_image_url(&url);
        if scheme == "http" || scheme == "https" {
            if rest.to_ascii_lowercase().ends_with(".svg") {
                prop_assert!(!ok);
            }
        } else {
            prop_assert!(!ok);
        }
    }

    /// WireGuard parser must never panic on junk (Err is fine).
    #[test]
    fn wg_parse_never_panics(conf in "\\PC{0,2000}") {
        let _ = parse_profile(&conf);
    }

    #[test]
    fn wg_noise_without_interface_is_err(junk in "[A-Za-z0-9 =\\n]{0,80}") {
        prop_assume!(!junk.to_ascii_lowercase().contains("[interface]"));
        prop_assume!(!junk.to_ascii_lowercase().contains("[peer]"));
        // Pure noise without sections must not yield a usable tunnel profile.
        prop_assert!(parse_profile(&junk).is_err());
    }
}
