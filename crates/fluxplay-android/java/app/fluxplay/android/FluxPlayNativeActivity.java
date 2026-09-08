package app.fluxplay.android;

import android.app.Activity;
import android.app.NativeActivity;
import android.app.PictureInPictureParams;
import android.content.Intent;
import android.content.res.Configuration;
import android.database.Cursor;
import android.media.AudioAttributes;
import android.media.AudioFocusRequest;
import android.media.AudioManager;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.provider.OpenableColumns;
import android.util.DisplayMetrics;
import android.util.Log;
import android.util.Rational;
import android.view.View;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.view.WindowManager;

import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;

/**
 * NativeActivity subclass: SAF document pick/create, system insets, OS PiP,
 * audio focus, immersive mode. Results under filesDir for the Rust side to poll.
 */
public class FluxPlayNativeActivity extends NativeActivity {
    private static final String TAG = "FluxPlay";
    private static final int REQ_OPEN = 4101;
    private static final int REQ_CREATE = 4102;

    private static FluxPlayNativeActivity sInstance;
    private static volatile boolean sInPip = false;

    private String pendingCreateSource;
    private String pendingMime = "*/*";
    private AudioFocusRequest audioFocusRequest;
    private AudioManager.OnAudioFocusChangeListener audioFocusListener;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        sInstance = this;
        super.onCreate(savedInstanceState);
    }

    @Override
    protected void onDestroy() {
        if (sInstance == this) {
            sInstance = null;
        }
        super.onDestroy();
    }

    @Override
    public void onPictureInPictureModeChanged(boolean isInPictureInPictureMode) {
        super.onPictureInPictureModeChanged(isInPictureInPictureMode);
        sInPip = isInPictureInPictureMode;
        writePipFlag(isInPictureInPictureMode);
    }

    @Override
    public void onPictureInPictureModeChanged(boolean isInPictureInPictureMode,
                                              Configuration newConfig) {
        if (Build.VERSION.SDK_INT >= 26) {
            super.onPictureInPictureModeChanged(isInPictureInPictureMode, newConfig);
        }
        sInPip = isInPictureInPictureMode;
        writePipFlag(isInPictureInPictureMode);
    }

    /** Called from Rust via JNI. mode open|create; mime e.g. text/plain or star/star. */
    public static void startSaf(String mode, String mime, String createSourcePath) {
        FluxPlayNativeActivity a = sInstance;
        if (a == null) {
            Log.w(TAG, "startSaf: no activity");
            return;
        }
        a.runOnUiThread(() -> a.launchSaf(mode, mime, createSourcePath));
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

    /**
     * Returns insets in px: [left, top, right, bottom, densityDpi].
     * API 30+: systemBars | displayCutout | ime (ime folded into bottom).
     * No fake 56dp fallback — bottom is 0 when the system reports none.
     */
    public static int[] systemInsetsPx() {
        FluxPlayNativeActivity a = sInstance;
        if (a == null) {
            return new int[] {0, 0, 0, 0, 160};
        }
        View decor = a.getWindow().getDecorView();
        int left = 0, top = 0, right = 0, bottom = 0;
        WindowInsets insets = null;
        if (Build.VERSION.SDK_INT >= 23) {
            insets = decor.getRootWindowInsets();
        }
        if (insets != null) {
            if (Build.VERSION.SDK_INT >= 30) {
                int types = WindowInsets.Type.systemBars()
                        | WindowInsets.Type.displayCutout()
                        | WindowInsets.Type.ime();
                android.graphics.Insets bars = insets.getInsets(types);
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
        }
        DisplayMetrics dm = a.getResources().getDisplayMetrics();
        int dpi = dm.densityDpi;
        return new int[] {left, top, right, bottom, dpi};
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

    public static void requestAudioFocus() {
        FluxPlayNativeActivity a = sInstance;
        if (a == null) {
            return;
        }
        a.runOnUiThread(a::requestAudioFocusInner);
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
                audioFocusListener = focusChange -> { /* Rust polls playback state */ };
            }
            if (Build.VERSION.SDK_INT >= 26) {
                if (audioFocusRequest == null) {
                    AudioAttributes attrs = new AudioAttributes.Builder()
                            .setUsage(AudioAttributes.USAGE_MEDIA)
                            .setContentType(AudioAttributes.CONTENT_TYPE_MOVIE)
                            .build();
                    audioFocusRequest = new AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
                            .setAudioAttributes(attrs)
                            .setOnAudioFocusChangeListener(audioFocusListener)
                            .build();
                }
                am.requestAudioFocus(audioFocusRequest);
            } else {
                am.requestAudioFocus(
                        audioFocusListener,
                        AudioManager.STREAM_MUSIC,
                        AudioManager.AUDIOFOCUS_GAIN);
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
        return s.replace("\\", "\\\\").replace("\"", "\\\"");
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
