//! macOS 环形加载动画：borderless floating NSWindow + CALayer 旋转动画。
//!
//! 内容只有一张静态帧（环是圆对称图形，旋转它 ≡ 逐帧重绘），旋转由
//! Core Animation 渲染服务器驱动（`transform.rotation` 无限循环）——
//! 主线程被 Tauri build()/setup 阻塞期间照常旋转。不采用 CADisplayLink/
//! NSTimer：它们要等主 run loop 转起来才回调，恰好在最需要的阶段失效。

use std::cell::Cell;

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSBackingStoreType, NSFloatingWindowLevel, NSWindow, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::CATransaction;

/// 64 位平台 `CGFloat == f64`（objc2-foundation 0.3 未导出该 CoreFoundation
/// 别名，本地等价定义；Tauri 不支持 32 位 macOS）。
type CGFloat = f64;

// CoreGraphics C 接口（零依赖直连，避免仅为 CGImage 拉入 objc2-core-graphics）
extern "C" {
    fn CGColorSpaceCreateDeviceRGB() -> *mut std::ffi::c_void;
    fn CGColorSpaceRelease(space: *mut std::ffi::c_void);
    fn CGDataProviderCreateWithData(
        info: *mut std::ffi::c_void,
        data: *const std::ffi::c_void,
        size: usize,
        release: Option<ProviderReleaseCallback>,
    ) -> *mut std::ffi::c_void;
    fn CGDataProviderRelease(provider: *mut std::ffi::c_void);
    fn CGImageCreate(w: usize, h: usize, bpc: usize, bpp: usize, bpr: usize, space: *mut std::ffi::c_void, info: u32, provider: *mut std::ffi::c_void, decode: *const std::ffi::c_void, interp: bool, intent: u32) -> *mut std::ffi::c_void;
    fn CGImageRelease(img: *mut std::ffi::c_void);
}

/// `CGDataProviderReleaseDataCallback`：provider 销毁时释放移交的 `Box<[u8]>`。
type ProviderReleaseCallback = unsafe extern "C" fn(
    info: *mut std::ffi::c_void,
    data: *const std::ffi::c_void,
    size: usize,
);

unsafe extern "C" fn provider_release(info: *mut std::ffi::c_void, _data: *const std::ffi::c_void, _size: usize) {
    if !info.is_null() {
        // info 是 Box<Box<[u8]>> 的外层瘦指针（切片指针是胖指针，塞不进单个字）
        drop(Box::from_raw(info as *mut Box<[u8]>));
    }
}

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

// SAFETY：裸指针仅作为句柄跨线程携带（listen 回调 → run_on_main_thread），
// 从不在非主线程解引用——AppKit 的所有触碰都在主线程：创建于 Builder 之前、
// recenter 于 setup（主线程）、close 于 run_on_main_thread 回调或主线程 Drop。
// 移动裸指针本身不构成数据竞争；共享 &Self 的并发读也只发生在主线程，
// 跨线程侧仅 clone Arc、从不触碰内部字段。
unsafe impl Send for MacSplash {}
unsafe impl Sync for MacSplash {}

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
        let (screen_frame, scale) = {
            match objc2_app_kit::NSScreen::mainScreen(mtm) {
                Some(s) => {
                    let scale: CGFloat = unsafe { msg_send![&*s, backingScaleFactor] };
                    (s.frame(), scale as f64)
                }
                None => (NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1920.0, 1080.0)), 2.0),
            }
        };
        let cx = screen_frame.origin.x + (screen_frame.size.width - SPLASH_SIZE) / 2.0;
        let cy = screen_frame.origin.y + (screen_frame.size.height - SPLASH_SIZE) / 2.0;
        let content_rect = NSRect::new(NSPoint::new(cx, cy), NSSize::new(SPLASH_SIZE, SPLASH_SIZE));

        unsafe {
            // 强类型 alloc+init（objc2 0.6 所有权模型：Allocated → Retained，
            // +1 引用归调用方，close 时经 from_raw 释放）；mtm.alloc::<T>()
            // 是 MainThreadMarker 的固有方法（ClassType::alloc 的便捷形态）
            let w = NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc(),
                content_rect,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            );
            w.setLevel(NSFloatingWindowLevel);
            w.setCollectionBehavior(NSWindowCollectionBehavior::CanJoinAllSpaces);
            w.setOpaque(false);
            // NSColor feature 未启用，背景色走 msg_send 传 nil（borderless 窗口即透明）
            let _: () = msg_send![&*w, setBackgroundColor: std::ptr::null::<AnyObject>()];
            // 无阴影：borderless 层窗口的系统阴影会勾出方形轮廓；Windows 侧同样无
            w.setHasShadow(false);
            // 49pt 小窗不拦截鼠标（不挡用户点击）
            let _: () = msg_send![&*w, setIgnoresMouseEvents: true];
            // alloc/init 创建的窗口 releasedWhenClosed 默认为 YES：close 会额外
            // release 一次，与 close_impl 的 from_raw 释放叠加成双重释放——关掉
            w.setReleasedWhenClosed(false);

            // NSView feature 未启用，contentView/wantsLayer 走 msg_send
            let cv: *mut AnyObject = msg_send![&*w, contentView];
            let _: () = msg_send![cv, setWantsLayer: true];

            let w: *mut AnyObject = Retained::into_raw(w).cast();
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
        // tiny-skia 输出直通 RGBA；CGImageCreate 声明 kCGImageAlphaPremultipliedLast，
        // 须预乘（否则半透明 muted 轨道按预乘解释会过亮）——与 Windows 侧一致
        super::render::premultiply_alpha(&mut buf);

        // AppKit 托管的 backing layer 几何由 AppKit 接管：非 flipped NSView
        // 的锚点被设在左下角，transform 动画会绕它旋转（= 绕窗口左下角甩）。
        // 自建 CALayer 子层完全自管几何：默认 anchorPoint (0.5, 0.5)，
        // 旋转即绕环心；backing layer 仅作容器。
        let backing: *mut AnyObject = msg_send![cv, layer];
        let layer_cls = AnyClass::get(c"CALayer").expect("CALayer class");
        let layer: *mut AnyObject = msg_send![layer_cls, layer];
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(SPLASH_SIZE, SPLASH_SIZE));
        let _: () = msg_send![layer, setFrame: frame];
        let _: () = msg_send![backing, addSublayer: layer];
        // buf 的所有权移交给 CGDataProvider（release 回调释放 Box）：
        // CG 渲染服务器按需异步读取像素，函数作用域内的 Vec 在返回后
        // 即被释放——悬空指针会渲染出随机垃圾（方块/花屏）。
        let cg = create_cgimage(buf.into_boxed_slice(), super::render::SIZE);
        // CGImageRef 以 id 形式喂给 contents（经典用法）：传指针本体，
        // CALayer 的 retain 语义对 CF 类型按 CFRetain 处理
        let _: () = msg_send![layer, setContents: cg as *mut AnyObject];
        // CGImageCreate 的 +1 归调用方；layer 已 CFRetain——释放自己的份额
        // （官方模式：contents = (__bridge id)image; CGImageRelease(image);）
        CGImageRelease(cg);

        // 旋转动画：transform.rotation 正角在层坐标系（y 向上）里为逆时针，
        // 取负方向 = 屏幕顺时针，与 Windows 侧（y 向下）转向一致
        let anim_cls = AnyClass::get(c"CABasicAnimation").expect("CABasicAnimation class");
        let key = NSString::from_str("transform.rotation.z");
        let anim: *mut AnyObject = msg_send![anim_cls, animationWithKeyPath: &*key];
        let _: () = msg_send![anim, setFromValue: ns_number(0.0)];
        let _: () = msg_send![anim, setToValue: ns_number(-2.0 * std::f64::consts::PI)];
        let _: () = msg_send![anim, setDuration: REV_SECS];
        // repeatCount 是 CGFloat(f64)：f32 会走 S 寄存器、按 D 寄存器读取得到垃圾值
        let _: () = msg_send![anim, setRepeatCount: f32::MAX as f64];
        let anim_key = NSString::from_str("spin");
        let _: () = msg_send![layer, addAnimation: anim, forKey: &*anim_key];
    }

    unsafe fn close_impl(&self) {
        let w = self.window.get();
        if !w.is_null() {
            let _: () = msg_send![w, close];
            // 恢复 alloc+init 的 +1 所有权，drop 即 release
            // （releasedWhenClosed 已置 false，close 不会抢先释放）
            drop(Retained::from_raw(w.cast::<NSWindow>()));
            self.window.set(std::ptr::null_mut());
        }
    }
}

/// `NSNumber numberWithDouble:`——autoreleased 对象，仅在当前 autorelease
/// pool 生命周期内立即消费（后续 setter 各自 retain）。
unsafe fn ns_number(v: f64) -> *mut AnyObject {
    let cls = AnyClass::get(c"NSNumber").expect("NSNumber class");
    msg_send![cls, numberWithDouble: v]
}

/// `data` 的所有权移交给 CGDataProvider（release 回调释放 Box）——渲染
/// 服务器在任意时刻按需读取像素，生命周期须与 CGImage 完全一致。
unsafe fn create_cgimage(data: Box<[u8]>, size: u32) -> *mut std::ffi::c_void {
    let len = data.len();
    // 双层 Box：外层是单字瘦指针可塞进 info；内层胖指针随堆数据走
    let boxed: Box<Box<[u8]>> = Box::new(data);
    let pixels: *const std::ffi::c_void = (**boxed).as_ptr() as _;
    let owner = Box::into_raw(boxed) as *mut std::ffi::c_void;
    let cs = CGColorSpaceCreateDeviceRGB();
    let prov = CGDataProviderCreateWithData(owner, pixels, len, Some(provider_release));
    // kCGImageAlphaPremultipliedLast = 1：RGBA 字节序 + 预乘（与 render 输出一致）
    let img = CGImageCreate(size as _, size as _, 8, 32, (size * 4) as _, cs, 1, prov, std::ptr::null(), false, 0);
    // Create 规则：CGImageCreate 对 space/provider 各自 CFRetain，
    // 自己的 +1 就地释放（img 释放时连带 provider → 回调释放像素）
    CGColorSpaceRelease(cs);
    CGDataProviderRelease(prov);
    img
}
