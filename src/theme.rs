//! Static, native blue skin. Images are embedded and decoded once, with no repaint timer.
use windows::core::{w, BOOL};
use windows::Win32::{
    Foundation::*,
    Graphics::{
        Gdi::*,
        GdiPlus::{
            FillModeAlternate, GdipAddPathArc, GdipClosePathFigure, GdipCreateBitmapFromStream,
            GdipCreateFromHDC, GdipCreatePath, GdipCreatePen1, GdipCreateSolidFill,
            GdipDeleteBrush, GdipDeleteGraphics, GdipDeletePath, GdipDeletePen, GdipDisposeImage,
            GdipDrawImageRectRectI, GdipDrawPath, GdipFillPath, GdipSetInterpolationMode,
            GdipSetSmoothingMode, GdiplusShutdown, GdiplusStartup, GdiplusStartupInput, GpImage,
            InterpolationModeHighQualityBicubic, Ok as GDIP_OK, SmoothingModeAntiAlias, UnitPixel,
        },
    },
    System::Com::IStream,
    UI::{
        Controls::*, HiDpi::GetDpiForWindow, Input::KeyboardAndMouse::*, Shell::*,
        WindowsAndMessaging::*,
    },
};

pub const fn color(rgb: u32) -> COLORREF {
    COLORREF(((rgb & 0xff) << 16) | (rgb & 0xff00) | ((rgb >> 16) & 0xff))
}
pub const BACKGROUND: COLORREF = color(0xf4f7fd);
pub const SIDEBAR: COLORREF = color(0xeaf1fd);
pub const SURFACE: COLORREF = color(0xffffff);
pub const INK: COLORREF = color(0x20334e);
pub const MUTED: COLORREF = color(0x60738e);
pub const BLUE: COLORREF = color(0x2863df);
pub const BORDER: COLORREF = color(0xdce6f5);

struct Raster {
    image: *mut GpImage,
    _stream: IStream,
}
impl Raster {
    unsafe fn load(bytes: &[u8]) -> anyhow::Result<Self> {
        let stream =
            SHCreateMemStream(Some(bytes)).ok_or_else(|| anyhow::anyhow!("无法读取界面素材"))?;
        let mut bitmap = std::ptr::null_mut();
        anyhow::ensure!(
            GdipCreateBitmapFromStream(&stream, &mut bitmap) == GDIP_OK,
            "无法解码界面素材"
        );
        Ok(Self {
            image: bitmap.cast(),
            _stream: stream,
        })
    }
    unsafe fn draw(&self, dc: HDC, rect: RECT, source: (i32, i32, i32, i32)) {
        let mut graphics = std::ptr::null_mut();
        if GdipCreateFromHDC(dc, &mut graphics) != GDIP_OK {
            return;
        }
        GdipSetInterpolationMode(graphics, InterpolationModeHighQualityBicubic);
        GdipDrawImageRectRectI(
            graphics,
            self.image,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            source.0,
            source.1,
            source.2,
            source.3,
            UnitPixel,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
        );
        GdipDeleteGraphics(graphics);
    }
}
impl Drop for Raster {
    fn drop(&mut self) {
        unsafe {
            GdipDisposeImage(self.image);
        }
    }
}

pub struct Theme {
    icons: Option<Raster>,
    wave: Option<Raster>,
    empty: Option<Raster>,
    logo: Option<Raster>,
    token: usize,
    pub background: HBRUSH,
    pub sidebar: HBRUSH,
    pub surface: HBRUSH,
    pub icon: HICON,
}
impl Theme {
    pub unsafe fn new() -> anyhow::Result<Self> {
        let mut token = 0;
        anyhow::ensure!(
            GdiplusStartup(
                &mut token,
                &GdiplusStartupInput {
                    GdiplusVersion: 1,
                    SuppressExternalCodecs: BOOL(1),
                    ..Default::default()
                },
                std::ptr::null_mut()
            ) == GDIP_OK,
            "无法初始化界面绘制"
        );
        let mut theme = Self {
            icons: None,
            wave: None,
            empty: None,
            logo: None,
            token,
            background: CreateSolidBrush(BACKGROUND),
            sidebar: CreateSolidBrush(SIDEBAR),
            surface: CreateSolidBrush(SURFACE),
            icon: HICON::default(),
        };
        theme.icons = Some(Raster::load(include_bytes!("../assets/icons.png"))?);
        theme.wave = Some(Raster::load(include_bytes!("../assets/blue-wave.png"))?);
        theme.empty = Some(Raster::load(include_bytes!("../assets/empty-library.png"))?);
        theme.logo = Some(Raster::load(include_bytes!("../assets/logo.png"))?);
        theme.icon = CreateIconFromResourceEx(
            include_bytes!("../assets/logo.png"),
            true,
            0x30000,
            32,
            32,
            LR_DEFAULTCOLOR,
        )?;
        Ok(theme)
    }
    pub unsafe fn draw_icon(&self, dc: HDC, index: usize, variant: usize, rect: RECT) {
        if let Some(image) = &self.icons {
            image.draw(dc, rect, (index as i32 * 64, variant as i32 * 64, 64, 64));
        }
    }
    pub unsafe fn background(&self, dc: HDC, bounds: RECT, scale: f64) {
        let p = |n: i32| (n as f64 * scale).round() as i32;
        FillRect(dc, &bounds, self.background);
        let w = bounds.right;
        let h = bounds.bottom;
        if let Some(wave) = &self.wave {
            wave.draw(dc, rect(p(260), p(50), w - p(24), p(84)), (0, 0, 720, 160));
        }
        rounded(
            dc,
            rect(p(12), p(88), p(244), h - p(46)),
            SIDEBAR,
            SIDEBAR,
            p(18),
        );
        rounded(
            dc,
            rect(p(244), p(166), w - p(12), h - p(166)),
            SURFACE,
            BORDER,
            p(16),
        );
        rounded(
            dc,
            rect(p(244), h - p(166), w - p(12), h - p(38)),
            SURFACE,
            BORDER,
            p(16),
        );
        // A restrained status separator keeps the original footer geometry.
        fill(dc, rect(p(24), h - p(40), w - p(24), h - p(39)), BORDER);
    }
    pub unsafe fn brand(&self, dc: HDC, bounds: RECT, font: HFONT, scale: f64) {
        FillRect(dc, &bounds, self.background);
        let side = (29.0 * scale).round() as i32;
        if let Some(logo) = &self.logo {
            logo.draw(
                dc,
                rect(
                    bounds.left,
                    bounds.top + 2,
                    bounds.left + side,
                    bounds.top + 2 + side,
                ),
                (0, 0, 128, 128),
            );
        }
        text(
            dc,
            "云简",
            rect(
                bounds.left + side + (8.0 * scale) as i32,
                bounds.top,
                bounds.right,
                bounds.bottom,
            ),
            font,
            INK,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );
    }
    pub unsafe fn button(&self, item: &DRAWITEMSTRUCT, label: &str, font: HFONT, scale: f64) {
        let p = |n: i32| (n as f64 * scale).round() as i32;
        let primary = matches!(item.CtlID, 104 | 106 | 108);
        let disabled = item.itemState.0 & ODS_DISABLED.0 != 0;
        let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let _ = ScreenToClient(item.hwndItem, &mut point);
        let hot = point.x >= 0
            && point.y >= 0
            && point.x < item.rcItem.right
            && point.y < item.rcItem.bottom;
        let parent = if matches!(item.CtlID, 102 | 103 | 114) {
            SIDEBAR
        } else if matches!(item.CtlID, 107 | 108 | 109 | 118) {
            SURFACE
        } else {
            BACKGROUND
        };
        fill(item.hDC, item.rcItem, parent);
        let (bg, border, fg, variant) = if disabled {
            (color(0xf1f5fb), color(0xe4ebf6), color(0xa9b7cc), 3)
        } else if primary {
            (
                if pressed {
                    color(0x194dbd)
                } else if hot {
                    color(0x3974ef)
                } else {
                    BLUE
                },
                BLUE,
                SURFACE,
                1,
            )
        } else {
            (
                if pressed {
                    color(0xdce9ff)
                } else if hot {
                    color(0xe9f1ff)
                } else {
                    SURFACE
                },
                if hot { color(0xb8d1fa) } else { BORDER },
                color(0x42658f),
                0,
            )
        };
        let mut bounds = item.rcItem;
        bounds.right -= 1;
        bounds.bottom -= 1;
        rounded(item.hDC, bounds, bg, border, p(8));
        let icon = match item.CtlID {
            103 => Some(2),
            106 => Some(3),
            107 => Some(4),
            108 => Some(if label == "暂停" { 6 } else { 5 }),
            109 => Some(7),
            102 => Some(8),
            104 => Some(9),
            105 => Some(10),
            114 => Some(11),
            112 => Some(12),
            _ => None,
        };
        let mut content = bounds;
        content.left += p(10);
        content.right -= p(10);
        if pressed {
            content.top += 1;
            content.bottom += 1;
        }
        if let Some(icon) = icon {
            let side = p(16);
            let mut measured = RECT::default();
            let old = SelectObject(item.hDC, font.into());
            let mut encoded = label.encode_utf16().collect::<Vec<_>>();
            DrawTextW(
                item.hDC,
                &mut encoded,
                &mut measured,
                DT_CALCRECT | DT_SINGLELINE,
            );
            SelectObject(item.hDC, old);
            // Small controls retain their complete accessible labels; use a glyph only when it fits.
            if measured.right + side + p(6) <= content.right - content.left {
                let left = content.left
                    + ((content.right - content.left) - (measured.right + side + p(6))) / 2;
                let y = (content.top + content.bottom - side) / 2;
                self.draw_icon(
                    item.hDC,
                    icon,
                    variant,
                    rect(left, y, left + side, y + side),
                );
                content.left = left + side + p(6);
                content.right = content.left + measured.right + p(2);
            }
        }
        text(
            item.hDC,
            label,
            content,
            font,
            fg,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );
        if item.itemState.0 & ODS_FOCUS.0 != 0
            && item.itemState.0 & ODS_NOFOCUSRECT.0 == 0
            && !disabled
        {
            let focus = rect(
                bounds.left + p(4),
                bounds.top + p(4),
                bounds.right - p(4),
                bounds.bottom - p(4),
            );
            SetTextColor(item.hDC, fg);
            let _ = DrawFocusRect(item.hDC, &focus);
        }
    }
    pub unsafe fn nav(&self, item: &DRAWITEMSTRUCT, label: &str, font: HFONT, scale: f64) {
        let p = |n: i32| (n as f64 * scale).round() as i32;
        fill(item.hDC, item.rcItem, SIDEBAR);
        if item.itemID == u32::MAX {
            return;
        }
        let selected = item.itemState.0 & ODS_SELECTED.0 != 0;
        let bounds = rect(
            item.rcItem.left,
            item.rcItem.top + p(2),
            item.rcItem.right,
            item.rcItem.bottom - p(2),
        );
        if selected {
            rounded(item.hDC, bounds, BLUE, BLUE, p(8));
        }
        let side = p(18);
        let x = bounds.left + p(13);
        let y = (bounds.top + bounds.bottom - side) / 2;
        self.draw_icon(
            item.hDC,
            if item.itemID == 0 { 0 } else { 1 },
            if selected { 1 } else { 0 },
            rect(x, y, x + side, y + side),
        );
        text(
            item.hDC,
            label,
            rect(
                x + side + p(10),
                bounds.top,
                bounds.right - p(10),
                bounds.bottom,
            ),
            font,
            if selected { SURFACE } else { color(0x42658f) },
            DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
        );
        if item.itemState.0 & ODS_FOCUS.0 != 0 && item.itemState.0 & ODS_NOFOCUSRECT.0 == 0 {
            let inset = rect(
                bounds.left + p(4),
                bounds.top + p(4),
                bounds.right - p(4),
                bounds.bottom - p(4),
            );
            let _ = DrawFocusRect(item.hDC, &inset);
        }
    }
    pub unsafe fn empty(&self, dc: HDC, bounds: RECT, font: HFONT, scale: f64, local: bool) {
        let p = |n: i32| (n as f64 * scale).round() as i32;
        let height = bounds.bottom - bounds.top;
        if height < p(125) {
            return;
        }
        let iw = p(180).min((height - p(62)) * 4 / 3);
        let ih = iw * 3 / 4;
        let x = (bounds.right - iw) / 2;
        let y = (height - ih - p(32)) / 2 + p(8);
        if let Some(image) = &self.empty {
            image.draw(dc, rect(x, y, x + iw, y + ih), (0, 0, 240, 180));
        }
        text(
            dc,
            if local {
                "你的音乐，从这里开始"
            } else {
                "暂时没有找到歌曲"
            },
            rect(0, y + ih + p(2), bounds.right, y + ih + p(29)),
            font,
            MUTED,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
    }
}
impl Drop for Theme {
    fn drop(&mut self) {
        unsafe {
            self.icons.take();
            self.wave.take();
            self.empty.take();
            self.logo.take();
            let _ = DeleteObject(self.background.into());
            let _ = DeleteObject(self.sidebar.into());
            let _ = DeleteObject(self.surface.into());
            if !self.icon.0.is_null() {
                let _ = DestroyIcon(self.icon);
            }
            GdiplusShutdown(self.token);
        }
    }
}

pub fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
    RECT {
        left,
        top,
        right,
        bottom,
    }
}
pub unsafe fn fill(dc: HDC, bounds: RECT, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    FillRect(dc, &bounds, brush);
    let _ = DeleteObject(brush.into());
}
pub unsafe fn rounded(dc: HDC, bounds: RECT, bg: COLORREF, border: COLORREF, radius: i32) {
    if bounds.right <= bounds.left || bounds.bottom <= bounds.top {
        return;
    }
    let mut graphics = std::ptr::null_mut();
    if GdipCreateFromHDC(dc, &mut graphics) != GDIP_OK {
        fill(dc, bounds, bg);
        return;
    }
    GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias);
    let mut path = std::ptr::null_mut();
    if GdipCreatePath(FillModeAlternate, &mut path) != GDIP_OK {
        GdipDeleteGraphics(graphics);
        return;
    }
    let left = bounds.left as f32 + 0.5;
    let top = bounds.top as f32 + 0.5;
    let right = bounds.right as f32 - 0.5;
    let bottom = bounds.bottom as f32 - 0.5;
    let diameter = (radius * 2)
        .min(bounds.right - bounds.left - 1)
        .min(bounds.bottom - bounds.top - 1)
        .max(1) as f32;
    GdipAddPathArc(path, left, top, diameter, diameter, 180.0, 90.0);
    GdipAddPathArc(path, right - diameter, top, diameter, diameter, 270.0, 90.0);
    GdipAddPathArc(
        path,
        right - diameter,
        bottom - diameter,
        diameter,
        diameter,
        0.0,
        90.0,
    );
    GdipAddPathArc(
        path,
        left,
        bottom - diameter,
        diameter,
        diameter,
        90.0,
        90.0,
    );
    GdipClosePathFigure(path);
    let argb =
        |c: COLORREF| 0xff000000 | ((c.0 & 255) << 16) | (c.0 & 0xff00) | ((c.0 >> 16) & 255);
    let mut brush = std::ptr::null_mut();
    if GdipCreateSolidFill(argb(bg), &mut brush) == GDIP_OK {
        GdipFillPath(graphics, brush.cast(), path);
        GdipDeleteBrush(brush.cast());
    }
    let mut pen = std::ptr::null_mut();
    if GdipCreatePen1(argb(border), 1.0, UnitPixel, &mut pen) == GDIP_OK {
        GdipDrawPath(graphics, pen, path);
        GdipDeletePen(pen);
    }
    GdipDeletePath(path);
    GdipDeleteGraphics(graphics);
}

// Color callbacks are reentrant. Immutable HWND metadata keeps nested paints
// independent of the application's RefCell and prevents a system-gray fallback.
pub unsafe fn set_colors(
    hwnd: HWND,
    brush: HBRUSH,
    background: COLORREF,
    foreground: COLORREF,
) -> windows::core::Result<()> {
    SetPropW(hwnd, w!("CloudLiteBrush"), Some(HANDLE(brush.0)))?;
    SetPropW(
        hwnd,
        w!("CloudLiteBackground"),
        Some(HANDLE(background.0 as usize as *mut _)),
    )?;
    SetPropW(
        hwnd,
        w!("CloudLiteForeground"),
        Some(HANDLE(foreground.0 as usize as *mut _)),
    )?;
    Ok(())
}
pub unsafe fn control_color(message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    if !matches!(
        message,
        WM_CTLCOLORSTATIC | WM_CTLCOLORLISTBOX | WM_CTLCOLOREDIT
    ) {
        return None;
    }
    let control = HWND(lparam.0 as *mut _);
    let brush = GetPropW(control, w!("CloudLiteBrush"));
    if brush.0.is_null() {
        return None;
    }
    let dc = HDC(wparam.0 as *mut _);
    SetBkColor(
        dc,
        COLORREF(GetPropW(control, w!("CloudLiteBackground")).0 as usize as u32),
    );
    SetTextColor(
        dc,
        COLORREF(GetPropW(control, w!("CloudLiteForeground")).0 as usize as u32),
    );
    SetBkMode(dc, TRANSPARENT);
    Some(LRESULT(brush.0 as isize))
}
pub unsafe fn text(
    dc: HDC,
    label: &str,
    mut bounds: RECT,
    font: HFONT,
    color: COLORREF,
    flags: DRAW_TEXT_FORMAT,
) {
    let old = SelectObject(dc, font.into());
    SetBkMode(dc, TRANSPARENT);
    SetTextColor(dc, color);
    let mut data = label.encode_utf16().collect::<Vec<_>>();
    DrawTextW(dc, &mut data, &mut bounds, flags | DT_NOPREFIX);
    SelectObject(dc, old);
}

pub unsafe extern "system" fn button_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if message == WM_MOUSEMOVE {
        let _ = TrackMouseEvent(&mut TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: hwnd,
            dwHoverTime: 0,
        });
        let _ = InvalidateRect(Some(hwnd), None, false);
    } else if message == WM_MOUSELEAVE {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
    DefSubclassProc(hwnd, message, wparam, lparam)
}

pub unsafe extern "system" fn header_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if message != WM_PAINT {
        return DefSubclassProc(hwnd, message, wparam, lparam);
    }
    let mut paint = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut paint);
    let mut bounds = RECT::default();
    let _ = GetClientRect(hwnd, &mut bounds);
    fill(dc, bounds, color(0xf0f5fe));
    let font = HFONT(SendMessageW(hwnd, WM_GETFONT, None, None).0 as *mut _);
    let count = SendMessageW(hwnd, HDM_GETITEMCOUNT, None, None).0.max(0) as usize;
    let padding = (12.0 * GetDpiForWindow(hwnd) as f64 / 96.0).round() as i32;
    for index in 0..count {
        let mut column = RECT::default();
        SendMessageW(
            hwnd,
            HDM_GETITEMRECT,
            Some(WPARAM(index)),
            Some(LPARAM(&mut column as *mut _ as isize)),
        );
        let mut buffer = [0u16; 128];
        let mut item = HDITEMW {
            mask: HDI_TEXT,
            pszText: windows::core::PWSTR(buffer.as_mut_ptr()),
            cchTextMax: 128,
            ..Default::default()
        };
        SendMessageW(
            hwnd,
            HDM_GETITEMW,
            Some(WPARAM(index)),
            Some(LPARAM(&mut item as *mut _ as isize)),
        );
        let len = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
        column.left += padding;
        text(
            dc,
            &String::from_utf16_lossy(&buffer[..len]),
            column,
            font,
            color(0x647d9f),
            DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_END_ELLIPSIS,
        );
    }
    fill(
        dc,
        rect(0, bounds.bottom - 1, bounds.right, bounds.bottom),
        BORDER,
    );
    let _ = EndPaint(hwnd, &paint);
    LRESULT(0)
}
