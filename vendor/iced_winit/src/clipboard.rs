//! Access the clipboard.
#![allow(unsafe_code)]

use crate::core::clipboard::Kind;
use std::sync::Arc;
use winit::window::{Window, WindowId};

/// A buffer for short-term storage and transfer within and between
/// applications.
pub struct Clipboard {
    state: State,
}

enum State {
    #[cfg(not(target_os = "android"))]
    Connected {
        clipboard: window_clipboard::Clipboard,
        // Held until drop to satisfy the safety invariants of
        // `window_clipboard::Clipboard`.
        //
        // Note that the field ordering is load-bearing.
        #[allow(dead_code)]
        window: Arc<Window>,
    },
    #[cfg(target_os = "android")]
    Android {
        #[allow(dead_code)]
        window: Arc<Window>,
    },
    Unavailable,
}

impl Clipboard {
    /// Creates a new [`Clipboard`] for the given window.
    pub fn connect(window: Arc<Window>) -> Clipboard {
        #[cfg(target_os = "android")]
        {
            // window_clipboard's Android backend is a stub (Unimplemented).
            // Use JNI ClipboardManager instead.
            return Clipboard {
                state: State::Android { window },
            };
        }

        #[cfg(not(target_os = "android"))]
        {
            // SAFETY: The window handle will stay alive throughout the entire
            // lifetime of the `window_clipboard::Clipboard` because we hold
            // the `Arc<Window>` together with `State`, and enum variant fields
            // get dropped in declaration order.
            #[allow(unsafe_code)]
            let clipboard =
                unsafe { window_clipboard::Clipboard::connect(&window) };

            let state = match clipboard {
                Ok(clipboard) => State::Connected { clipboard, window },
                Err(_) => State::Unavailable,
            };

            Clipboard { state }
        }
    }

    /// Creates a new [`Clipboard`] that isn't associated with a window.
    /// This clipboard will never contain a copied value.
    pub fn unconnected() -> Clipboard {
        Clipboard {
            state: State::Unavailable,
        }
    }

    /// Reads the current content of the [`Clipboard`] as text.
    pub fn read(&self, kind: Kind) -> Option<String> {
        match &self.state {
            #[cfg(not(target_os = "android"))]
            State::Connected { clipboard, .. } => match kind {
                Kind::Standard => clipboard.read().ok(),
                Kind::Primary => clipboard.read_primary().and_then(Result::ok),
            },
            #[cfg(target_os = "android")]
            State::Android { .. } => match kind {
                Kind::Standard | Kind::Primary => android_clipboard::read_text(),
            },
            State::Unavailable => None,
        }
    }

    /// Writes the given text contents to the [`Clipboard`].
    pub fn write(&mut self, kind: Kind, contents: String) {
        match &mut self.state {
            #[cfg(not(target_os = "android"))]
            State::Connected { clipboard, .. } => {
                let result = match kind {
                    Kind::Standard => clipboard.write(contents),
                    Kind::Primary => {
                        clipboard.write_primary(contents).unwrap_or(Ok(()))
                    }
                };

                match result {
                    Ok(()) => {}
                    Err(error) => {
                        log::warn!("error writing to clipboard: {error}");
                    }
                }
            }
            #[cfg(target_os = "android")]
            State::Android { .. } => {
                if let Err(error) = android_clipboard::write_text(&contents) {
                    log::warn!("error writing to android clipboard: {error}");
                }
                let _ = kind;
            }
            State::Unavailable => {}
        }
    }

    /// Returns the identifier of the window used to create the [`Clipboard`], if any.
    pub fn window_id(&self) -> Option<WindowId> {
        match &self.state {
            #[cfg(not(target_os = "android"))]
            State::Connected { window, .. } => Some(window.id()),
            #[cfg(target_os = "android")]
            State::Android { window } => Some(window.id()),
            State::Unavailable => None,
        }
    }
}

impl crate::core::Clipboard for Clipboard {
    fn read(&self, kind: Kind) -> Option<String> {
        self.read(kind)
    }

    fn write(&mut self, kind: Kind, contents: String) {
        self.write(kind, contents);
    }
}

#[cfg(target_os = "android")]
mod android_clipboard {
    //! Android `ClipboardManager` via JNI (window_clipboard stub is unimplemented).

    use jni::objects::{JObject, JString, JValue};
    use jni::JavaVM;

    pub fn read_text() -> Option<String> {
        read_text_inner().ok().flatten()
    }

    fn read_text_inner() -> Result<Option<String>, String> {
        let ctx = ndk_context::android_context();
        let vm =
            unsafe { JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| e.to_string())?;
        let activity_ptr = ctx.context() as jni::sys::jobject;
        let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
        let activity = unsafe { JObject::from_raw(activity_ptr) };

        let service = env
            .new_string("clipboard")
            .map_err(|e| e.to_string())?;
        let mgr = env
            .call_method(
                &activity,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[JValue::Object(&service)],
            )
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;
        if mgr.is_null() {
            return Ok(None);
        }

        let has = env
            .call_method(&mgr, "hasPrimaryClip", "()Z", &[])
            .map_err(|e| e.to_string())?
            .z()
            .map_err(|e| e.to_string())?;
        if !has {
            return Ok(None);
        }

        let clip = env
            .call_method(
                &mgr,
                "getPrimaryClip",
                "()Landroid/content/ClipData;",
                &[],
            )
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;
        if clip.is_null() {
            return Ok(None);
        }

        let item = env
            .call_method(
                &clip,
                "getItemAt",
                "(I)Landroid/content/ClipData$Item;",
                &[JValue::Int(0)],
            )
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;
        if item.is_null() {
            return Ok(None);
        }

        let coerced = env
            .call_method(
                &item,
                "coerceToText",
                "(Landroid/content/Context;)Ljava/lang/CharSequence;",
                &[JValue::Object(&activity)],
            )
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;
        if coerced.is_null() {
            return Ok(None);
        }

        let jstr = env
            .call_method(&coerced, "toString", "()Ljava/lang/String;", &[])
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;
        if jstr.is_null() {
            return Ok(None);
        }
        let jstring = JString::from(jstr);
        let s: String = env
            .get_string(&jstring)
            .map_err(|e| e.to_string())?
            .into();
        Ok(Some(s))
    }

    pub fn write_text(contents: &str) -> Result<(), String> {
        let ctx = ndk_context::android_context();
        let vm =
            unsafe { JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| e.to_string())?;
        let activity_ptr = ctx.context() as jni::sys::jobject;
        let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
        let activity = unsafe { JObject::from_raw(activity_ptr) };

        let service = env
            .new_string("clipboard")
            .map_err(|e| e.to_string())?;
        let mgr = env
            .call_method(
                &activity,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[JValue::Object(&service)],
            )
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;
        if mgr.is_null() {
            return Err("ClipboardManager null".into());
        }

        let label = env.new_string("FluxPlay").map_err(|e| e.to_string())?;
        let text = env.new_string(contents).map_err(|e| e.to_string())?;
        let clip_class = env
            .find_class("android/content/ClipData")
            .map_err(|e| e.to_string())?;
        let clip = env
            .call_static_method(
                clip_class,
                "newPlainText",
                "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Landroid/content/ClipData;",
                &[JValue::Object(&label), JValue::Object(&text)],
            )
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;
        let _ = env
            .call_method(
                &mgr,
                "setPrimaryClip",
                "(Landroid/content/ClipData;)V",
                &[JValue::Object(&clip)],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

