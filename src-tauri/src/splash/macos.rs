//! macOS 环形加载动画：borderless floating NSWindow + CALayer 旋转动画。
//!
//! 内容只有一张静态帧（环是圆对称图形，旋转它 ≡ 逐帧重绘），旋转由
//! Core Animation 渲染服务器驱动（`transform.rotation` 无限循环）——
//! 主线程被 Tauri build()/setup 阻塞期间照常旋转。不采用 CADisplayLink/
//! NSTimer：它们要等主 run loop 转起来才回调，恰好在最需要的阶段失效。

use std::cell::Cell;

use objc2::msg_send;
use objc2::runtime::{AnyObject, Class};
use objc2::MainThreadMarker;
use objc2_foundation::{CGFloat, NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::CATransaction;

const SPLASH_SIZE: f64 = super::render::SIZE as f64;
/// 每秒一圈（与 Windows 侧 render::SPEED_DEG_PER_SEC 一致）。
const REV_SECS: f64 = 1.0;

pub(crate) struct MacSplash {
    window: Cell<*mut AnyObject>,
    /// 创建时捕获的主屏 frame（逻辑点，AppKit 左下原点）——recenter 翻转 y 轴用。
    screen: NSRect,
    /// 主屏 backing scale（物理像素 → 逻辑点）。
    scale: f64,
}

impl MacSplash {
    pub(crate) fn new() -> Option<Self> { MacSplash::create_window() }

    pub(crate) fn close(&self) {
        unsafe { self.close_impl(); }
    }

    /// 将环形窗口移动到 (cx, cy)（物理像素，屏幕坐标左上原点）。
    ///
    /// 须在主线程调用（Tauri setup 满足）；AppKit 原点在主屏左下角，
    /// 需按捕获的主屏 frame 翻转 y 轴。
    pub(crate) fn recenter(&self, cx: i32, cy: i32) {
        let w = self.window.get();
        if w.is_null() { return; }
        let lx = cx as f64 / self.scale;
        let ly_top = cy as f64 / self.scale;
        let screen_top = self.screen.origin.y + self.screen.size.height;
        let origin = NSPoint::new(
            lx - SPLASH_SIZE / 2.0,
            screen_top - ly_top - SPLASH_SIZE / 2.0,
        );
        unsafe { let _: () = msg_send![w, setFrameOrigin: origin]; }
    }
}

impl Drop for MacSplash {
    fn drop(&mut self) {
        unsafe { self.close_impl(); }
    }
}

impl MacSplash {
    fn create_window() -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let (screen_frame, scale) = unsafe {
            match objc2_app_kit::NSScreen::mainScreen(&mtm) {
                Some(s) => {
                    let scale: CGFloat = msg_send![&*s, backingScaleFactor];
                    (s.frame(), scale as f64)
                }
                None => (NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1920.0, 1080.0)), 2.0),
            }
        };
        let cx = screen_frame.origin.x + (screen_frame.size.width - SPLASH_SIZE as CGFloat) / 2.0;
        let cy = screen_frame.origin.y + (screen_frame.size.height - SPLASH_SIZE as CGFloat) / 2.0;
        let content_rect = NSRect::new(NSPoint::new(cx, cy), NSSize::new(SPLASH_SIZE as CGFloat, SPLASH_SIZE as CGFloat));

        unsafe {
            let ns_window_class = objc2_app_kit::NSWindow::class();
            let w: *mut AnyObject = msg_send![ns_window_class, alloc];
            let w: *mut AnyObject = msg_send![w, initWithContentRect: content_rect
                                                       styleMask: 0u32 // NSBorderlessWindowMask
                                                         backing: 2u32 // NSBackingStoreBuffered
                                                           defer: false];
            let _: () = msg_send![w, setLevel: 5i32]; // NSFloatingWindowLevel
            let _: () = msg_send![w, setCollectionBehavior: 1u32]; // CanJoinAllSpaces
            let _: () = msg_send![w, setOpaque: false];
            let _: () = msg_send![w, setBackgroundColor: 0i32]; // clearColor
            let _: () = msg_send![w, setHasShadow: true];
            let cv: *mut AnyObject = msg_send![w, contentView];
            let _: () = msg_send![cv, setWantsLayer: true];

            let splash = Self { window: Cell::new(w), screen: screen_frame, scale };
            splash.mount_spinning_content(cv);
            let _: () = msg_send![w, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
            // run loop 尚未运转——flush 强制首帧 + 动画立即提交给渲染服务器
            CATransaction::flush();
            Some(splash)
        }
    }

    /// 单帧内容 + 渲染服务器驱动的无限旋转（零 CPU、与显示器同频）。
    unsafe fn mount_spinning_content(&self, cv: *mut AnyObject) {
        let buf_size = (super::render::SIZE * super::render::SIZE * 4) as usize;
        let mut buf = vec![0u8; buf_size];
        super::render::render_frame(&mut buf, 0.0);

        let layer: *mut AnyObject = msg_send![cv, layer];
        let cg = create_cgimage(&buf, super::render::SIZE);
        // CGImageRef 以 id 形式喂给 contents（经典用法）：传指针本体，
        // CALayer 的 retain 语义对 CF 类型按 CFRetain 处理
        let _: () = msg_send![layer, setContents: cg as *mut AnyObject];

        // 旋转动画：transform.rotation 正角在层坐标系（y 向上）里为逆时针，
        // 取负方向 = 屏幕顺时针，与 Windows 侧（y 向下）转向一致
        let anim_cls = Class::get("CABasicAnimation").expect("CABasicAnimation class");
        let key = NSString::from_str("transform.rotation.z");
        let anim: *mut AnyObject = msg_send![anim_cls, animationWithKeyPath: &*key];
        let _: () = msg_send![anim, setFromValue: ns_number(0.0)];
        let _: () = msg_send![anim, setToValue: ns_number(-2.0 * std::f64::consts::PI)];
        let _: () = msg_send![anim, setDuration: REV_SECS];
        let _: () = msg_send![anim, setRepeatCount: f32::MAX];
        let anim_key = NSString::from_str("spin");
        let _: () = msg_send![layer, addAnimation: anim forKey: &*anim_key];
    }

    unsafe fn close_impl(&self) {
        let w = self.window.get();
        if !w.is_null() {
            let _: () = msg_send![w, close];
            self.window.set(std::ptr::null_mut());
        }
    }
}

/// `NSNumber numberWithDouble:`——autoreleased 对象，仅在当前 autorelease
/// pool 生命周期内立即消费（后续 setter 各自 retain）。
unsafe fn ns_number(v: f64) -> *mut AnyObject {
    let cls = Class::get("NSNumber").expect("NSNumber class");
    msg_send![cls, numberWithDouble: v]
}

unsafe fn create_cgimage(buf: &[u8], size: u32) -> *mut std::ffi::c_void {
    extern "C" {
        fn CGColorSpaceCreateDeviceRGB() -> *mut std::ffi::c_void;
        fn CGColorSpaceRelease(space: *mut std::ffi::c_void);
        fn CGDataProviderCreateWithData(info: *mut std::ffi::c_void, data: *const std::ffi::c_void, size: usize, release: *const std::ffi::c_void) -> *mut std::ffi::c_void;
        fn CGImageCreate(w: usize, h: usize, bpc: usize, bpp: usize, bpr: usize, space: *mut std::ffi::c_void, info: u32, provider: *mut std::ffi::c_void, decode: *const std::ffi::c_void, interp: bool, intent: u32) -> *mut std::ffi::c_void;
        fn CGImageRelease(img: *mut std::ffi::c_void);
    }
    let cs = CGColorSpaceCreateDeviceRGB();
    let prov = CGDataProviderCreateWithData(std::ptr::null_mut(), buf.as_ptr() as _, buf.len(), std::ptr::null());
    // kCGImageAlphaPremultipliedLast = 1：RGBA 字节序 + 预乘（与 render 输出一致）
    let img = CGImageCreate(size as _, size as _, 8, 32, (size * 4) as _, cs, 1, prov, std::ptr::null(), false, 0);
    CGColorSpaceRelease(cs);
    img
}
