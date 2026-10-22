//! Small per-platform helpers.

/// Sets this process's Dock icon to the app icon (macOS).
///
/// The viewer and sign-in windows run as child processes that macOS doesn't recognise as the
/// bundled app, so each would otherwise show a generic Dock icon. Setting the icon at runtime
/// makes every scry window show the same hexagon. Must be called on the main thread.
#[cfg(target_os = "macos")]
pub fn set_dock_icon() {
    use objc2::ClassType;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::{MainThreadMarker, NSData};

    let Some(mtm) = MainThreadMarker::new() else { return };
    let data = NSData::with_bytes(&crate::icon::png_bytes());
    let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    // SAFETY: standard AppKit call on the main thread with a valid image.
    unsafe { app.setApplicationIconImage(Some(&image)) };
}

#[cfg(not(target_os = "macos"))]
pub fn set_dock_icon() {}
