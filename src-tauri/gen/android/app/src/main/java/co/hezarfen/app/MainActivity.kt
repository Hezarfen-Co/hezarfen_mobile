package co.hezarfen.app

import android.os.Bundle
import android.webkit.JavascriptInterface
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge
import androidx.core.splashscreen.SplashScreen.Companion.installSplashScreen
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
  private var webView: WebView? = null

  /**
   * What the page can ask the shell about the device it is running on. Bound to
   * the webview, so it survives navigations and reloads — unlike a value pushed
   * into a document, which dies with that document.
   */
  inner class ShellBridge {
    /** Height of the gesture bar in CSS pixels; 0 when there is none. */
    @JavascriptInterface
    fun bottomInsetPx(): Float {
      val view = webView ?: return 0f
      val insets = ViewCompat.getRootWindowInsets(view) ?: return 0f
      return insets.getInsets(WindowInsetsCompat.Type.systemBars()).bottom /
        view.resources.displayMetrics.density
    }
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    // Must run before super.onCreate so the system keeps the launch screen up
    // instead of flashing the plain window background.
    installSplashScreen()
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
  }

  // On Android, CSS env(safe-area-inset-*) only reports the display cutout, so
  // the page can see the camera notch but not the gesture bar — the bottom tab
  // bar would render underneath it. Only the system knows that height, so read
  // it here and hand it to the page as a custom property its layout can pad by.
  // Keeping the padding inside the page (rather than insetting the webview)
  // means the tab bar's own background covers the strip, so it stays the colour
  // of whichever theme the user picked in the app.
  //
  // The insets are read from the view rather than through an
  // OnApplyWindowInsetsListener on purpose: installing that listener takes over
  // the webview's inset handling, and the page then loses
  // env(safe-area-inset-top) — the cutout is only reported to CSS while the
  // webview applies the insets itself.
  override fun onWebViewCreate(webView: WebView) {
    this.webView = webView
    webView.addJavascriptInterface(ShellBridge(), "HezarfenShell")

    // The page hides scrollbars in CSS, but those rules cannot reach the ones
    // Android draws over the webview itself, which is what still flashes down
    // the right edge while scrolling.
    webView.isVerticalScrollBarEnabled = false
    webView.isHorizontalScrollBarEnabled = false
    webView.addOnLayoutChangeListener { _, _, _, _, _, _, _, _, _ -> publishBottomInset() }
    // The first layout can land before the page has a document to set the
    // property on; this catches that case.
    webView.postDelayed({ publishBottomInset() }, 500)
  }

  override fun onResume() {
    super.onResume()
    // Re-applied after a reload or a return from the background, when the
    // property set above no longer exists on the new document.
    publishBottomInset()
  }

  private fun publishBottomInset() {
    val view = webView ?: return
    val insets = ViewCompat.getRootWindowInsets(view) ?: return
    val bottom = insets.getInsets(WindowInsetsCompat.Type.systemBars()).bottom
    val dp = bottom / view.resources.displayMetrics.density
    view.evaluateJavascript(
      "document.documentElement.style.setProperty('--android-nav-inset', '${dp}px')",
      null,
    )
  }
}
