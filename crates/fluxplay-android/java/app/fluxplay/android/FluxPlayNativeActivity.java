package app.fluxplay.android;

import android.app.Activity;
import android.app.NativeActivity;
import android.app.PictureInPictureParams;
import android.content.Intent;
import android.content.pm.ActivityInfo;
import android.content.res.Configuration;
import android.database.Cursor;
import android.graphics.PixelFormat;
import android.media.AudioAttributes;
import android.media.AudioFocusRequest;
import android.media.AudioManager;
import android.media.MediaCodecInfo;
import android.media.MediaCodecList;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.Looper;
import android.provider.OpenableColumns;
import android.util.DisplayMetrics;
import android.util.Log;
import android.util.Rational;
import android.view.Display;
import android.view.Gravity;
import android.view.Surface;
import android.view.SurfaceControl;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.view.View;
import android.view.ViewGroup;
import android.view.ViewParent;
import android.view.Window;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.view.WindowManager;
import android.widget.FrameLayout;

import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayList;
import java.util.List;

/**
 * NativeActivity subclass: SAF, PiP, insets, audio focus, immersive mode,
 * and a full-bleed SurfaceView above iced for MediaCodec zero-copy video (Phase A).
 * Display HDR/Hz + MediaCodec caps written to device_caps.json (Phases B–C).
 */
public class FluxPlayNativeActivity extends NativeActivity {
    private static final String TAG = "FluxPlay";
    private static final int REQ_OPEN = 4101;
    private static final int REQ_CREATE = 4102;

    static {
        // media-kit FFmpeg MediaCodec needs JNI/JavaVM registration via this helper.
        try {
            System.loadLibrary("mediakitandroidhelper");
            Log.i(TAG, "loaded mediakitandroidhelper");
        } catch (UnsatisfiedLinkError e) {
            Log.w(TAG, "mediakitandroidhelper not packaged — MediaCodec Surface may fail", e);
        }
    }

    private static FluxPlayNativeActivity sInstance;
    private static volatile boolean sInPip = false;
    /** Cached system insets [L,T,R,B,dpi] — updated on UI thread only. */
    private static final int[] sInsetsPx = new int[] {0, 0, 0, 0, 160};

    private String pendingCreateSource;
    private String pendingMime = "*/*";
    private AudioFocusRequest audioFocusRequest;
    private AudioManager.OnAudioFocusChangeListener audioFocusListener;

    /** MediaCodec SurfaceView above iced GLES (mpv-android wid model). */
    private SurfaceView videoSurfaceView;
    private volatile Surface videoSurface;
    private volatile boolean surfaceReady = false;
    private volatile int surfaceW = 0;
    private volatile int surfaceH = 0;
    private volatile int surfaceGen = 0;
    /** True while Rust wants Surface present — drives reattach after destroy/rotate. */
    private volatile boolean surfaceSessionWanted = false;
    private volatile boolean punchThroughEnabled = false;
    /** Draw SurfaceView above iced (chrome uses bottom inset). */
    private volatile boolean surfaceZOrderOnTop = true;
    private volatile float surfaceChromeInsetDp = 0f;
    /**
     * True after ensureVideoSurface until first holder callback.
     * Hard reattach during this window tears down the fresh view and makes bind fail.
     */
    private volatile boolean surfaceAwaitingFirst = false;
    /** After first ready size — ignore chrome inset / BLAST size thrash under MediaCodec. */
    private volatile boolean surfaceSizeLocked = false;
    /** Decoded video size (Rust pushes) — the view is fit to this ratio (letterbox). */
    private volatile int videoAspectW = 0;
    private volatile int videoAspectH = 0;
    /** MediaCodec buffer size (decoded, pre-SAR) + display size for cover transform. */
    private volatile int videoBufferW = 0;
    private volatile int videoBufferH = 0;
    private volatile boolean bufferTransformDirty = false;
    /** True when SurfaceView is attached via WindowManager (above NativeActivity GLES). */
    private volatile boolean videoSurfaceViaWm = false;
    private final Runnable reattachSurfaceRunnable = new Runnable() {
        @Override
        public void run() {
            if (sInstance != FluxPlayNativeActivity.this || !surfaceSessionWanted) {
                return;
            }
            if (surfaceReady && videoSurface != null && videoSurface.isValid()) {
                return;
            }
            // First create still in flight — never removeView (Pixel bind race).
            if (surfaceAwaitingFirst) {
                Log.i(TAG, "surface reattach skipped (awaiting first callback)");
                return;
            }
            if (!surfaceNeedsHardReattach()) {
                return;
            }
            try {
                // Attached View with a dead Surface never gets surfaceCreated again —
                // remove and recreate (rotate / NativeWindow churn on Pixel).
                removeVideoSurfaceView();
                videoSurface = null;
                surfaceReady = false;
                ensureVideoSurface();
                // Do not set TRANSLUCENT — Pixel NativeWindow resize storm.
                Log.i(TAG, "surface reattach forced gen=" + surfaceGen
                        + " ready=" + surfaceReady);
            } catch (Exception e) {
                Log.e(TAG, "surface reattach", e);
            }
        }
    };

    /**
     * Hard reattach only when the view is laid out but its Surface is truly gone —
     * not while waiting for the first surfaceChanged size.
     */
    private boolean surfaceNeedsHardReattach() {
        if (videoSurfaceView == null) {
            return false;
        }
        if (surfaceReady && videoSurface != null && videoSurface.isValid()) {
            return false;
        }
        if (surfaceAwaitingFirst) {
            return false;
        }
        SurfaceHolder holder = videoSurfaceView.getHolder();
        Surface s = holder != null ? holder.getSurface() : null;
        if (s != null && s.isValid()) {
            // Surface exists; size/ready pending via surfaceChanged — wait.
            return false;
        }
        // Laid-out view with no valid Surface → need remove+recreate.
        return videoSurfaceView.getWindowToken() != null;
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        sInstance = this;
        // TRANSLUCENT before NativeActivity creates its BLAST surface — the video
        // SurfaceView lives BEHIND the window (layer system): iced clears the video
        // stage with alpha=0 so MediaCodec shows through; chrome draws on top.
        // Set once here — runtime setFormat recreates the BLAST surface (storm).
        try {
            getWindow().setFormat(PixelFormat.TRANSLUCENT);
        } catch (Throwable ignored) {
        }
        super.onCreate(savedInstanceState);
        try {
            Window window = getWindow();
            // Theme is opaque (windowIsTranslucent=false). Never setFormat(OPAQUE).
            if (Build.VERSION.SDK_INT >= 26) {
                try {
                    window.setColorMode(ActivityInfo.COLOR_MODE_DEFAULT);
                } catch (Throwable ignored) {
                }
            }
            // Do NOT FLAG_KEEP_SCREEN_ON here — PlayerTick only runs while playing, so
            // a boot-time flag would hold the Pixel OLED awake during idle browse.
            View decor = window.getDecorView();
            DisplayMetrics dm = getResources().getDisplayMetrics();
            synchronized (sInsetsPx) {
                sInsetsPx[4] = dm.densityDpi;
            }
            if (Build.VERSION.SDK_INT >= 20) {
                decor.setOnApplyWindowInsetsListener((v, insets) -> {
                    cacheInsetsFrom(insets);
                    return v.onApplyWindowInsets(insets);
                });
            }
            // SurfaceView is created lazily at play time (setVideoSurfaceVisible) —
            // attaching in onCreate races NativeWindowDestroyed during iced boot and
            // leaves a dead Surface (create→destroy within ~1s on Pixel).
            decor.post(() -> {
                refreshInsetsFromDecor();
                writeDeviceCaps();
                // First CreateWindow can still see 0×0 before layout — poke once.
                if (decor.getWidth() <= 0 || decor.getHeight() <= 0) {
                    decor.requestLayout();
                }
            });
        } catch (Exception e) {
            Log.e(TAG, "onCreate insets/surface", e);
        }
    }

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (hasFocus) {
            try {
                refreshInsetsFromDecor();
                writeDeviceCaps();
                if (surfaceSessionWanted && surfaceNeedsHardReattach()) {
                    scheduleSurfaceReattach(0);
                }
            } catch (Exception e) {
                Log.e(TAG, "onWindowFocusChanged insets", e);
            }
        }
    }

    @Override
    public void onConfigurationChanged(Configuration newConfig) {
        super.onConfigurationChanged(newConfig);
        try {
            DisplayMetrics dm = getResources().getDisplayMetrics();
            synchronized (sInsetsPx) {
                sInsetsPx[4] = dm.densityDpi;
            }
            refreshInsetsFromDecor();
            writeDeviceCaps();
            // Rotation: re-cover with the new screen dims (view is MATCH_PARENT).
            bufferTransformDirty = true;
            applyVideoCoverTransform();
            if (videoSurfaceView != null) {
                videoSurfaceView.requestLayout();
            }
            if (surfaceSessionWanted && surfaceNeedsHardReattach()) {
                scheduleSurfaceReattach(50);
            }
            int orient = newConfig.orientation;
            String o = orient == Configuration.ORIENTATION_LANDSCAPE ? "landscape"
                    : orient == Configuration.ORIENTATION_PORTRAIT ? "portrait" : "other";
            Log.i(TAG, "configChanged " + o + " " + dm.widthPixels + "x" + dm.heightPixels
                    + " dpi=" + dm.densityDpi + " surfaceSession=" + surfaceSessionWanted);
        } catch (Exception e) {
            Log.e(TAG, "onConfigurationChanged", e);
        }
    }

    @Override
    protected void onResume() {
        super.onResume();
        try {
            refreshInsetsFromDecor();
            writeDeviceCaps();
            // Poke ViewRoot only when the native surface is missing after resume
            // (NO_SURFACE / black iced). Avoid requestLayout every resume — that
            // can feed NativeWindowResized storms on Pixel.
            View decor = getWindow().getDecorView();
            decor.post(() -> {
                try {
                    if (decor.getWidth() <= 0 || decor.getHeight() <= 0) {
                        decor.requestLayout();
                        decor.invalidate();
                    }
                } catch (Throwable ignored) {
                }
            });
            if (surfaceSessionWanted && surfaceNeedsHardReattach()) {
                scheduleSurfaceReattach(100);
            }
        } catch (Exception e) {
            Log.e(TAG, "onResume stabilize", e);
        }
    }

    @Override
    protected void onPause() {
        // Keep SurfaceView attached across brief Pause (rotate / PIP / focus blip).
        // surfaceDestroyed will clear ready; onResume reattaches if session wanted.
        super.onPause();
    }

    @Override
    protected void onDestroy() {
        View decor = null;
        try {
            decor = getWindow().getDecorView();
            decor.removeCallbacks(reattachSurfaceRunnable);
        } catch (Throwable ignored) {
        }
        surfaceSessionWanted = false;
        removeVideoSurfaceView();
        if (sInstance == this) {
            abandonAudioFocusInner();
            writeAudioFocusFlag(false);
            sInstance = null;
        }
        super.onDestroy();
    }

    private void scheduleSurfaceReattach(long delayMs) {
        try {
            View decor = getWindow().getDecorView();
            decor.removeCallbacks(reattachSurfaceRunnable);
            if (delayMs <= 0) {
                decor.post(reattachSurfaceRunnable);
            } else {
                decor.postDelayed(reattachSurfaceRunnable, delayMs);
            }
        } catch (Exception e) {
            Log.e(TAG, "scheduleSurfaceReattach", e);
        }
    }

    @Override
    public void onPictureInPictureModeChanged(boolean isInPictureInPictureMode) {
        super.onPictureInPictureModeChanged(isInPictureInPictureMode);
        sInPip = isInPictureInPictureMode;
        writePipFlag(isInPictureInPictureMode);
        try {
            refreshInsetsFromDecor();
            if (surfaceSessionWanted && surfaceNeedsHardReattach()) {
                scheduleSurfaceReattach(0);
            }
        } catch (Exception e) {
            Log.e(TAG, "onPictureInPictureModeChanged", e);
        }
    }

    @Override
    public void onPictureInPictureModeChanged(boolean isInPictureInPictureMode,
                                              Configuration newConfig) {
        if (Build.VERSION.SDK_INT >= 26) {
            super.onPictureInPictureModeChanged(isInPictureInPictureMode, newConfig);
        }
        sInPip = isInPictureInPictureMode;
        writePipFlag(isInPictureInPictureMode);
        try {
            refreshInsetsFromDecor();
            if (surfaceSessionWanted && surfaceNeedsHardReattach()) {
                scheduleSurfaceReattach(0);
            }
        } catch (Exception e) {
            Log.e(TAG, "onPictureInPictureModeChanged(config)", e);
        }
    }

    /** Called from Rust via JNI. mode open|create; mime e.g. text/plain or star/star.
     * @return false if Activity not ready (Rust must clear SAF_PENDING). */
    public static boolean startSaf(String mode, String mime, String createSourcePath) {
        FluxPlayNativeActivity a = sInstance;
        if (a == null) {
            Log.w(TAG, "startSaf: no activity");
            return false;
        }
        a.runOnUiThread(() -> a.launchSaf(mode, mime, createSourcePath));
        return true;
    }

    private void launchSaf(String mode, String mime, String createSourcePath) {
        pendingMime = (mime == null || mime.isEmpty()) ? "*/*" : mime;
        try {
            if ("create".equals(mode)) {
                pendingCreateSource = createSourcePath;
                Intent intent = new Intent(Intent.ACTION_CREATE_DOCUMENT);
                intent.addCategory(Intent.CATEGORY_OPENABLE);
                intent.setType(pendingMime);
                intent.putExtra(Intent.EXTRA_TITLE, new File(createSourcePath).getName());
                startActivityForResult(intent, REQ_CREATE);
            } else {
                pendingCreateSource = null;
                Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
                intent.addCategory(Intent.CATEGORY_OPENABLE);
                intent.setType(pendingMime);
                intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
                startActivityForResult(intent, REQ_OPEN);
            }
        } catch (Exception e) {
            Log.e(TAG, "launchSaf failed", e);
            writeInboxMeta("error", e.getMessage(), null, null);
        }
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode != REQ_OPEN && requestCode != REQ_CREATE) {
            return;
        }
        if (resultCode != Activity.RESULT_OK || data == null || data.getData() == null) {
            writeInboxMeta("cancel", null, null, null);
            return;
        }
        Uri uri = data.getData();
        try {
            if (requestCode == REQ_OPEN) {
                String name = queryDisplayName(uri);
                File out = durableImportFile(name);
                copyUriToFile(uri, out);
                writeInboxMeta("open", name, out.getAbsolutePath(), pendingMime);
            } else if (requestCode == REQ_CREATE) {
                if (pendingCreateSource != null) {
                    copyFileToUri(new File(pendingCreateSource), uri);
                }
                writeInboxMeta("create", queryDisplayName(uri), pendingCreateSource, pendingMime);
            }
        } catch (Exception e) {
            Log.e(TAG, "SAF result failed", e);
            writeInboxMeta("error", e.getMessage(), null, null);
        }
    }

    public static void enterPip(int width, int height) {
        FluxPlayNativeActivity a = sInstance;
        if (a == null || Build.VERSION.SDK_INT < 26) {
            return;
        }
        a.runOnUiThread(() -> {
            try {
                Rational aspect = new Rational(Math.max(width, 1), Math.max(height, 1));
                PictureInPictureParams params = new PictureInPictureParams.Builder()
                        .setAspectRatio(aspect)
                        .build();
                a.enterPictureInPictureMode(params);
            } catch (Exception e) {
                Log.e(TAG, "enterPip failed", e);
            }
        });
    }

    /** Bring the Activity out of system PiP (no public exit API — reorder to front). */
    public static void exitPip() {
        FluxPlayNativeActivity a = sInstance;
        if (a == null || Build.VERSION.SDK_INT < 26) {
            return;
        }
        a.runOnUiThread(() -> {
            try {
                if (!a.isInPictureInPictureMode()) {
                    return;
                }
                Intent i = new Intent(a, FluxPlayNativeActivity.class);
                i.addFlags(Intent.FLAG_ACTIVITY_REORDER_TO_FRONT
                        | Intent.FLAG_ACTIVITY_SINGLE_TOP
                        | Intent.FLAG_ACTIVITY_NEW_TASK);
                a.startActivity(i);
            } catch (Exception e) {
                Log.e(TAG, "exitPip failed", e);
            }
        });
    }

    public static boolean isInPip() {
        FluxPlayNativeActivity a = sInstance;
        if (a != null && Build.VERSION.SDK_INT >= 24) {
            try {
                sInPip = a.isInPictureInPictureMode();
            } catch (Exception ignored) {
            }
        }
        return sInPip;
    }

    public static boolean isNightMode() {
        FluxPlayNativeActivity a = sInstance;
        if (a == null) {
            return false;
        }
        int night = a.getResources().getConfiguration().uiMode & Configuration.UI_MODE_NIGHT_MASK;
        return night == Configuration.UI_MODE_NIGHT_YES;
    }

    public static boolean isTelevision() {
        FluxPlayNativeActivity a = sInstance;
        if (a == null) {
            return false;
        }
        int type = a.getResources().getConfiguration().uiMode & Configuration.UI_MODE_TYPE_MASK;
        return type == Configuration.UI_MODE_TYPE_TELEVISION;
    }

    private static void cacheInsetsFrom(WindowInsets insets) {
        if (insets == null) {
            return;
        }
        int left = 0, top = 0, right = 0, bottom = 0;
        if (Build.VERSION.SDK_INT >= 30) {
            int types = WindowInsets.Type.systemBars()
                    | WindowInsets.Type.displayCutout();
            // Immersive hide() zeros getInsets(systemBars) — chrome then sits under the
            // Pixel gesture handle. Ignoring visibility keeps the real bar sizes.
            android.graphics.Insets bars = insets.getInsetsIgnoringVisibility(types);
            left = bars.left;
            top = bars.top;
            right = bars.right;
            bottom = bars.bottom;
        } else {
            left = insets.getSystemWindowInsetLeft();
            top = insets.getSystemWindowInsetTop();
            right = insets.getSystemWindowInsetRight();
            bottom = insets.getSystemWindowInsetBottom();
        }
        synchronized (sInsetsPx) {
            sInsetsPx[0] = left;
            sInsetsPx[1] = top;
            sInsetsPx[2] = right;
            sInsetsPx[3] = bottom;
        }
    }

    private void refreshInsetsFromDecor() {
        try {
            View decor = getWindow().getDecorView();
            DisplayMetrics dm = getResources().getDisplayMetrics();
            synchronized (sInsetsPx) {
                sInsetsPx[4] = dm.densityDpi;
            }
            WindowInsets insets = decor.getRootWindowInsets();
            if (insets != null) {
                cacheInsetsFrom(insets);
            }
            // Gesture-nav / first frame: IgnoringVisibility can still leave zeros before
            // the first insets dispatch — seed status/nav dimen so chrome is not under bars.
            {
                int nav = systemDimenPx("navigation_bar_height");
                int status = systemDimenPx("status_bar_height");
                synchronized (sInsetsPx) {
                    if (sInsetsPx[3] <= 0 && nav > 0) {
                        sInsetsPx[3] = nav;
                    }
                    if (sInsetsPx[1] <= 0 && status > 0) {
                        sInsetsPx[1] = status;
                    }
                }
            }
        } catch (Exception e) {
            Log.e(TAG, "refreshInsetsFromDecor", e);
        }
    }

    private int systemDimenPx(String name) {
        try {
            int id = getResources().getIdentifier(name, "dimen", "android");
            if (id != 0) {
                return getResources().getDimensionPixelSize(id);
            }
        } catch (Exception ignored) {
        }
        return 0;
    }

    /**
     * Returns insets in px: [left, top, right, bottom, densityDpi].
     * Safe from any thread — reads UI-thread cache (no Window access off-UI).
     */
    public static int[] systemInsetsPx() {
        FluxPlayNativeActivity a = sInstance;
        int dpi = 160;
        if (a != null) {
            try {
                dpi = a.getResources().getDisplayMetrics().densityDpi;
                // Kick a UI refresh when cache looks empty (first frames / NativeActivity).
                boolean empty;
                synchronized (sInsetsPx) {
                    empty = sInsetsPx[0] == 0 && sInsetsPx[1] == 0
                            && sInsetsPx[2] == 0 && sInsetsPx[3] == 0;
                }
                if (empty) {
                    a.runOnUiThread(a::refreshInsetsFromDecor);
                }
            } catch (Exception ignored) {
            }
        }
        synchronized (sInsetsPx) {
            return new int[] {sInsetsPx[0], sInsetsPx[1], sInsetsPx[2], sInsetsPx[3], dpi};
        }
    }

    /** Explicit UI-thread insets refresh (Rust SafPoll). */
    public static void refreshSystemInsets() {
        FluxPlayNativeActivity a = sInstance;
        if (a == null) {
            return;
        }
        if (Looper.myLooper() == Looper.getMainLooper()) {
            a.refreshInsetsFromDecor();
            return;
        }
        final java.util.concurrent.CountDownLatch done =
                new java.util.concurrent.CountDownLatch(1);
        a.runOnUiThread(() -> {
            try {
                a.refreshInsetsFromDecor();
            } finally {
                done.countDown();
            }
        });
        try {
            done.await(250, java.util.concurrent.TimeUnit.MILLISECONDS);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
    }

    public static void setKeepScreenOn(boolean enable) {
        FluxPlayNativeActivity a = sInstance;
        if (a == null) {
            return;
        }
        a.runOnUiThread(() -> {
            if (enable) {
                a.getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
            } else {
                a.getWindow().clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
            }
        });
    }

    /**
     * Hide/show system bars via WindowInsetsController (API 30+); legacy flags below.
     * Named setImmersiveMode — Activity already has instance setImmersive(boolean).
     */
    public static void setImmersiveMode(boolean immersive) {
        FluxPlayNativeActivity a = sInstance;
        if (a == null) {
            return;
        }
        a.runOnUiThread(() -> {
            try {
                if (Build.VERSION.SDK_INT >= 30) {
                    WindowInsetsController c = a.getWindow().getInsetsController();
                    if (c == null) {
                        return;
                    }
                    int types = WindowInsets.Type.systemBars();
                    if (immersive) {
                        c.hide(types);
                        c.setSystemBarsBehavior(
                                WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
                    } else {
                        c.show(types);
                    }
                } else {
                    View decor = a.getWindow().getDecorView();
                    if (immersive) {
                        decor.setSystemUiVisibility(
                                View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY
                                        | View.SYSTEM_UI_FLAG_FULLSCREEN
                                        | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                                        | View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                                        | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                                        | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION);
                    } else {
                        decor.setSystemUiVisibility(View.SYSTEM_UI_FLAG_VISIBLE);
                    }
                }
            } catch (Exception e) {
                Log.e(TAG, "setImmersiveMode failed", e);
            }
        });
    }

    public static void finishActivity() {
        FluxPlayNativeActivity a = sInstance;
        if (a == null) {
            return;
        }
        a.runOnUiThread(a::finish);
    }

    /** ACTION_VIEW from Activity UI thread (ndk_context is often Application). */
    public static void openUrl(String url, String mime) {
        FluxPlayNativeActivity a = sInstance;
        if (a == null || url == null || url.isEmpty()) {
            return;
        }
        final String u = url;
        final String m = mime;
        a.runOnUiThread(() -> {
            try {
                Intent intent = new Intent(Intent.ACTION_VIEW, Uri.parse(u));
                if (m != null && !m.isEmpty()) {
                    intent.setDataAndType(Uri.parse(u), m);
                }
                a.startActivity(intent);
            } catch (Exception e) {
                Log.e(TAG, "openUrl failed", e);
            }
        });
    }

    public static void requestAudioFocus() {
        FluxPlayNativeActivity a = sInstance;
        if (a == null) {
            return;
        }
        // Sync when possible so Rust poll after request sees GRANTED before open_channel.
        if (Looper.myLooper() == Looper.getMainLooper()) {
            a.requestAudioFocusInner();
            return;
        }
        final java.util.concurrent.CountDownLatch done =
                new java.util.concurrent.CountDownLatch(1);
        a.runOnUiThread(() -> {
            try {
                a.requestAudioFocusInner();
            } finally {
                done.countDown();
            }
        });
        try {
            done.await(750, java.util.concurrent.TimeUnit.MILLISECONDS);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
    }

    public static void abandonAudioFocus() {
        FluxPlayNativeActivity a = sInstance;
        if (a == null) {
            return;
        }
        a.runOnUiThread(a::abandonAudioFocusInner);
    }

    private void requestAudioFocusInner() {
        try {
            AudioManager am = (AudioManager) getSystemService(AUDIO_SERVICE);
            if (am == null) {
                return;
            }
            if (audioFocusListener == null) {
                audioFocusListener = focusChange -> {
                    if (focusChange == AudioManager.AUDIOFOCUS_LOSS
                            || focusChange == AudioManager.AUDIOFOCUS_LOSS_TRANSIENT) {
                        writeAudioFocusFlag(false);
                        Log.i(TAG, "audio focus lost: " + focusChange);
                    } else if (focusChange == AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK) {
                        // Keep held=true — Rust should not hard-pause for duck.
                        Log.i(TAG, "audio focus duck");
                    } else if (focusChange == AudioManager.AUDIOFOCUS_GAIN) {
                        writeAudioFocusFlag(true);
                    }
                };
            }
            int granted;
            if (Build.VERSION.SDK_INT >= 26) {
                if (audioFocusRequest == null) {
                    AudioAttributes attrs = new AudioAttributes.Builder()
                            .setUsage(AudioAttributes.USAGE_MEDIA)
                            .setContentType(AudioAttributes.CONTENT_TYPE_MOVIE)
                            .build();
                    audioFocusRequest = new AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
                            .setAudioAttributes(attrs)
                            .setOnAudioFocusChangeListener(audioFocusListener)
                            .setWillPauseWhenDucked(false)
                            .setAcceptsDelayedFocusGain(true)
                            .build();
                }
                granted = am.requestAudioFocus(audioFocusRequest);
            } else {
                granted = am.requestAudioFocus(
                        audioFocusListener,
                        AudioManager.STREAM_MUSIC,
                        AudioManager.AUDIOFOCUS_GAIN);
            }
            if (granted == AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
                writeAudioFocusFlag(true);
            } else if (granted == AudioManager.AUDIOFOCUS_REQUEST_DELAYED) {
                // Neither true (fake hold) nor false (kills OpenSL) — wait for GAIN.
                Log.i(TAG, "audio focus DELAYED — waiting for GAIN");
            } else {
                writeAudioFocusFlag(false);
            }
        } catch (Exception e) {
            Log.e(TAG, "requestAudioFocus failed", e);
        }
    }

    private void abandonAudioFocusInner() {
        try {
            AudioManager am = (AudioManager) getSystemService(AUDIO_SERVICE);
            if (am == null) {
                return;
            }
            if (Build.VERSION.SDK_INT >= 26 && audioFocusRequest != null) {
                am.abandonAudioFocusRequest(audioFocusRequest);
            } else if (audioFocusListener != null) {
                am.abandonAudioFocus(audioFocusListener);
            }
            writeAudioFocusFlag(false);
        } catch (Exception e) {
            Log.e(TAG, "abandonAudioFocus failed", e);
        }
    }

    private File safInboxDir() {
        File dir = new File(getFilesDir(), "saf_inbox");
        //noinspection ResultOfMethodCallIgnored
        dir.mkdirs();
        return dir;
    }

    private File importsDir() {
        File dir = new File(getFilesDir(), "imports");
        //noinspection ResultOfMethodCallIgnored
        dir.mkdirs();
        return dir;
    }

    private File durableImportFile(String displayName) {
        String sanitized = sanitizeFileName(displayName);
        String stamp = String.valueOf(System.currentTimeMillis());
        return new File(importsDir(), stamp + "_" + sanitized);
    }

    private static String sanitizeFileName(String name) {
        if (name == null || name.isEmpty()) {
            return "picked.bin";
        }
        String s = name.replaceAll("[\\\\/:*?\"<>|\\x00-\\x1f]", "_").trim();
        if (s.isEmpty() || s.equals(".") || s.equals("..")) {
            return "picked.bin";
        }
        if (s.length() > 180) {
            s = s.substring(0, 180);
        }
        return s;
    }

    private void writePipFlag(boolean inPip) {
        try {
            File meta = new File(safInboxDir(), "pip.json");
            String json = "{\"in_pip\":" + (inPip ? "true" : "false") + "}";
            try (FileOutputStream fos = new FileOutputStream(meta)) {
                fos.write(json.getBytes("UTF-8"));
            }
        } catch (Exception e) {
            Log.e(TAG, "writePipFlag", e);
        }
    }

    private void writeAudioFocusFlag(boolean held) {
        try {
            File meta = new File(safInboxDir(), "audio_focus.json");
            String json = "{\"held\":" + (held ? "true" : "false") + "}";
            try (FileOutputStream fos = new FileOutputStream(meta)) {
                fos.write(json.getBytes("UTF-8"));
            }
        } catch (Exception e) {
            Log.e(TAG, "writeAudioFocusFlag", e);
        }
    }

    /** Detach SurfaceView from WindowManager or content hierarchy. */
    private void removeVideoSurfaceView() {
        if (videoSurfaceView == null) {
            return;
        }
        try {
            if (videoSurfaceViaWm) {
                getWindowManager().removeViewImmediate(videoSurfaceView);
            } else {
                ViewParent parent = videoSurfaceView.getParent();
                if (parent instanceof ViewGroup) {
                    ((ViewGroup) parent).removeView(videoSurfaceView);
                }
            }
        } catch (Exception e) {
            Log.w(TAG, "removeVideoSurfaceView", e);
        }
        videoSurfaceView = null;
        videoSurfaceViaWm = false;
        surfaceSizeLocked = false;
        surfaceAwaitingFirst = false;
    }

    /**
     * Create SurfaceView as a CHILD of the content view, z-ordered BEHIND the
     * translucent NativeActivity window (layer system). iced clears the video stage
     * with alpha=0 so MediaCodec shows through; player chrome composites on top.
     * The view is always MATCH_PARENT — cover fitting is done via SurfaceControl
     * transform (setVideoBufferSize), never by relayouting the view.
     */
    private void ensureVideoSurface() {
        // Already attached — keep it (reattach path calls remove first).
        if (videoSurfaceView != null) {
            if (videoSurfaceView.getParent() != null) {
                return;
            }
            videoSurfaceView = null;
        }
        videoSurface = null;
        surfaceReady = false;
        try {
            videoSurfaceView = new SurfaceView(this);
            videoSurfaceView.setVisibility(
                    surfaceSessionWanted ? View.VISIBLE : View.GONE);
            videoSurfaceView.getHolder().setFormat(PixelFormat.OPAQUE);
            videoSurfaceView.addOnLayoutChangeListener((v, l, t, r, b, ol, ot, orr, ob) -> {
                if (v == videoSurfaceView) {
                    applyVideoCoverTransform();
                }
            });
            videoSurfaceView.getHolder().addCallback(new SurfaceHolder.Callback() {
                @Override
                public void surfaceCreated(SurfaceHolder holder) {
                    videoSurface = holder.getSurface();
                    surfaceAwaitingFirst = false;
                    surfaceGen++;
                    int vw = videoSurfaceView != null ? videoSurfaceView.getWidth() : 0;
                    int vh = videoSurfaceView != null ? videoSurfaceView.getHeight() : 0;
                    if (vw >= 64 && vh >= 64 && videoSurface != null && videoSurface.isValid()) {
                        surfaceW = vw;
                        surfaceH = vh;
                        surfaceReady = true;
                    } else {
                        surfaceReady = false;
                        surfaceW = 0;
                        surfaceH = 0;
                    }
                    bufferTransformDirty = true;
                    applyVideoCoverTransform();
                    writeSurfaceState();
                    writeDeviceCaps();
                    Log.i(TAG, "video SurfaceView created gen=" + surfaceGen
                            + " " + surfaceW + "x" + surfaceH + " ready=" + surfaceReady);
                }

                @Override
                public void surfaceChanged(SurfaceHolder holder, int format, int width, int height) {
                    surfaceAwaitingFirst = false;
                    surfaceW = Math.max(width, 0);
                    surfaceH = Math.max(height, 0);
                    videoSurface = holder.getSurface();
                    surfaceReady = videoSurface != null && videoSurface.isValid()
                            && surfaceW >= 64 && surfaceH >= 64;
                    bufferTransformDirty = true;
                    applyVideoCoverTransform();
                    writeSurfaceState();
                    if (surfaceReady) {
                        writeDeviceCaps();
                    }
                    Log.i(TAG, "video SurfaceView changed " + surfaceW + "x" + surfaceH
                            + " ready=" + surfaceReady);
                }

                @Override
                public void surfaceDestroyed(SurfaceHolder holder) {
                    surfaceAwaitingFirst = false;
                    surfaceSizeLocked = false;
                    surfaceReady = false;
                    videoSurface = null;
                    surfaceW = 0;
                    surfaceH = 0;
                    writeSurfaceState();
                    writeDeviceCaps();
                    Log.i(TAG, "video SurfaceView destroyed session=" + surfaceSessionWanted);
                    if (surfaceSessionWanted) {
                        scheduleSurfaceReattach(250);
                    }
                }
            });
            FrameLayout content = getWindow().getDecorView().findViewById(android.R.id.content);
            FrameLayout.LayoutParams lp = new FrameLayout.LayoutParams(
                    ViewGroup.LayoutParams.MATCH_PARENT,
                    ViewGroup.LayoutParams.MATCH_PARENT);
            content.addView(videoSurfaceView, 0, lp);
            videoSurfaceViaWm = false;
            surfaceAwaitingFirst = true;
            if (!surfaceSessionWanted) {
                videoSurfaceView.setVisibility(View.GONE);
            }
            Log.i(TAG, "video SurfaceView attached as child (behind window) wanted="
                    + surfaceSessionWanted);
        } catch (Exception e) {
            Log.e(TAG, "ensureVideoSurface failed", e);
        }
    }

    private void writeSurfaceState() {
        try {
            String json = "{"
                    + "\"ready\":" + (surfaceReady ? "true" : "false") + ","
                    + "\"w\":" + surfaceW + ","
                    + "\"h\":" + surfaceH + ","
                    + "\"gen\":" + surfaceGen
                    + "}";
            File meta = new File(safInboxDir(), "surface_state.json");
            try (FileOutputStream fos = new FileOutputStream(meta)) {
                fos.write(json.getBytes("UTF-8"));
            }
        } catch (Exception e) {
            Log.e(TAG, "writeSurfaceState", e);
        }
    }

    public static void setVideoSurfaceZOrderOnTop(boolean onTop) {
        FluxPlayNativeActivity a = sInstance;
        if (a == null) {
            return;
        }
        a.runOnUiThread(() -> {
            try {
                a.surfaceZOrderOnTop = false;
                if (a.videoSurfaceView != null) {
                    // Layer system: video must stay BEHIND the translucent window.
                    // setZOrderOnTop(true) would punch it above iced chrome and
                    // re-create the "video hides the controls" conflict.
                    a.videoSurfaceView.setZOrderOnTop(false);
                    if (Build.VERSION.SDK_INT >= 21) {
                        a.videoSurfaceView.setZOrderMediaOverlay(false);
                    }
                }
            } catch (Exception e) {
                Log.e(TAG, "setVideoSurfaceZOrderOnTop", e);
            }
        });
    }

    /**
     * Legacy chrome-inset hook. NO-OP by design: the video SurfaceView is a
     * full-screen child BEHIND the translucent window, and the player chrome is
     * an iced overlay ON TOP. Toggling chrome must never resize the video —
     * that was the shrink/hide bug the layer system eliminated.
     */
    public static void layoutVideoSurfaceChromeInsetDp(float bottomDp) {
        // Intentionally ignored (kept for JNI signature stability).
    }

    /**
     * Keep SurfaceView only while a Surface present session is wanted.
     * Hidden (not removed) on soft/idle so returning to play reuses the same Surface.
     */
    public static void setVideoSurfaceVisible(boolean visible) {
        FluxPlayNativeActivity a = sInstance;
        if (a == null) {
            return;
        }
        // Set wanted flag synchronously so late UI runnables see the latest intent.
        a.surfaceSessionWanted = visible;
        a.runOnUiThread(() -> {
            try {
                if (visible) {
                    // Stale runnable: soft path may have already cancelled the session.
                    if (!a.surfaceSessionWanted) {
                        return;
                    }
                    a.ensureVideoSurface();
                    if (!a.surfaceSessionWanted) {
                        if (a.videoSurfaceView != null) {
                            a.videoSurfaceView.setVisibility(View.GONE);
                        }
                        return;
                    }
                    if (a.videoSurfaceView != null
                            && a.videoSurfaceView.getVisibility() != View.VISIBLE) {
                        a.videoSurfaceView.setVisibility(View.VISIBLE);
                    }
                    // Only hard-reattach when Surface is dead — never tear down first create.
                    if (a.surfaceNeedsHardReattach()) {
                        a.scheduleSurfaceReattach(0);
                    }
                } else {
                    View decor = a.getWindow().getDecorView();
                    decor.removeCallbacks(a.reattachSurfaceRunnable);
                    a.surfaceAwaitingFirst = false;
                    a.surfaceSizeLocked = false;
                    // Hide overlay (WM) — keep it attached so re-show reuses the Surface.
                    if (a.videoSurfaceView != null) {
                        a.videoSurfaceView.setVisibility(View.GONE);
                    }
                    a.surfaceReady = false;
                    a.videoSurface = null;
                    a.surfaceW = 0;
                    a.surfaceH = 0;
                    a.writeSurfaceState();
                }
            } catch (Exception e) {
                Log.e(TAG, "setVideoSurfaceVisible", e);
            }
        });
    }

    /**
     * Toggle GLES window punch-through for MediaCodec Surface under iced.
     * Opaque otherwise — stops Pixel NativeWindow resize/redraw storms.
     */
    public static void setWindowPunchThrough(boolean enable) {
        FluxPlayNativeActivity a = sInstance;
        if (a == null) {
            return;
        }
        a.runOnUiThread(() -> {
            try {
                a.punchThroughEnabled = enable;
                // Never call Window.setFormat here — OPAQUE/TRANSLUCENT both recreate
                // the NativeActivity BLAST surface and can leave iced with NO_SURFACE.
                // Punch-through remains Soft-only until a non-format-change path exists.
                Log.i(TAG, "window punch-through=" + enable + " (no setFormat)");
            } catch (Exception e) {
                Log.e(TAG, "setWindowPunchThrough", e);
            }
        });
    }

    /** Surface generation counter — Rust rebinds wid when this changes. */
    public static int getSurfaceGeneration() {
        FluxPlayNativeActivity a = sInstance;
        return a == null ? 0 : a.surfaceGen;
    }

    /**
     * Full stabilize pass: insets, caps, Surface session, display mode.
     * Call after rotate / resume / play bind.
     */
    public static void stabilizeAndroidSession() {
        FluxPlayNativeActivity a = sInstance;
        if (a == null) {
            return;
        }
        a.runOnUiThread(() -> {
            try {
                a.refreshInsetsFromDecor();
                a.writeDeviceCaps();
                if (a.surfaceSessionWanted) {
                    a.ensureVideoSurface();
                    // Never force TRANSLUCENT here — Pixel resize storm.
                    // Reattach only when Surface is dead (ready path is no-op in runnable).
                    if (a.surfaceNeedsHardReattach()) {
                        a.scheduleSurfaceReattach(0);
                    }
                }
                Log.i(TAG, "stabilizeAndroidSession surface=" + a.surfaceReady
                        + " gen=" + a.surfaceGen + " session=" + a.surfaceSessionWanted);
            } catch (Exception e) {
                Log.e(TAG, "stabilizeAndroidSession", e);
            }
        });
    }

    public static boolean isVideoSurfaceReady() {
        FluxPlayNativeActivity a = sInstance;
        return a != null && a.surfaceReady && a.videoSurface != null && a.videoSurface.isValid()
                && a.surfaceW >= 64 && a.surfaceH >= 64;
    }

    /**
     * Mark Surface size stable AFTER Rust acquired wid so chrome inset cannot relayout.
     * Do NOT call Holder.setFixedSize here — that recreates the Surface and kills wid.
     */
    public static void lockVideoSurfaceSize() {
        FluxPlayNativeActivity a = sInstance;
        if (a == null) {
            return;
        }
        a.surfaceSizeLocked = a.surfaceReady && a.surfaceW >= 64 && a.surfaceH >= 64;
        if (a.surfaceSizeLocked) {
            Log.i(TAG, "video SurfaceView size locked (layout only) "
                    + a.surfaceW + "x" + a.surfaceH);
        }
    }

    /**
     * Push video geometry for the cover layout.
     * @param bw/bh MediaCodec buffer size (decoded, pre-SAR) — fixed Surface size.
     * @param dw/dh display size (SAR/DAR-corrected) — drives the cover scale.
     * The view is sized to a display-aspect box that COVERS the screen and is
     * centered in the content frame (parent clips the overflow). The buffer is
     * scale-to-filled into the view by SurfaceFlinger, which also corrects
     * anamorphic SAR. Chrome toggles never touch this — zero relayout.
     */
    public static void setVideoBufferSize(int bw, int bh, int dw, int dh) {
        FluxPlayNativeActivity a = sInstance;
        if (a == null) {
            return;
        }
        a.runOnUiThread(() -> {
            try {
                if (bw < 16 || bh < 16 || dw < 16 || dh < 16) {
                    return;
                }
                if (a.videoBufferW == bw && a.videoBufferH == bh
                        && a.videoAspectW == dw && a.videoAspectH == dh
                        && !a.bufferTransformDirty) {
                    return;
                }
                a.videoBufferW = bw;
                a.videoBufferH = bh;
                a.videoAspectW = dw;
                a.videoAspectH = dh;
                a.bufferTransformDirty = true;
                a.applyVideoCoverTransform();
                Log.i(TAG, "video geometry buf=" + bw + "x" + bh + " disp=" + dw + "x" + dh);
            } catch (Exception e) {
                Log.e(TAG, "setVideoBufferSize", e);
            }
        });
    }

    /**
     * Cover-fit by VIEW LAYOUT (not SurfaceControl — ViewRootImpl overwrites
     * SC transactions on SurfaceView every frame):
     *   1. holder.setFixedSize(buffer) pins the Surface to the decoded size so
     *      later view relayouts never recreate the Surface (no mpv rebind storm).
     *   2. The view is laid out at display-aspect × cover scale, centered —
     *      larger than the screen, the FrameLayout clips the overflow.
     * Result: exact screen-fill with preserved ratio (Netflix-style zoom/crop).
     */
    private void applyVideoCoverTransform() {
        if (!bufferTransformDirty) {
            return;
        }
        if (videoSurfaceView == null || videoBufferW < 16 || videoBufferH < 16
                || videoAspectW < 16 || videoAspectH < 16) {
            return;
        }
        DisplayMetrics dm = getResources().getDisplayMetrics();
        float scrW = dm.widthPixels;
        float scrH = dm.heightPixels;
        float cover = Math.max(scrW / videoAspectW, scrH / videoAspectH);
        int dispW = Math.round(videoAspectW * cover);
        int dispH = Math.round(videoAspectH * cover);
        try {
            // Pin the Surface buffer to the decoded size (idempotent).
            videoSurfaceView.getHolder().setFixedSize(videoBufferW, videoBufferH);
            ViewGroup.LayoutParams raw = videoSurfaceView.getLayoutParams();
            if (raw instanceof FrameLayout.LayoutParams) {
                FrameLayout.LayoutParams lp = (FrameLayout.LayoutParams) raw;
                if (lp.width != dispW || lp.height != dispH
                        || lp.gravity != Gravity.CENTER) {
                    lp.width = dispW;
                    lp.height = dispH;
                    lp.gravity = Gravity.CENTER;
                    videoSurfaceView.setLayoutParams(lp);
                }
            }
            bufferTransformDirty = false;
            Log.i(TAG, "video cover layout buf=" + videoBufferW + "x" + videoBufferH
                    + " view=" + dispW + "x" + dispH + " scr=" + (int) scrW + "x" + (int) scrH);
        } catch (Exception e) {
            Log.w(TAG, "applyVideoCoverTransform", e);
        }
    }

    /** Force sensor-landscape while playing (portrait mode dropped); restore after. */
    public static void setForceLandscape(boolean force) {
        FluxPlayNativeActivity a = sInstance;
        if (a == null) {
            return;
        }
        a.runOnUiThread(() -> {
            try {
                a.setRequestedOrientation(force
                        ? ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
                        : ActivityInfo.SCREEN_ORIENTATION_FULL_SENSOR);
                Log.i(TAG, "forceLandscape=" + force);
            } catch (Exception e) {
                Log.e(TAG, "setForceLandscape", e);
            }
        });
    }

    /** Live Surface for mpv wid (GlobalRef kept on Rust side). */
    public static Surface getVideoSurface() {        FluxPlayNativeActivity a = sInstance;
        if (a == null) {
            return null;
        }
        return a.videoSurface;
    }

    public static int[] videoSurfaceSizePx() {
        FluxPlayNativeActivity a = sInstance;
        if (a == null) {
            return new int[] {0, 0};
        }
        return new int[] {a.surfaceW, a.surfaceH};
    }

    /** Match panel refresh when playing (Phase B). Prefer content Hz when provided. */
    public static void setVideoFrameRate(float fps) {
        FluxPlayNativeActivity a = sInstance;
        if (a == null || a.videoSurfaceView == null) {
            return;
        }
        final float rate = fps;
        a.runOnUiThread(() -> {
            try {
                if (Build.VERSION.SDK_INT >= 30 && a.videoSurface != null) {
                    a.videoSurface.setFrameRate(
                            rate > 1f ? rate : 60f,
                            Surface.FRAME_RATE_COMPATIBILITY_DEFAULT);
                }
                // Do NOT applyPreferredDisplayMode here — thrashing preferred modes
                // storms NativeWindowResized on Pixel and blacks Soft/SurfaceView.
            } catch (Exception e) {
                Log.w(TAG, "setVideoFrameRate", e);
            }
        });
    }

    public static void setHdrColorMode(boolean hdr) {
        setDisplayColorMode(hdr ? "hdr" : "default");
    }

    /**
     * Window color mode: default | sdr | wide | hdr.
     * HDR/HDR10+ use COLOR_MODE_HDR; wide uses COLOR_MODE_WIDE_COLOR_GAMUT (API 26+).
     */
    public static void setDisplayColorMode(String mode) {
        FluxPlayNativeActivity a = sInstance;
        if (a == null || Build.VERSION.SDK_INT < 26) {
            return;
        }
        final String m = mode == null ? "default" : mode.toLowerCase();
        a.runOnUiThread(() -> {
            try {
                int colorMode = ActivityInfo.COLOR_MODE_DEFAULT;
                if ("hdr".equals(m) || "hdr_plus".equals(m) || "hdr10".equals(m)
                        || "dv".equals(m) || "dolby".equals(m)) {
                    colorMode = ActivityInfo.COLOR_MODE_HDR;
                } else if ("wide".equals(m) || "p3".equals(m) || "gamut".equals(m)) {
                    colorMode = ActivityInfo.COLOR_MODE_WIDE_COLOR_GAMUT;
                }
                a.getWindow().setColorMode(colorMode);
                Log.i(TAG, "displayColorMode=" + m + " -> " + colorMode);
            } catch (Throwable e) {
                Log.w(TAG, "setDisplayColorMode", e);
            }
        });
    }

    /**
     * Pick display mode: nearest supported refresh ≥ content target (or peak if target≤0).
     * Avoids always locking 120/165 when content is 24/30 — saves power, less judder.
     */
    private void applyPreferredDisplayMode(float targetHz) {
        if (Build.VERSION.SDK_INT < 23) {
            return;
        }
        try {
            Display display = getWindowManager().getDefaultDisplay();
            Display.Mode[] modes = display.getSupportedModes();
            Display.Mode current = display.getMode();
            Display.Mode best = current;
            float bestScore = Float.MAX_VALUE;
            for (Display.Mode m : modes) {
                // Prefer same or higher resolution as current physical panel.
                if (m.getPhysicalWidth() < current.getPhysicalWidth()
                        || m.getPhysicalHeight() < current.getPhysicalHeight()) {
                    continue;
                }
                float hz = m.getRefreshRate();
                float score;
                if (targetHz > 1f) {
                    // Prefer hz >= target, closest; else closest below.
                    float delta = hz - targetHz;
                    score = delta >= -0.5f ? delta : 1000f - delta;
                } else {
                    // Peak Hz when no content hint.
                    score = -hz;
                }
                if (score < bestScore) {
                    bestScore = score;
                    best = m;
                }
            }
            WindowManager.LayoutParams lp = getWindow().getAttributes();
            if (lp.preferredDisplayModeId != best.getModeId()) {
                lp.preferredDisplayModeId = best.getModeId();
                getWindow().setAttributes(lp);
                Log.i(TAG, "preferredDisplayMode " + best.getPhysicalWidth() + "x"
                        + best.getPhysicalHeight() + "@" + best.getRefreshRate()
                        + " targetHz=" + targetHz);
            }
        } catch (Exception e) {
            Log.w(TAG, "applyPreferredDisplayMode", e);
        }
    }

    /** SoC / HDR / Hz / MediaCodec / surface for Rust quality matrix (Phases B–C). */
    private void writeDeviceCaps() {
        try {
            String soc = "";
            if (Build.VERSION.SDK_INT >= 31) {
                try {
                    soc = Build.SOC_MODEL != null ? Build.SOC_MODEL : "";
                } catch (Throwable ignored) {
                    soc = "";
                }
            }
            if (soc.isEmpty() && Build.HARDWARE != null) {
                soc = Build.HARDWARE;
            }
            int cores = Runtime.getRuntime().availableProcessors();
            int refreshHz = 60;
            boolean hdrCapable = false;
            List<Integer> hdrTypes = new ArrayList<>();
            try {
                Display display = getWindowManager().getDefaultDisplay();
                refreshHz = Math.round(display.getRefreshRate());
                if (Build.VERSION.SDK_INT >= 24) {
                    Display.HdrCapabilities caps = display.getHdrCapabilities();
                    if (caps != null) {
                        int[] types = caps.getSupportedHdrTypes();
                        if (types != null) {
                            for (int t : types) {
                                hdrTypes.add(t);
                            }
                            hdrCapable = types.length > 0;
                        }
                    }
                }
                if (Build.VERSION.SDK_INT >= 23) {
                    Display.Mode mode = display.getMode();
                    if (mode != null) {
                        refreshHz = Math.max(refreshHz, Math.round(mode.getRefreshRate()));
                    }
                    for (Display.Mode m : display.getSupportedModes()) {
                        refreshHz = Math.max(refreshHz, Math.round(m.getRefreshRate()));
                    }
                }
            } catch (Exception e) {
                Log.w(TAG, "display probe", e);
            }
            boolean mc4k = false;
            boolean mcHdr = false;
            boolean mcVideo = false;
            int mcMaxW = 0;
            int mcMaxH = 0;
            List<Float> refreshModes = new ArrayList<>();
            try {
                Display display = getWindowManager().getDefaultDisplay();
                if (Build.VERSION.SDK_INT >= 23) {
                    for (Display.Mode m : display.getSupportedModes()) {
                        float hz = m.getRefreshRate();
                        boolean dup = false;
                        for (Float existing : refreshModes) {
                            if (Math.abs(existing - hz) < 0.5f) {
                                dup = true;
                                break;
                            }
                        }
                        if (!dup) {
                            refreshModes.add(hz);
                        }
                    }
                }
            } catch (Exception ignored) {
            }
            try {
                MediaCodecList list = new MediaCodecList(MediaCodecList.REGULAR_CODECS);
                for (MediaCodecInfo info : list.getCodecInfos()) {
                    if (info.isEncoder()) {
                        continue;
                    }
                    for (String type : info.getSupportedTypes()) {
                        if (!type.startsWith("video/")) {
                            continue;
                        }
                        mcVideo = true;
                        MediaCodecInfo.CodecCapabilities caps = info.getCapabilitiesForType(type);
                        MediaCodecInfo.VideoCapabilities vc = caps.getVideoCapabilities();
                        if (vc != null) {
                            try {
                                if (vc.isSizeSupported(3840, 2160)) {
                                    mc4k = true;
                                }
                            } catch (Throwable ignored) {
                            }
                            try {
                                int uw = vc.getSupportedWidths().getUpper();
                                int uh = vc.getSupportedHeights().getUpper();
                                if (uw > mcMaxW) {
                                    mcMaxW = uw;
                                }
                                if (uh > mcMaxH) {
                                    mcMaxH = uh;
                                }
                                if (uw >= 3840 && uh >= 2160) {
                                    mc4k = true;
                                }
                            } catch (Throwable ignored) {
                            }
                        }
                        for (MediaCodecInfo.CodecProfileLevel pl : caps.profileLevels) {
                            int p = pl.profile;
                            if (p == 2 || p == 4 || p == 4096 || p == 8192 || p == 16384) {
                                if ("video/hevc".equals(type)
                                        || "video/x-vnd.on2.vp9".equals(type)
                                        || "video/av01".equals(type)) {
                                    mcHdr = true;
                                }
                            }
                        }
                    }
                }
            } catch (Exception e) {
                Log.w(TAG, "mediacodec probe", e);
            }
            StringBuilder modesArr = new StringBuilder("[");
            for (int i = 0; i < refreshModes.size(); i++) {
                if (i > 0) {
                    modesArr.append(',');
                }
                modesArr.append(refreshModes.get(i));
            }
            modesArr.append(']');
            StringBuilder hdrArr = new StringBuilder("[");
            for (int i = 0; i < hdrTypes.size(); i++) {
                if (i > 0) {
                    hdrArr.append(',');
                }
                hdrArr.append(hdrTypes.get(i));
            }
            hdrArr.append(']');
            StringBuilder hdrLabels = new StringBuilder("[");
            for (int i = 0; i < hdrTypes.size(); i++) {
                if (i > 0) {
                    hdrLabels.append(',');
                }
                hdrLabels.append('"').append(esc(hdrTypeLabel(hdrTypes.get(i)))).append('"');
            }
            hdrLabels.append(']');
            boolean panelOled = guessOledPanel();
            String json = "{"
                    + "\"soc\":\"" + esc(soc) + "\","
                    + "\"board\":\"" + esc(Build.BOARD) + "\","
                    + "\"hardware\":\"" + esc(Build.HARDWARE) + "\","
                    + "\"manufacturer\":\"" + esc(Build.MANUFACTURER) + "\","
                    + "\"model\":\"" + esc(Build.MODEL) + "\","
                    + "\"gl_renderer\":\"\","
                    + "\"cores\":" + cores + ","
                    + "\"sdk_int\":" + Build.VERSION.SDK_INT + ","
                    + "\"refresh_hz\":" + refreshHz + ","
                    + "\"refresh_modes\":" + modesArr + ","
                    + "\"hdr_types\":" + hdrArr + ","
                    + "\"hdr_labels\":" + hdrLabels + ","
                    + "\"hdr_capable\":" + (hdrCapable ? "true" : "false") + ","
                    + "\"mediacodec_4k\":" + (mc4k ? "true" : "false") + ","
                    + "\"mediacodec_hdr\":" + (mcHdr ? "true" : "false") + ","
                    + "\"mediacodec_video\":" + (mcVideo ? "true" : "false") + ","
                    + "\"mediacodec_max_w\":" + mcMaxW + ","
                    + "\"mediacodec_max_h\":" + mcMaxH + ","
                    + "\"panel_oled\":" + (panelOled ? "true" : "false") + ","
                    + "\"surface_ready\":" + (surfaceReady ? "true" : "false") + ","
                    + "\"surface_w\":" + surfaceW + ","
                    + "\"surface_h\":" + surfaceH
                    + "}";
            File meta = new File(safInboxDir(), "device_caps.json");
            try (FileOutputStream fos = new FileOutputStream(meta)) {
                fos.write(json.getBytes("UTF-8"));
            }
            Log.i(TAG, "device_caps: " + Build.MANUFACTURER + " " + Build.MODEL
                    + " soc=" + soc + " hz=" + refreshHz + " hdr=" + hdrCapable
                    + " labels=" + hdrLabels
                    + " mc4k=" + mc4k + " mcHdr=" + mcHdr + " mcVideo=" + mcVideo
                    + " max=" + mcMaxW + "x" + mcMaxH
                    + " oled=" + panelOled
                    + " surface=" + surfaceReady + " cores=" + cores
                    + " sdk=" + Build.VERSION.SDK_INT);
        } catch (Exception e) {
            Log.e(TAG, "writeDeviceCaps", e);
        }
    }

    private static String hdrTypeLabel(int type) {
        // Display.HdrCapabilities constants
        switch (type) {
            case 1:
                return "Dolby Vision";
            case 2:
                return "HDR10";
            case 3:
                return "HLG";
            case 4:
                return "HDR10+";
            default:
                return "HDR(" + type + ")";
        }
    }

    /** Best-effort OLED/AMOLED detection — Android has no official panel API. */
    private boolean guessOledPanel() {
        String blob = ((Build.MANUFACTURER == null ? "" : Build.MANUFACTURER) + " "
                + (Build.MODEL == null ? "" : Build.MODEL) + " "
                + (Build.DEVICE == null ? "" : Build.DEVICE) + " "
                + (Build.HARDWARE == null ? "" : Build.HARDWARE)).toLowerCase();
        if (blob.contains("pixel")
                || blob.contains("galaxy")
                || blob.contains("sm-s")
                || blob.contains("sm-g")
                || blob.contains("sm-n")
                || blob.contains("oneplus")
                || blob.contains("oppo")
                || blob.contains("vivo")
                || blob.contains("xiaomi")
                || blob.contains("mi ")
                || blob.contains("redmi")
                || blob.contains("poco")
                || blob.contains("nothing")
                || blob.contains("amoled")
                || blob.contains("oled")) {
            return true;
        }
        // HDR-capable flagships without LCD branding are usually OLED.
        try {
            Display display = getWindowManager().getDefaultDisplay();
            if (Build.VERSION.SDK_INT >= 24) {
                Display.HdrCapabilities caps = display.getHdrCapabilities();
                if (caps != null && caps.getSupportedHdrTypes() != null
                        && caps.getSupportedHdrTypes().length > 0
                        && (blob.contains("google") || blob.contains("samsung"))) {
                    return true;
                }
            }
        } catch (Throwable ignored) {
        }
        return false;
    }

    private void writeInboxMeta(String status, String name, String path, String mime) {
        try {
            File meta = new File(safInboxDir(), "latest.json");
            String json = "{"
                    + "\"status\":\"" + esc(status) + "\","
                    + "\"name\":\"" + esc(name == null ? "" : name) + "\","
                    + "\"path\":\"" + esc(path == null ? "" : path) + "\","
                    + "\"mime\":\"" + esc(mime == null ? "" : mime) + "\""
                    + "}";
            try (FileOutputStream fos = new FileOutputStream(meta)) {
                fos.write(json.getBytes("UTF-8"));
            }
        } catch (Exception e) {
            Log.e(TAG, "writeInboxMeta", e);
        }
    }

    private static String esc(String s) {
        StringBuilder b = new StringBuilder(s.length() + 8);
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            switch (c) {
                case '\\': b.append("\\\\"); break;
                case '"': b.append("\\\""); break;
                case '\n': b.append("\\n"); break;
                case '\r': b.append("\\r"); break;
                case '\t': b.append("\\t"); break;
                default:
                    if (c < 0x20) {
                        b.append(String.format("\\u%04x", (int) c));
                    } else {
                        b.append(c);
                    }
            }
        }
        return b.toString();
    }

    private String queryDisplayName(Uri uri) {
        String name = "picked";
        try (Cursor c = getContentResolver().query(uri, null, null, null, null)) {
            if (c != null && c.moveToFirst()) {
                int idx = c.getColumnIndex(OpenableColumns.DISPLAY_NAME);
                if (idx >= 0) {
                    name = c.getString(idx);
                }
            }
        } catch (Exception ignored) {
        }
        return name;
    }

    private void copyUriToFile(Uri uri, File out) throws Exception {
        try (InputStream in = getContentResolver().openInputStream(uri);
             OutputStream os = new FileOutputStream(out)) {
            if (in == null) {
                throw new IllegalStateException("openInputStream null");
            }
            byte[] buf = new byte[8192];
            int n;
            while ((n = in.read(buf)) >= 0) {
                os.write(buf, 0, n);
            }
        }
    }

    private void copyFileToUri(File src, Uri uri) throws Exception {
        try (InputStream in = new java.io.FileInputStream(src);
             OutputStream os = getContentResolver().openOutputStream(uri)) {
            if (os == null) {
                throw new IllegalStateException("openOutputStream null");
            }
            byte[] buf = new byte[8192];
            int n;
            while ((n = in.read(buf)) >= 0) {
                os.write(buf, 0, n);
            }
        }
    }
}
