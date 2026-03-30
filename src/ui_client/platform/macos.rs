use gpui::{Window, WindowBackgroundAppearance};
use objc2::{MainThreadMarker, MainThreadOnly, runtime::AnyClass};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSAutoresizingMaskOptions, NSColor,
    NSGlassEffectView, NSGlassEffectViewStyle, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindowOrderingMode,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

pub fn set_activation_policy_accessory() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };

    let app = NSApplication::sharedApplication(mtm);
    let _ = app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
}

pub fn install_backdrop(window: &mut Window) {
    if unsafe { install_backdrop_impl(window) }.is_err() {
        window.set_background_appearance(WindowBackgroundAppearance::Blurred);
    }
}

unsafe fn install_backdrop_impl(window: &Window) -> Result<(), ()> {
    let handle = HasWindowHandle::window_handle(window).map_err(|_| ())?;
    let RawWindowHandle::AppKit(raw) = handle.as_raw() else {
        return Err(());
    };

    let ns_view = unsafe { (raw.ns_view.as_ptr() as *const NSView).as_ref() }.ok_or(())?;
    let ns_window = ns_view.window().ok_or(())?;
    let host_view = unsafe { ns_view.superview() }
        .or_else(|| ns_window.contentView())
        .ok_or(())?;

    let frame = host_view.bounds();
    let autoresizing =
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable;
    let mtm = MainThreadMarker::new().ok_or(())?;

    if AnyClass::get(c"NSGlassEffectView").is_some() {
        let glass = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), frame);
        glass.setStyle(NSGlassEffectViewStyle::Regular);
        glass.setAutoresizingMask(autoresizing);

        let filler = NSView::initWithFrame(NSView::alloc(mtm), frame);
        filler.setAutoresizingMask(autoresizing);
        glass.setContentView(Some(&filler));

        host_view.addSubview_positioned_relativeTo(&glass, NSWindowOrderingMode::Below, None);
    } else {
        let visual = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
        visual.setMaterial(NSVisualEffectMaterial::ContentBackground);
        visual.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        visual.setState(NSVisualEffectState::Active);
        visual.setAutoresizingMask(autoresizing);

        host_view.addSubview_positioned_relativeTo(&visual, NSWindowOrderingMode::Below, None);
    }

    ns_window.setOpaque(false);
    ns_window.setMovableByWindowBackground(true);
    ns_window.setHasShadow(true);
    let clear = NSColor::clearColor();
    ns_window.setBackgroundColor(Some(&clear));

    Ok(())
}
