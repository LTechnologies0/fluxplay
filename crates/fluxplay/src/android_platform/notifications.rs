//! Runtime permission for playback notifications (API 33+).

use std::sync::atomic::{AtomicBool, Ordering};

use super::jni_host as plat;

static ASKED: AtomicBool = AtomicBool::new(false);

/// Request `POST_NOTIFICATIONS` once when needed (MediaSession / lock screen controls).
pub fn ensure_post_notifications_permission() {
    if ASKED.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some((vm, raw_ctx)) = plat::vm_context() else {
        ASKED.store(false, Ordering::SeqCst);
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        ASKED.store(false, Ordering::SeqCst);
        return;
    };
    let Some(cls) = plat::activity_class(&mut env, raw_ctx) else {
        ASKED.store(false, Ordering::SeqCst);
        return;
    };
    if env
        .call_static_method(cls, "ensurePostNotificationsPermissionNative", "()V", &[])
        .is_err()
    {
        plat::clear_ex(&mut env);
        ASKED.store(false, Ordering::SeqCst);
    }
}
