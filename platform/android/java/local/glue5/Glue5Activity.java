package local.glue5;

import android.content.Context;
import android.os.Build;
import android.os.Bundle;
import android.provider.Settings;
import android.system.ErrnoException;
import android.system.Os;
import android.view.WindowManager;

import org.libsdl.app.SDLActivity;
import org.libsdl.app.SDLSurface;

/** The app's activity: SDL's, with our library (libglue5.so) as the main one. */
public class Glue5Activity extends SDLActivity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        // The same switches as a desktop run, RUST_LOG and GLUE5_*: string
        // extras of the launch intent (`mise run android:run` passes them),
        // set in the environment before SDL starts the library's main.
        Bundle extras = getIntent().getExtras();
        if (extras != null) {
            for (String key : extras.keySet()) {
                String value = extras.getString(key);
                if (value != null && (key.equals("RUST_LOG") || key.startsWith("GLUE5_"))) {
                    try {
                        Os.setenv(key, value, true);
                    } catch (ErrnoException e) {
                        // Left unset.
                    }
                }
            }
        }
        // The device's name as its owner set it (Settings › About phone),
        // for the Studio's pages (device.rs), unless the intent named one.
        if (Os.getenv("GLUE5_DEVICE_NAME") == null) {
            String name = Settings.Global.getString(getContentResolver(), Settings.Global.DEVICE_NAME);
            if (name == null || name.isEmpty()) {
                name = Build.MODEL;
            }
            try {
                Os.setenv("GLUE5_DEVICE_NAME", name, true);
            } catch (ErrnoException e) {
                // The kind of device, then.
            }
        }
        super.onCreate(savedInstanceState);
        // Android 11 and later tell where the on-screen keyboard is
        // (Glue5Surface): the app's pages end above it, so the window stays
        // put. Android 10 pans the window for the text being typed.
        if (Build.VERSION.SDK_INT >= 30 /* Android 11 (R) */) {
            getWindow().setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_NOTHING);
        }
    }

    @Override
    protected SDLSurface createSDLSurface(Context context) {
        return new Glue5Surface(context);
    }

    /**
     * From keyboard.rs: 1, the window pans for the text being typed (the
     * game's text box, which the game draws where it does); 0, it stays put
     * (the app's pages end above the keyboard). Sent before the keyboard
     * shows, through the queue SDL shows it with.
     */
    private static final int COMMAND_KEYBOARD_PANS = COMMAND_USER;

    @Override
    protected boolean onUnhandledMessage(int command, Object param) {
        if (command == COMMAND_KEYBOARD_PANS) {
            if (Build.VERSION.SDK_INT >= 30 /* Android 11 (R) */) {
                boolean pans = param instanceof Integer && (Integer) param != 0;
                getWindow().setSoftInputMode(pans
                        ? WindowManager.LayoutParams.SOFT_INPUT_ADJUST_PAN
                        : WindowManager.LayoutParams.SOFT_INPUT_ADJUST_NOTHING);
            }
            return true;
        }
        return super.onUnhandledMessage(command, param);
    }

    @Override
    protected String[] getLibraries() {
        return new String[] { "SDL3", "glue5" };
    }
}
