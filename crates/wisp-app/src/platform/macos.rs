use std::{ffi::CStr, sync::OnceLock};

use cocoa::{
    appkit::{NSApp, NSApplication, NSApplicationActivationPolicyAccessory, NSScreen},
    base::{NO, YES, nil},
    foundation::{NSPoint, NSRect, NSString},
};
use gpui::Window;
use objc::{class, msg_send, runtime::Object, sel, sel_impl};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use wisp_config::OverlayConfig;
use wisp_protocol::RenderModel;

use crate::layout::{
    OverlayPlacement, maximum_overlay_height, overlay_top_for_placement,
    preferred_overlay_placement,
};

static STATUS_ITEM: OnceLock<usize> = OnceLock::new();

#[allow(unexpected_cfgs)]
pub(crate) fn configure_application() -> anyhow::Result<()> {
    let application = unsafe { NSApp() };
    if application == nil {
        return Err(anyhow::anyhow!("AppKit application is unavailable"));
    }
    let changed =
        unsafe { application.setActivationPolicy_(NSApplicationActivationPolicyAccessory) };
    if changed != YES {
        return Err(anyhow::anyhow!(
            "AppKit rejected accessory activation policy"
        ));
    }
    let active: objc::runtime::BOOL = unsafe { msg_send![application, isActive] };
    if active == YES {
        unsafe {
            let _: () = msg_send![application, deactivate];
        }
    }
    Ok(())
}

#[allow(unexpected_cfgs)]
pub(crate) fn install_status_item() -> anyhow::Result<()> {
    if STATUS_ITEM.get().is_some() {
        return Ok(());
    }

    unsafe {
        let application = NSApp();
        let status_bar: *mut Object = msg_send![class!(NSStatusBar), systemStatusBar];
        if status_bar.is_null() {
            return Err(anyhow::anyhow!("AppKit status bar is unavailable"));
        }
        let status_item: *mut Object = msg_send![status_bar, statusItemWithLength: -1.0_f64];
        if status_item.is_null() {
            return Err(anyhow::anyhow!("could not create AppKit status item"));
        }
        let button: *mut Object = msg_send![status_item, button];
        let title = NSString::alloc(nil).init_str("✦");
        let tooltip = NSString::alloc(nil).init_str("Wisp Autocomplete");
        let _: () = msg_send![button, setTitle: title];
        let _: () = msg_send![button, setToolTip: tooltip];

        let menu: *mut Object = msg_send![class!(NSMenu), alloc];
        let menu: *mut Object = msg_send![menu, init];
        let running_title = NSString::alloc(nil).init_str("Wisp 正在运行");
        let empty = NSString::alloc(nil).init_str("");
        let running_item: *mut Object = msg_send![class!(NSMenuItem), alloc];
        let running_item: *mut Object =
            msg_send![running_item, initWithTitle: running_title action: nil keyEquivalent: empty];
        let _: () = msg_send![running_item, setEnabled: NO];
        let _: () = msg_send![menu, addItem: running_item];
        let separator: *mut Object = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: separator];

        let quit_title = NSString::alloc(nil).init_str("退出 Wisp");
        let quit_key = NSString::alloc(nil).init_str("q");
        let quit_item: *mut Object = msg_send![class!(NSMenuItem), alloc];
        let quit_item: *mut Object = msg_send![quit_item,
            initWithTitle: quit_title
            action: sel!(terminate:)
            keyEquivalent: quit_key
        ];
        let _: () = msg_send![quit_item, setTarget: application];
        let _: () = msg_send![menu, addItem: quit_item];
        let _: () = msg_send![status_item, setMenu: menu];
        let _: *mut Object = msg_send![status_item, retain];
        STATUS_ITEM
            .set(status_item as usize)
            .map_err(|_| anyhow::anyhow!("Wisp status item was already installed"))?;
    }
    Ok(())
}

#[allow(unexpected_cfgs)]
pub(crate) fn terminal_is_frontmost(expected_application_id: Option<&str>) -> bool {
    if let Ok(value) =
        std::env::var("WISP_TERMINAL_ACTIVE").or_else(|_| std::env::var("WISP_ALACRITTY_ACTIVE"))
    {
        return matches!(value.as_str(), "1" | "true" | "yes");
    }
    let Some(expected_application_id) = expected_application_id else {
        return false;
    };
    unsafe {
        let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        if workspace.is_null() {
            return false;
        }
        let application: *mut Object = msg_send![workspace, frontmostApplication];
        if application.is_null() {
            return false;
        }
        let bundle_id: *mut Object = msg_send![application, bundleIdentifier];
        if bundle_id.is_null() {
            return false;
        }
        let bytes: *const std::os::raw::c_char = msg_send![bundle_id, UTF8String];
        !bytes.is_null()
            && CStr::from_ptr(bytes)
                .to_string_lossy()
                .eq_ignore_ascii_case(expected_application_id)
    }
}

#[allow(unexpected_cfgs)]
pub(crate) fn preferred_model_placement(
    config: &OverlayConfig,
    model: &RenderModel,
) -> OverlayPlacement {
    let Some(anchor) = model.anchor else {
        return OverlayPlacement::Below;
    };
    let screen = unsafe { NSScreen::mainScreen(nil) };
    let screen_frame: NSRect = unsafe { NSScreen::frame(screen) };
    preferred_overlay_placement(
        config,
        anchor,
        maximum_overlay_height(config, model),
        screen_frame.size.height as f32,
    )
}

#[allow(unexpected_cfgs)]
pub(crate) fn placement_height_limit(
    config: &OverlayConfig,
    model: &RenderModel,
    placement: OverlayPlacement,
) -> f32 {
    let Some(anchor) = model.anchor else {
        return f32::MAX;
    };
    let screen = unsafe { NSScreen::mainScreen(nil) };
    let screen_frame: NSRect = unsafe { NSScreen::frame(screen) };
    match placement {
        OverlayPlacement::Below => {
            (screen_frame.size.height as f32 - anchor.position.y - config.cursor_gap).max(0.0)
        }
        OverlayPlacement::Above => {
            (anchor.position.y - anchor.line_height - config.cursor_gap).max(0.0)
        }
    }
}

#[allow(unexpected_cfgs)]
pub(crate) fn reposition_overlay_window(
    window: &Window,
    config: &OverlayConfig,
    model: &RenderModel,
    height: f32,
    placement: OverlayPlacement,
) -> anyhow::Result<()> {
    let Some(anchor) = model.anchor else {
        return Ok(());
    };
    let native_window = native_window(window)?;
    let screen = unsafe { NSScreen::mainScreen(nil) };
    let screen_frame: NSRect = unsafe { NSScreen::frame(screen) };
    let top = overlay_top_for_placement(config, anchor, height, placement);
    let origin_x = f64::from(anchor.position.x);
    let origin_y =
        screen_frame.origin.y + screen_frame.size.height - f64::from(top) - f64::from(height);
    dispatch::Queue::main().exec_async(move || {
        let native_window = native_window as *mut Object;
        let origin = NSPoint::new(origin_x, origin_y);
        unsafe {
            let _: () = msg_send![native_window, setFrameOrigin: origin];
        }
    });
    Ok(())
}

#[allow(unexpected_cfgs)]
pub(crate) fn reposition_detail_window(
    window: &Window,
    config: &OverlayConfig,
    model: &RenderModel,
    height: f32,
    placement: OverlayPlacement,
) -> anyhow::Result<()> {
    let Some(anchor) = model.anchor else {
        return Ok(());
    };
    let native_window = native_window(window)?;
    let screen = unsafe { NSScreen::mainScreen(nil) };
    let screen_frame: NSRect = unsafe { NSScreen::frame(screen) };
    let right = anchor.position.x + config.width + config.detail_window_gap;
    let x = if right + config.detail_width <= screen_frame.size.width as f32 {
        right
    } else {
        (anchor.position.x - config.detail_window_gap - config.detail_width).max(0.0)
    };
    let top = overlay_top_for_placement(config, anchor, height, placement);
    let origin_y =
        screen_frame.origin.y + screen_frame.size.height - f64::from(top) - f64::from(height);
    dispatch::Queue::main().exec_async(move || {
        let native_window = native_window as *mut Object;
        let origin = NSPoint::new(f64::from(x), origin_y);
        unsafe {
            let _: () = msg_send![native_window, setFrameOrigin: origin];
        }
    });
    Ok(())
}

#[allow(unexpected_cfgs)]
pub(crate) fn set_overlay_window_visible(window: &Window, visible: bool) -> anyhow::Result<()> {
    let native_window = native_window(window)?;
    dispatch::Queue::main().exec_async(move || {
        let native_window = native_window as *mut Object;
        unsafe {
            let _: () = msg_send![native_window, setIgnoresMouseEvents: NO];
            let _: () = msg_send![native_window, setAcceptsMouseMovedEvents: YES];
            let _: () = msg_send![native_window, setBecomesKeyOnlyIfNeeded: YES];
            if visible {
                let _: () = msg_send![native_window, orderFront: nil];
            } else {
                let _: () = msg_send![native_window, orderOut: nil];
            }
        }
    });
    Ok(())
}

#[allow(unexpected_cfgs)]
pub(crate) fn set_detail_window_visible(window: &Window, visible: bool) -> anyhow::Result<()> {
    let native_window = native_window(window)?;
    dispatch::Queue::main().exec_async(move || {
        let native_window = native_window as *mut Object;
        unsafe {
            let _: () = msg_send![native_window, setIgnoresMouseEvents: YES];
            let _: () = msg_send![native_window, setBecomesKeyOnlyIfNeeded: YES];
            if visible {
                let _: () = msg_send![native_window, orderFront: nil];
            } else {
                let _: () = msg_send![native_window, orderOut: nil];
            }
        }
    });
    Ok(())
}

#[allow(unexpected_cfgs)]
fn native_window(window: &Window) -> anyhow::Result<usize> {
    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| anyhow::anyhow!("read GPUI window handle: {error:?}"))?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err(anyhow::anyhow!("GPUI window is not backed by AppKit"));
    };
    let view = handle.ns_view.as_ptr().cast::<Object>();
    let native_window: *mut Object = unsafe { msg_send![view, window] };
    if native_window.is_null() {
        return Err(anyhow::anyhow!("GPUI AppKit view has no window"));
    }
    Ok(native_window as usize)
}
