//! Windows 环形加载动画：分层窗口 + 独立后台线程消息泵。
//!
//! 动画优先走 DirectComposition（DWM 合成器驱动）：内容只有一张静态帧
//! （环是圆对称图形，旋转它 ≡ 逐帧重绘），`IDCompositionRotateTransform`
//! + 线性多项式动画（360°/s，无后续段即无限持续）让 DWM 自己转——零 CPU、
//! 与显示器同频。DComp 任一步失败则回退到 WM_TIMER + UpdateLayeredWindow
//! 的 30fps 逐帧渲染（窗口样式不同，需销毁重建）。
//!
//! FFI 层全部 Win32 调用走 `extern "system"`；D3D/DXGI/DComp 走 `windows` crate。

use std::sync::{Arc, Mutex};
use std::time::Instant;

use windows::core::Interface;
use windows::Win32::Foundation::{HMODULE, HWND as WinHWND};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, ID3D11Device,
    ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionAnimation, IDCompositionDevice, IDCompositionRotateTransform,
    IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIDevice, IDXGIFactory2, IDXGISwapChain1, DXGI_PRESENT,
    DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};

// ── Win32 FFI ──

type HWND = isize;
type HDC = isize;
type HGDIOBJ = isize;
type BOOL = i32;
type UINT = u32;
type DWORD = u32;
type LONG = i32;
type LPCWSTR = *const u16;
type LRESULT = isize;
type WPARAM = usize;
type LPARAM = isize;
type LPVOID = *mut core::ffi::c_void;
type ATOM = u16;
type COLORREF = DWORD;

#[repr(C)] struct POINT { x: LONG, y: LONG }
#[repr(C)] struct SIZE { cx: LONG, cy: LONG }
#[repr(C)] struct MSG { hwnd: HWND, message: UINT, w_param: WPARAM, l_param: LPARAM, time: DWORD, pt: POINT }
#[repr(C)] struct WNDCLASSW {
    style: UINT, lpfn_wnd_proc: Option<unsafe extern "system" fn(HWND, UINT, WPARAM, LPARAM) -> LRESULT>,
    cb_cls_extra: i32, cb_wnd_extra: i32, h_instance: isize, h_icon: isize,
    h_cursor: isize, hbr_background: isize, lpsz_menu_name: LPCWSTR, lpsz_class_name: LPCWSTR,
}
#[repr(C)] struct BLENDFUNCTION { blend_op: u8, blend_flags: u8, source_constant_alpha: u8, alpha_format: u8 }
#[repr(C)] struct BITMAPINFOHEADER {
    bi_size: DWORD, bi_width: LONG, bi_height: LONG, bi_planes: u16, bi_bit_count: u16,
    bi_compression: DWORD, bi_size_image: DWORD, bi_x_pels_per_meter: LONG,
    bi_y_pels_per_meter: LONG, bi_clr_used: DWORD, bi_clr_important: DWORD,
}
#[repr(C)]
struct RECT { left: LONG, top: LONG, right: LONG, bottom: LONG }

#[repr(C)] struct BITMAPINFO { bmi_header: BITMAPINFOHEADER, bmi_colors: [DWORD; 3] }
#[repr(C)] struct MONITORINFO {
    cb_size: DWORD, rc_monitor: RECT, rc_work: RECT, dw_flags: DWORD,
}

extern "system" {
    fn RegisterClassW(lp: *const WNDCLASSW) -> ATOM;
    fn CreateWindowExW(dw_ex: DWORD, cn: LPCWSTR, wn: LPCWSTR, style: DWORD, x: i32, y: i32, w: i32, h: i32, parent: HWND, menu: isize, inst: isize, lp: LPVOID) -> HWND;
    fn DefWindowProcW(h: HWND, m: UINT, w: WPARAM, l: LPARAM) -> LRESULT;
    fn GetMessageW(m: *mut MSG, h: HWND, min: UINT, max: UINT) -> BOOL;
    fn TranslateMessage(m: *const MSG) -> BOOL;
    fn DispatchMessageW(m: *const MSG) -> LRESULT;
    fn PostQuitMessage(ec: i32);
    fn PostMessageW(h: HWND, m: UINT, w: WPARAM, l: LPARAM) -> BOOL;
    fn SetTimer(h: HWND, id: usize, ms: UINT, cb: LPVOID) -> usize;
    fn KillTimer(h: HWND, id: usize) -> BOOL;
    fn GetModuleHandleW(n: LPCWSTR) -> isize;
    fn ShowWindow(h: HWND, cmd: i32) -> BOOL;
    fn DestroyWindow(h: HWND) -> BOOL;
    fn SetWindowPos(h: HWND, after: HWND, x: i32, y: i32, cx: i32, cy: i32, f: UINT) -> BOOL;
    fn SetWindowLongPtrW(h: HWND, idx: i32, v: isize) -> isize;
    fn GetWindowLongPtrW(h: HWND, idx: i32) -> isize;
    // GDI
    fn GetDC(h: HWND) -> HDC;
    fn ReleaseDC(h: HWND, dc: HDC) -> i32;
    fn CreateCompatibleDC(dc: HDC) -> HDC;
    fn DeleteDC(dc: HDC) -> BOOL;
    fn CreateDIBSection(dc: HDC, bmi: *const BITMAPINFO, usage: UINT, bits: *mut LPVOID, sec: isize, off: DWORD) -> isize;
    fn SelectObject(dc: HDC, o: HGDIOBJ) -> HGDIOBJ;
    fn DeleteObject(o: HGDIOBJ) -> BOOL;
    fn UpdateLayeredWindow(h: HWND, dc_dst: HDC, pt_dst: *const POINT, sz: *const SIZE, dc_src: HDC, pt_src: *const POINT, key: COLORREF, bl: *const BLENDFUNCTION, fl: DWORD) -> BOOL;
    // Multi-monitor / DPI
    fn MonitorFromPoint(x: LONG, y: LONG, flags: DWORD) -> isize;
    fn GetMonitorInfoW(monitor: isize, mi: *mut MONITORINFO) -> BOOL;
}

// ── Constants ──

const WS_EX_LAYERED: DWORD = 0x80000;
const WS_EX_TOOLWINDOW: DWORD = 0x80;
const WS_EX_TOPMOST: DWORD = 0x8;
/// DComp 专用：无重定向表面，窗口全部像素由 DComp 可视树提供（含预乘 alpha 透明）。
const WS_EX_NOREDIRECTIONBITMAP: DWORD = 0x0200_0000;
const WS_POPUP: DWORD = 0x80000000;
const WM_TIMER: UINT = 0x113;
const WM_DESTROY: UINT = 0x2;
const WM_QUIT: UINT = 0x12;
const CS_HREDRAW: UINT = 0x2;
const CS_VREDRAW: UINT = 0x1;
const SW_SHOW: i32 = 5;
const HWND_TOPMOST: HWND = -1;
const SWP_NOMOVE: UINT = 0x2;
const SWP_NOSIZE: UINT = 0x1;
const SWP_NOACTIVATE: UINT = 0x10;
const GWLP_USERDATA: i32 = -21;
const MONITOR_DEFAULTTONEAREST: DWORD = 2;
const ULW_ALPHA: DWORD = 0x2;
const AC_SRC_OVER: u8 = 0;
const AC_SRC_ALPHA: u8 = 1;
const BI_RGB: DWORD = 0;
const DIB_RGB_COLORS: UINT = 0;
const TIMER_ID: usize = 1;
const TIMER_MS: u32 = 33; // ~30fps

/// 自定义重定位消息（WM_APP 私有区间）：l_param 打包目标中心 (x, y)。
const WM_APP_SPLASH_RECENTER: UINT = 0x8000;
const SWP_NOZORDER: UINT = 0x4;

/// 窗口逻辑尺寸（与 render::SIZE 一致）。
const WIN_SIZE: i32 = super::render::SIZE as i32;

struct ThreadCtx {
    close: Arc<Mutex<bool>>,
    start: Instant,
    /// DComp 上下文——持有全部 COM 引用到窗口销毁（释放即动画/内容消失）。
    dcomp: Option<DcompSpin>,
}

/// 保活集合：任一接口释放都会中断可视树/动画。
struct DcompSpin {
    _d3d: ID3D11Device,
    _d3d_ctx: ID3D11DeviceContext,
    _dxgi_dev: IDXGIDevice,
    _factory: IDXGIFactory2,
    _swap: IDXGISwapChain1,
    _anim: IDCompositionAnimation,
    _rotate: IDCompositionRotateTransform,
    _visual: IDCompositionVisual,
    _target: IDCompositionTarget,
    _device: IDCompositionDevice,
}

/// DComp 全链路初始化：D3D11 设备（硬失败转 WARP）→ DXGI → DComp 设备 →
/// 绑定窗口 → 静态帧上 composition swapchain → 合成器驱动的无限旋转。
/// 任一步失败返回 None（调用方回退分层窗口路径）。
unsafe fn try_dcomp(hwnd: isize) -> Option<DcompSpin> {
    // 1. D3D11 设备：环只有 49px，WARP 软渲染也无压力
    let mut d3d: Option<ID3D11Device> = None;
    for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
        let mut dev: Option<ID3D11Device> = None;
        if D3D11CreateDevice(None, driver, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None, D3D11_SDK_VERSION, Some(&mut dev), None, None).is_ok()
        {
            d3d = dev;
            break;
        }
    }
    let d3d = d3d?;
    let d3d_ctx: ID3D11DeviceContext = d3d.GetImmediateContext().ok()?;

    // 2. DComp 设备 + 绑定目标窗口
    let dxgi_dev: IDXGIDevice = d3d.cast().ok()?;
    let device: IDCompositionDevice = DCompositionCreateDevice(&dxgi_dev).ok()?;
    let target: IDCompositionTarget = device.CreateTargetForHwnd(WinHWND(hwnd as _), true).ok()?;

    // 3. composition swapchain：上传一张静态帧（预乘 BGRA）后 Present 一次
    let factory: IDXGIFactory2 = CreateDXGIFactory1().ok()?;
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: super::render::SIZE,
        Height: super::render::SIZE,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        Stereo: false.into(),
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
        AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
        Flags: 0,
    };
    let swap: IDXGISwapChain1 = factory.CreateSwapChainForComposition(&d3d, &desc, None).ok()?;
    let buf_size = (super::render::SIZE * super::render::SIZE * 4) as usize;
    let mut buf = vec![0u8; buf_size];
    super::render::render_frame(&mut buf, 0.0);
    super::render::premultiply_alpha(&mut buf);
    super::render::to_bgra_inplace(&mut buf);
    let tex: ID3D11Texture2D = swap.GetBuffer(0).ok()?;
    d3d_ctx.UpdateSubresource(&tex, 0, None, buf.as_ptr() as *const core::ffi::c_void,
        super::render::SIZE * 4, 0);
    if swap.Present(1, DXGI_PRESENT(0)).is_err() { return None; }

    // 4. 可视树：静态帧内容 + 合成器驱动的线性旋转
    let visual: IDCompositionVisual = device.CreateVisual().ok()?;
    visual.SetContent(&swap).ok()?;
    let rotate: IDCompositionRotateTransform = device.CreateRotateTransform().ok()?;
    rotate.SetCenterX2(super::render::SIZE as f32 / 2.0).ok()?;
    rotate.SetCenterY2(super::render::SIZE as f32 / 2.0).ok()?;
    let anim: IDCompositionAnimation = device.CreateAnimation().ok()?;
    // 官方线性无限旋转配方：多项式系数（c=360°/s）；无后续段则无限持续，
    // DComp 正角 = 顺时针，与 tiny-skia（y 向下）逐帧渲染方向一致
    anim.AddCubic(0.0, 0.0, 360.0, 0.0, 0.0).ok()?;
    rotate.SetAngle(&anim).ok()?;
    visual.SetTransform(&rotate).ok()?;
    target.SetRoot(&visual).ok()?;
    device.Commit().ok()?;

    Some(DcompSpin {
        _d3d: d3d,
        _d3d_ctx: d3d_ctx,
        _dxgi_dev: dxgi_dev,
        _factory: factory,
        _swap: swap,
        _anim: anim,
        _rotate: rotate,
        _visual: visual,
        _target: target,
        _device: device,
    })
}

pub(crate) struct WinSplash {
    close: Arc<Mutex<bool>>,
    thread: Option<std::thread::JoinHandle<()>>,
    hwnd: HWND,
}

// SAFETY：HWND 仅作为句柄跨线程携带（listen 回调 → run_on_main_thread），
// 本类型对它的全部操作都是 PostMessageW——Win32 明确线程安全的投递 API，
// 不解引用窗口资源，不与动画线程的消息泵产生数据竞争；跨线程侧仅
// clone Arc、从不触碰内部字段。
unsafe impl Send for WinSplash {}
unsafe impl Sync for WinSplash {}

impl WinSplash {
    pub(crate) fn new() -> Option<Self> {
        let close = Arc::new(Mutex::new(false));
        let c = close.clone();
        let hwnd_slot: Arc<Mutex<HWND>> = Arc::new(Mutex::new(0));
        let hs = hwnd_slot.clone();

        let t = std::thread::spawn(move || {
            if let Err(e) = unsafe { thread_main(c, hs) } {
                tracing::warn!("Splash thread: {e}");
            }
        });

        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(5));
            if *hwnd_slot.lock().unwrap() != 0 {
                return Some(Self { close, thread: Some(t), hwnd: *hwnd_slot.lock().unwrap() });
            }
        }

        *close.lock().unwrap() = true;
        let h = *hwnd_slot.lock().unwrap();
        if h != 0 { unsafe { PostMessageW(h, WM_QUIT, 0, 0); } }
        let _ = t.join();
        None
    }

    pub(crate) fn close(&self) {
        *self.close.lock().unwrap() = true;
        if self.hwnd != 0 { unsafe { PostMessageW(self.hwnd, WM_QUIT, 0, 0); } }
    }

    /// 将环形窗口移动到 (cx, cy)（物理像素）。
    ///
    /// 经 PostMessage 投递、由动画线程自己的消息泵执行——避免跨线程
    /// SetWindowPos 的同步 SendMessage 在 blit 期间阻塞调用方。
    pub(crate) fn recenter(&self, cx: i32, cy: i32) {
        if self.hwnd != 0 {
            unsafe { PostMessageW(self.hwnd, WM_APP_SPLASH_RECENTER, 0, pack_lparam(cx, cy)); }
        }
    }
}

/// 将两个 i32 屏幕坐标打包进 l_param（多屏下坐标可为负）。
fn pack_lparam(x: i32, y: i32) -> LPARAM {
    (((y as u32 as usize) << 32) | (x as u32 as usize)) as LPARAM
}

fn unpack_lparam(l: LPARAM) -> (i32, i32) {
    ((l & 0xFFFF_FFFF) as u32 as i32, (l >> 32) as u32 as i32)
}

impl Drop for WinSplash {
    fn drop(&mut self) {
        self.close();
        if let Some(t) = self.thread.take() { let _ = t.join(); }
    }
}

unsafe fn thread_main(close: Arc<Mutex<bool>>, hwnd_slot: Arc<Mutex<HWND>>) -> Result<(), String> {
    let hinst = GetModuleHandleW(std::ptr::null());
    let cn: Vec<u16> = "XrlSplash\0".encode_utf16().collect();
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW, lpfn_wnd_proc: Some(wndproc),
        cb_cls_extra: 0, cb_wnd_extra: 0, h_instance: hinst,
        h_icon: 0, h_cursor: 0, hbr_background: 0,
        lpsz_menu_name: std::ptr::null(), lpsz_class_name: cn.as_ptr(),
    };
    if RegisterClassW(&wc) == 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(1410) { return Err(format!("RegisterClassW: {e}")); }
    }

    // 主显示器工作区中心（与 tauri.conf.json "center": true 的居中目标一致，
    // 任务栏位置免疫）。setup 阶段 recenter() 会再按实测值吸附到主窗口
    // 客户区严格中心——这里只是窗口出现前的最近似位置。
    let mon = MonitorFromPoint(0, 0, MONITOR_DEFAULTTONEAREST);
    let mut mi = MONITORINFO {
        cb_size: std::mem::size_of::<MONITORINFO>() as DWORD,
        rc_monitor: RECT { left: 0, top: 0, right: 0, bottom: 0 },
        rc_work: RECT { left: 0, top: 0, right: 1920, bottom: 1032 },
        dw_flags: 0,
    };
    if mon != 0 { GetMonitorInfoW(mon, &mut mi); }
    let x = (mi.rc_work.left + mi.rc_work.right - WIN_SIZE) / 2;
    let y = (mi.rc_work.top + mi.rc_work.bottom - WIN_SIZE) / 2;

    let make_window = |ex_style: DWORD| unsafe {
        CreateWindowExW(
            ex_style | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            cn.as_ptr(), std::ptr::null(), WS_POPUP,
            x, y, WIN_SIZE, WIN_SIZE, 0, 0, hinst, std::ptr::null_mut(),
        )
    };

    // 优先 DComp：无重定向表面窗口（预乘 alpha 由 DComp 可视树提供）
    let mut hwnd = make_window(WS_EX_NOREDIRECTIONBITMAP);
    let mut dcomp = if hwnd != 0 { unsafe { try_dcomp(hwnd) } } else { None };
    if hwnd != 0 && dcomp.is_none() {
        // DComp 不可用（极老系统 / DWM 异常）——销毁重建为分层窗口，
        // 走 WM_TIMER + UpdateLayeredWindow 的 30fps 兜底
        unsafe { DestroyWindow(hwnd); }
        hwnd = make_window(WS_EX_LAYERED);
        dcomp = None;
    }
    if hwnd == 0 { return Err(format!("CreateWindowExW: {}", std::io::Error::last_os_error())); }

    if dcomp.is_some() {
        tracing::info!("Splash: DComp compositor-driven animation");
    } else {
        tracing::info!("Splash: layered-window fallback (30fps timer)");
    }

    let ctx = Box::new(ThreadCtx { close: close.clone(), start: Instant::now(), dcomp });
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(ctx) as isize);
    *hwnd_slot.lock().unwrap() = hwnd;

    SetTimer(hwnd, TIMER_ID, TIMER_MS, std::ptr::null_mut());
    SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    ShowWindow(hwnd, SW_SHOW);

    let mut msg: MSG = std::mem::zeroed();
    loop {
        if GetMessageW(&mut msg, 0, 0, 0) <= 0 { break; }
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }

    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ThreadCtx;
    if !p.is_null() { drop(Box::from_raw(p)); }
    Ok(())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: UINT, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_APP_SPLASH_RECENTER => {
            let (cx, cy) = unpack_lparam(l);
            SetWindowPos(hwnd, 0, cx - WIN_SIZE / 2, cy - WIN_SIZE / 2, 0, 0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            0
        }
        WM_TIMER if w == TIMER_ID => {
            let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ThreadCtx;
            if p.is_null() { return DefWindowProcW(hwnd, msg, w, l); }
            let ctx = &*p;
            if *ctx.close.lock().unwrap() { KillTimer(hwnd, TIMER_ID); PostQuitMessage(0); return 0; }

            // DComp 路径：合成器自己转，本线程只轮询 close 标志
            if ctx.dcomp.is_some() { return 0; }

            let buf_size = (WIN_SIZE * WIN_SIZE * 4) as usize;
            let mut buf = vec![0u8; buf_size];
            super::render::render_frame(&mut buf, ctx.start.elapsed().as_secs_f64());
            super::render::premultiply_alpha(&mut buf);
            super::render::to_bgra_inplace(&mut buf);
            if let Err(e) = blit(hwnd, &buf, WIN_SIZE, WIN_SIZE) { tracing::warn!("Splash: {e}"); }
            0
        }
        // 注意：不能在此 PostQuitMessage——DComp 初始化失败时窗口会被
        // 销毁重建（触发本消息），退出统一由 close 标志路径驱动
        WM_DESTROY => 0,
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

unsafe fn blit(hwnd: HWND, buf: &[u8], w: i32, h: i32) -> Result<(), String> {
    let bmi = BITMAPINFO {
        bmi_header: BITMAPINFOHEADER {
            bi_size: std::mem::size_of::<BITMAPINFOHEADER>() as DWORD, bi_width: w, bi_height: -h,
            bi_planes: 1, bi_bit_count: 32, bi_compression: BI_RGB, bi_size_image: 0,
            bi_x_pels_per_meter: 0, bi_y_pels_per_meter: 0, bi_clr_used: 0, bi_clr_important: 0,
        },
        bmi_colors: [0; 3],
    };
    let dc = GetDC(0);
    if dc == 0 { return Err("GetDC".into()); }
    let mut bits: LPVOID = std::ptr::null_mut();
    let bmp = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, 0, 0);
    if bmp == 0 { ReleaseDC(0, dc); return Err("CreateDIBSection".into()); }
    std::slice::from_raw_parts_mut(bits as *mut u8, buf.len()).copy_from_slice(buf);
    let mem = CreateCompatibleDC(dc);
    let old = SelectObject(mem, bmp);
    let bl = BLENDFUNCTION { blend_op: AC_SRC_OVER, blend_flags: 0, source_constant_alpha: 255, alpha_format: AC_SRC_ALPHA };
    let pt = POINT { x: 0, y: 0 };
    let sz = SIZE { cx: w, cy: h };
    let r = UpdateLayeredWindow(hwnd, 0, std::ptr::null(), &sz, mem, &pt, 0, &bl, ULW_ALPHA);
    SelectObject(mem, old); DeleteDC(mem); DeleteObject(bmp); ReleaseDC(0, dc);
    if r == 0 { Err(format!("UpdateLayeredWindow: {}", std::io::Error::last_os_error())) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lparam_roundtrip() {
        for (x, y) in [(0, 0), (960, 540), (600, 415), (-1920, -216), (i32::MIN, i32::MAX)] {
            assert_eq!(unpack_lparam(pack_lparam(x, y)), (x, y));
        }
    }
}