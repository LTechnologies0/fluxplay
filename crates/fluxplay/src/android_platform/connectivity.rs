//! Default network callback — edge detect offline → online for IPTV reconnect.

use std::sync::atomic::{AtomicBool, Ordering};

use super::jni_host as plat;

static ONLINE: AtomicBool = AtomicBool::new(true);
static REGISTERED: AtomicBool = AtomicBool::new(false);
static RECONNECT_PULSE: AtomicBool = AtomicBool::new(false);

pub fn on_network_changed(online: bool) {
    let was = ONLINE.swap(online, Ordering::SeqCst);
    if !was && online {
        RECONNECT_PULSE.store(true, Ordering::SeqCst);
        tracing::info!(target: "fluxplay::android", "network available — reconnect pulse");
    } else if was && !online {
        tracing::warn!(target: "fluxplay::android", "network lost");
    }
}

pub fn is_online() -> bool {
    ONLINE.load(Ordering::Relaxed)
}

pub fn take_reconnect_pulse() -> bool {
    RECONNECT_PULSE.swap(false, Ordering::SeqCst)
}

/// Register `ConnectivityManager.registerDefaultNetworkCallback` (once).
pub fn register_default_callback() {
    if REGISTERED.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some((vm, raw_ctx)) = plat::vm_context() else {
        REGISTERED.store(false, Ordering::SeqCst);
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        REGISTERED.store(false, Ordering::SeqCst);
        return;
    };
    let Some(cls) = plat::activity_class(&mut env, raw_ctx) else {
        REGISTERED.store(false, Ordering::SeqCst);
        return;
    };
    if env
        .call_static_method(cls, "registerDefaultNetworkCallbackNative", "()V", &[])
        .is_err()
    {
        plat::clear_ex(&mut env);
        REGISTERED.store(false, Ordering::SeqCst);
        tracing::warn!(target: "fluxplay::android", "network callback register failed");
    }
}

pub fn unregister_default_callback() {
    if !REGISTERED.load(Ordering::Relaxed) {
        return;
    }
    let Some((vm, raw_ctx)) = plat::vm_context() else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let Some(cls) = plat::activity_class(&mut env, raw_ctx) else {
        return;
    };
    let _ = env.call_static_method(cls, "unregisterDefaultNetworkCallbackNative", "()V", &[]);
    plat::clear_ex(&mut env);
    REGISTERED.store(false, Ordering::SeqCst);
}
