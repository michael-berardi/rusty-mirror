//! WKWebView snapshots of a window that is never on screen.
//!
//! `takeSnapshotWithConfiguration:` renders the web view itself, so it needs no
//! Screen Recording permission and never shows, raises or focuses anything.
//! The completion handler is an Objective-C block with explicit Arc ownership:
//! a timed-out receiver cannot leave WebKit holding a dangling pointer.
use std::{
    ffi::{c_char, c_void, CStr},
    mem,
    sync::{mpsc, Arc},
};

type Id = *mut c_void;
pub(crate) type Sender = mpsc::SyncSender<Result<Vec<u8>, String>>;

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Id;
    fn objc_msgSend();
}
#[link(name = "AppKit", kind = "framework")]
extern "C" {}
#[link(name = "WebKit", kind = "framework")]
extern "C" {}
extern "C" {
    static _NSConcreteStackBlock: c_void;
    fn _Block_copy(block: *const c_void) -> *mut c_void;
    fn _Block_release(block: *const c_void);
}

#[repr(C)]
struct Descriptor {
    reserved: usize,
    size: usize,
    copy: unsafe extern "C" fn(*mut Block, *const Block),
    dispose: unsafe extern "C" fn(*mut Block),
}

#[repr(C)]
struct Block {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: unsafe extern "C" fn(*mut Block, Id, Id),
    descriptor: *const Descriptor,
    sender: *const Sender,
}

unsafe extern "C" fn copy(dst: *mut Block, src: *const Block) {
    Arc::increment_strong_count((*src).sender);
    (*dst).sender = (*src).sender;
}

unsafe extern "C" fn dispose(block: *mut Block) {
    Arc::decrement_strong_count((*block).sender);
}

static DESCRIPTOR: Descriptor = Descriptor {
    reserved: 0,
    size: mem::size_of::<Block>(),
    copy,
    dispose,
};

unsafe fn sel(s: &'static [u8]) -> Id {
    sel_registerName(s.as_ptr().cast())
}

unsafe fn class(s: &'static [u8]) -> Id {
    objc_getClass(s.as_ptr().cast())
}

unsafe fn get(obj: Id, s: &'static [u8]) -> Id {
    let f: unsafe extern "C" fn(Id, Id) -> Id = mem::transmute(objc_msgSend as *const ());
    f(obj, sel(s))
}

unsafe fn responds(obj: Id, selector: Id) -> bool {
    let f: unsafe extern "C" fn(Id, Id, Id) -> bool = mem::transmute(objc_msgSend as *const ());
    !obj.is_null() && f(obj, sel(b"respondsToSelector:\0"), selector)
}

/// Keep timers and rendering running while the mirror is hidden (macOS 14+).
/// Only the mirror's own WKPreferences change; window visibility never does.
pub(crate) unsafe fn keep_scheduling(webview: Id) {
    let preferences = get(get(webview, b"configuration\0"), b"preferences\0");
    let setter = sel(b"setInactiveSchedulingPolicy:\0");
    if responds(preferences, setter) {
        let set: unsafe extern "C" fn(Id, Id, isize) = mem::transmute(objc_msgSend as *const ());
        set(preferences, setter, 2); // WKInactiveSchedulingPolicyNone
    }
}

unsafe extern "C" fn complete(block: *mut Block, image: Id, error: Id) {
    let result = (|| -> Result<Vec<u8>, String> {
        if !error.is_null() || image.is_null() {
            let detail = if error.is_null() {
                "no image".to_string()
            } else {
                let text = get(get(error, b"localizedDescription\0"), b"UTF8String\0");
                if text.is_null() {
                    "WebKit error".into()
                } else {
                    CStr::from_ptr(text.cast()).to_string_lossy().into_owned()
                }
            };
            return Err(format!("hidden snapshot unavailable: {detail}. The window was not shown; use eval instead."));
        }
        let tiff = get(image, b"TIFFRepresentation\0");
        if tiff.is_null() {
            return Err("snapshot has no pixels".into());
        }
        let one: unsafe extern "C" fn(Id, Id, Id) -> Id = mem::transmute(objc_msgSend as *const ());
        let rep = one(
            class(b"NSBitmapImageRep\0"),
            sel(b"imageRepWithData:\0"),
            tiff,
        );
        let png: unsafe extern "C" fn(Id, Id, usize, Id) -> Id =
            mem::transmute(objc_msgSend as *const ());
        let data = png(
            rep,
            sel(b"representationUsingType:properties:\0"),
            4,
            get(class(b"NSDictionary\0"), b"dictionary\0"),
        );
        if data.is_null() {
            return Err("PNG conversion failed".into());
        }
        let length: unsafe extern "C" fn(Id, Id) -> usize =
            mem::transmute(objc_msgSend as *const ());
        let len = length(data, sel(b"length\0"));
        if len == 0 || len > 32 * 1024 * 1024 {
            return Err("snapshot PNG is empty or over 32 MiB".into());
        }
        let bytes = get(data, b"bytes\0");
        if bytes.is_null() {
            return Err("snapshot PNG has no data".into());
        }
        let bytes = std::slice::from_raw_parts(bytes.cast::<u8>(), len).to_vec();
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err("snapshot is not a PNG".into());
        }
        Ok(bytes)
    })();
    let _ = (*(*block).sender).try_send(result);
}

pub(crate) unsafe fn take_snapshot(webview: Id, sender: Sender) {
    let method = sel(b"takeSnapshotWithConfiguration:completionHandler:\0");
    if !responds(webview, method) {
        let _ = sender.try_send(Err(
            "WKWebView snapshots are unsupported here; use eval".into()
        ));
        return;
    }
    let sender = Arc::into_raw(Arc::new(sender));
    let block = Block {
        isa: &_NSConcreteStackBlock,
        flags: 1 << 25, // BLOCK_HAS_COPY_DISPOSE
        reserved: 0,
        invoke: complete,
        descriptor: &DESCRIPTOR,
        sender,
    };
    let copied = _Block_copy(&block as *const _ as *const c_void);
    Arc::decrement_strong_count(sender);
    let config = get(class(b"WKSnapshotConfiguration\0"), b"new\0");
    // The view is never on screen, so there is no screen update to wait for.
    let set: unsafe extern "C" fn(Id, Id, bool) = mem::transmute(objc_msgSend as *const ());
    set(config, sel(b"setAfterScreenUpdates:\0"), false);
    let call: unsafe extern "C" fn(Id, Id, Id, *mut c_void) =
        mem::transmute(objc_msgSend as *const ());
    call(webview, method, config, copied);
    get(config, b"release\0");
    _Block_release(copied);
}
