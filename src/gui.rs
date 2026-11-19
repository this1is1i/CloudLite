use crate::{
    theme::{self, Theme},
    worker::{self, Command, Event},
};
use anyhow::Context;
use cloudlite::{
    cloud::Playlist,
    media::Player,
    model::{local_match, PlaybackIntent, Queue, SearchPager, Track},
};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, Sender},
        Arc,
    },
};
use windows::{
    core::{w, IInspectable, HSTRING, PCWSTR, PWSTR},
    Foundation::TypedEventHandler,
    Media::{
        MediaPlaybackType,
        Playback::{MediaPlayer, MediaPlayerFailedEventArgs},
        SystemMediaTransportControls, SystemMediaTransportControlsButton,
        SystemMediaTransportControlsButtonPressedEventArgs,
    },
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::{
            Com::*,
            LibraryLoader::GetModuleHandleW,
            SystemServices::{SS_ENDELLIPSIS, SS_OWNERDRAW, SS_RIGHT},
            WinRT::*,
        },
        UI::{Controls::*, HiDpi::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
    },
};

const APP_EVENT: u32 = WM_APP + 1;
const MEDIA_EVENT: u32 = WM_APP + 2;
const MEDIA_ENDED: u32 = WM_APP + 3;
const MEDIA_ERROR: u32 = WM_APP + 4;
const TRAY_EVENT: u32 = WM_APP + 5;
const QR_CLOSED: u32 = WM_APP + 6;
// commctrl.h defines TBM_GETPOS as WM_USER, so windows-rs does not emit an alias.
const TRACKBAR_GETPOS: u32 = WM_USER;
const ID_LOCAL: u16 = 101;
const ID_SCAN: u16 = 102;
const ID_FOLDER: u16 = 103;
const ID_LOGIN: u16 = 104;
const ID_LOGOUT: u16 = 105;
const ID_SEARCH: u16 = 106;
const ID_PREV: u16 = 107;
const ID_PLAY: u16 = 108;
const ID_NEXT: u16 = 109;
const ID_MORE: u16 = 110;
const ID_NAV: u16 = 111;
const ID_EXIT: u16 = 112;
const ID_SHOW: u16 = 113;
const ID_CLEAR: u16 = 114;
const ID_SEARCH_EDIT: u16 = 115;
const ID_MEDIA_PLAY: u16 = 116;
const ID_MEDIA_PAUSE: u16 = 117;
const ID_SHUFFLE: u16 = 118;

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    folders: Vec<PathBuf>,
    volume: u32,
    shuffle: bool,
    device_id: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            folders: Vec::new(),
            volume: 70,
            shuffle: false,
            device_id: ncm_api_rs::util::device::generate_device_id(),
        }
    }
}

struct App {
    hwnd: HWND,
    controls: HashMap<&'static str, HWND>,
    font: HFONT,
    title_font: HFONT,
    theme: Theme,
    scale: f64,
    data: PathBuf,
    settings: Settings,
    player: Player,
    commands: Option<Sender<Command>>,
    events: Option<Receiver<Event>>,
    local: Vec<Track>,
    displayed: Vec<Track>,
    playlists: Vec<Playlist>,
    queue: Queue,
    play_epoch: Arc<AtomicU64>,
    view_epoch: u64,
    login_epoch: u64,
    login_gate: Arc<AtomicU64>,
    qr_window: Option<HWND>,
    user: Option<u64>,
    local_view: bool,
    search: SearchPager,
    intent: PlaybackIntent,
    temp: Option<tempfile::NamedTempFile>,
    pending_file: Option<PathBuf>,
    now: Option<Track>,
    visible: bool,
    source_tokens: Option<(i64, i64, i64)>,
    scanning: bool,
}

pub fn run(
    data: PathBuf,
    initial_folder: Option<PathBuf>,
    initial_file: Option<PathBuf>,
) -> anyhow::Result<()> {
    unsafe {
        // Process-local DPI setup; no registry, environment, installation or file associations.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        RoInitialize(RO_INIT_SINGLETHREADED)?;
        anyhow::ensure!(
            InitCommonControlsEx(&INITCOMMONCONTROLSEX {
                dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_LISTVIEW_CLASSES | ICC_BAR_CLASSES | ICC_PROGRESS_CLASS
            })
            .as_bool(),
            "无法初始化系统控件"
        );
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = WNDCLASSW {
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hIcon: app_icon(GetSystemMetrics(SM_CXICON)),
            hInstance: instance,
            lpszClassName: w!("CloudLiteMain"),
            lpfnWndProc: Some(window_proc),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            ..Default::default()
        };
        anyhow::ensure!(RegisterClassW(&class) != 0, "无法注册主窗口");
        let qr_class = WNDCLASSW {
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hInstance: instance,
            lpszClassName: w!("CloudLiteQR"),
            lpfnWndProc: Some(qr_proc),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            ..Default::default()
        };
        RegisterClassW(&qr_class);
        let mut settings = cloudlite::persistence::load_json(&data.join("settings.json"))
            .ok()
            .and_then(|v| serde_json::from_value::<Settings>(v).ok())
            .unwrap_or_default();
        if let Some(folder) = initial_folder {
            let path = folder.canonicalize()?;
            if !settings.folders.contains(&path) {
                settings.folders.push(path);
            }
        }
        let mut queue = Queue::default();
        queue.set_shuffle(settings.shuffle);
        let local = cloudlite::persistence::load_json(&data.join("library.json"))
            .ok()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let mut app = Box::new(RefCell::new(App {
            hwnd: HWND::default(),
            controls: HashMap::new(),
            font: HFONT::default(),
            title_font: HFONT::default(),
            theme: Theme::new()?,
            scale: 1.,
            data,
            settings,
            player: Player::new()?,
            commands: None,
            events: None,
            local,
            displayed: Vec::new(),
            playlists: Vec::new(),
            queue,
            play_epoch: Arc::new(AtomicU64::new(0)),
            view_epoch: 0,
            login_epoch: 0,
            login_gate: Arc::new(AtomicU64::new(0)),
            qr_window: None,
            user: None,
            local_view: true,
            search: SearchPager::default(),
            intent: PlaybackIntent::default(),
            temp: None,
            pending_file: initial_file,
            now: None,
            visible: true,
            source_tokens: None,
            scanning: false,
        }));
        let app_ptr = app.as_mut() as *mut RefCell<App>;
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("CloudLiteMain"),
            w!("云简 · CloudLite"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1120,
            760,
            None,
            None,
            Some(instance),
            Some(app_ptr.cast()),
        )?;
        {
            let mut a = app.borrow_mut();
            a.hwnd = hwnd;
            a.scale = GetDpiForWindow(hwnd) as f64 / 96.;
            a.initialize()?;
            let mut monitor = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(
                MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
                &mut monitor,
            )
            .as_bool()
            {
                let work = monitor.rcWork;
                let width = a.px(1080).min(work.right - work.left - 16);
                let height = a.px(720).min(work.bottom - work.top - 16);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    work.left + (work.right - work.left - width) / 2,
                    work.top + (work.bottom - work.top - height) / 2,
                    width,
                    height,
                    SWP_NOZORDER,
                );
                a.layout();
            }
        }
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);
        let mut message = MSG::default();
        loop {
            let result = GetMessageW(&mut message, None, 0, 0).0;
            if result == 0 {
                break;
            }
            if result == -1 {
                return Err(windows::core::Error::from_win32().into());
            }
            if message.message == WM_KEYDOWN && message.wParam.0 == VK_RETURN.0 as usize {
                let focus = GetFocus();
                let mut a = app.borrow_mut();
                if focus == a.control("search") {
                    a.search(false);
                    continue;
                }
                if focus == a.control("list") {
                    a.play_selected();
                    continue;
                }
            }
            if !IsDialogMessageW(hwnd, &message).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        {
            let a = app.borrow();
            a.save_settings();
            let _ = a.player.pause();
            a.tray(NIM_DELETE);
        }
        drop(app);
        RoUninitialize();
    }
    Ok(())
}

/// Loads the multi-size icon embedded by build.rs (resource id 1). An empty handle means
/// the build did not find rc.exe, in which case callers fall back to the drawn brand icon.
unsafe fn app_icon(size: i32) -> HICON {
    let instance: HINSTANCE = match GetModuleHandleW(None) {
        Ok(module) => module.into(),
        Err(_) => return HICON::default(),
    };
    LoadImageW(
        Some(instance),
        PCWSTR(1 as *const u16),
        IMAGE_ICON,
        size,
        size,
        LR_DEFAULTCOLOR,
    )
    .map(|handle| HICON(handle.0))
    .unwrap_or_default()
}

pub fn pick_folder_helper(owner: usize) -> anyhow::Result<Option<PathBuf>> {
    unsafe {
        RoInitialize(RO_INIT_SINGLETHREADED)?;
        let owner = HWND(owner as *mut _);
        if !owner.0.is_null() {
            let mut pid = 0;
            GetWindowThreadProcessId(owner, Some(&mut pid));
            if pid != 0 {
                use windows::Win32::System::Threading::*;
                if let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                    let handle = handle.0 as usize;
                    std::thread::spawn(move || {
                        WaitForSingleObject(HANDLE(handle as *mut _), u32::MAX);
                        let _ = CloseHandle(HANDLE(handle as *mut _));
                        std::process::exit(0);
                    });
                }
            }
        }
        let result = App::pick_folder(owner);
        RoUninitialize();
        result
    }
}

impl App {
    fn control(&self, name: &str) -> HWND {
        *self.controls.get(name).unwrap_or(&HWND::default())
    }
    fn send(&self, command: Command) {
        if let Some(tx) = &self.commands {
            let _ = tx.send(command);
        }
    }
    fn px(&self, n: i32) -> i32 {
        (n as f64 * self.scale).round() as i32
    }
    fn save_settings(&self) {
        match serde_json::to_value(&self.settings)
            .map_err(anyhow::Error::from)
            .and_then(|value| {
                cloudlite::persistence::save_json(&self.data.join("settings.json"), &value)
            }) {
            Ok(()) => {}
            Err(error) => self.status(&format!("设置未能保存：{error:#}")),
        }
    }
    fn text(&self, name: &str, value: &str) {
        unsafe {
            let _ = SetWindowTextW(self.control(name), &HSTRING::from(value));
        }
    }
    fn status(&self, value: &str) {
        self.text("status", value);
    }
    unsafe fn add(
        &mut self,
        name: &'static str,
        class: PCWSTR,
        text: &str,
        style: u32,
        id: u16,
    ) -> anyhow::Result<HWND> {
        let is_button = matches!(
            name,
            "login"
                | "logout"
                | "folder"
                | "scan"
                | "clear"
                | "search_button"
                | "more"
                | "prev"
                | "play"
                | "next"
                | "shuffle"
                | "exit"
        );
        let style = if is_button {
            style | BS_OWNERDRAW as u32
        } else {
            style
        };
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            &HSTRING::from(text),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | style),
            0,
            0,
            10,
            10,
            Some(self.hwnd),
            Some(HMENU(id as usize as *mut _)),
            Some(GetModuleHandleW(None)?.into()),
            None,
        )?;
        SendMessageW(
            hwnd,
            WM_SETFONT,
            Some(WPARAM(self.font.0 as usize)),
            Some(LPARAM(1)),
        );
        let (background, brush) = if matches!(name, "nav" | "nav_title") {
            (theme::SIDEBAR, self.theme.sidebar)
        } else if matches!(
            name,
            "now" | "time" | "volume_label" | "volume" | "seek" | "search" | "list"
        ) {
            (theme::SURFACE, self.theme.surface)
        } else {
            (theme::BACKGROUND, self.theme.background)
        };
        theme::set_colors(
            hwnd,
            brush,
            background,
            if matches!(name, "title" | "now" | "search") {
                theme::INK
            } else {
                theme::MUTED
            },
        )?;
        self.controls.insert(name, hwnd);
        if is_button {
            let _ = SetWindowSubclass(hwnd, Some(theme::button_proc), 1, 0);
        }
        Ok(hwnd)
    }
    unsafe fn reset_fonts(&mut self) {
        let old_font = self.font;
        let old_title = self.title_font;
        self.font = CreateFontW(
            -self.px(15),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Microsoft YaHei UI"),
        );
        self.title_font = CreateFontW(
            -self.px(24),
            0,
            0,
            0,
            600,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Microsoft YaHei UI"),
        );
        for (name, control) in &self.controls {
            let font = if matches!(*name, "brand" | "title") {
                self.title_font
            } else {
                self.font
            };
            SendMessageW(
                *control,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
        }
        if !old_font.0.is_null() {
            let _ = DeleteObject(old_font.into());
        }
        if !old_title.0.is_null() {
            let _ = DeleteObject(old_title.into());
        }
    }
    unsafe fn initialize(&mut self) -> anyhow::Result<()> {
        self.reset_fonts();
        self.add("brand", w!("STATIC"), "云简", SS_OWNERDRAW.0 as u32, 0)?;
        for (kind, size) in [
            (ICON_SMALL, GetSystemMetrics(SM_CXSMICON)),
            (ICON_BIG, GetSystemMetrics(SM_CXICON)),
        ] {
            let icon = app_icon(size);
            let icon = if icon.0.is_null() {
                self.theme.icon
            } else {
                icon
            };
            SendMessageW(
                self.hwnd,
                WM_SETICON,
                Some(WPARAM(kind as usize)),
                Some(LPARAM(icon.0 as isize)),
            );
        }
        SendMessageW(
            self.control("brand"),
            WM_SETFONT,
            Some(WPARAM(self.title_font.0 as usize)),
            Some(LPARAM(1)),
        );
        self.add("tagline", w!("STATIC"), "音乐，回归简单。", 0, 0)?;
        self.add(
            "account",
            w!("STATIC"),
            "未登录 · 本地音乐可直接播放",
            SS_RIGHT.0 as u32,
            0,
        )?;
        self.add("login", w!("BUTTON"), "扫码登录", WS_TABSTOP.0, ID_LOGIN)?;
        self.add("logout", w!("BUTTON"), "退出账号", WS_TABSTOP.0, ID_LOGOUT)?;
        self.add("nav_title", w!("STATIC"), "我的音乐", 0, 0)?;
        self.add(
            "nav",
            w!("LISTBOX"),
            "",
            WS_TABSTOP.0
                | WS_VSCROLL.0
                | LBS_NOTIFY as u32
                | LBS_NOINTEGRALHEIGHT as u32
                | LBS_OWNERDRAWFIXED as u32
                | LBS_HASSTRINGS as u32,
            ID_NAV,
        )?;
        SendMessageW(
            self.control("nav"),
            LB_SETITEMHEIGHT,
            Some(WPARAM(0)),
            Some(LPARAM(self.px(42) as isize)),
        );
        self.add(
            "folder",
            w!("BUTTON"),
            "添加音乐文件夹",
            WS_TABSTOP.0,
            ID_FOLDER,
        )?;
        self.add("scan", w!("BUTTON"), "重新扫描", WS_TABSTOP.0, ID_SCAN)?;
        self.add("clear", w!("BUTTON"), "管理文件夹", WS_TABSTOP.0, ID_CLEAR)?;
        self.add("title", w!("STATIC"), "本地曲库", 0, 0)?;
        self.add(
            "search",
            w!("EDIT"),
            "",
            WS_TABSTOP.0 | ES_AUTOHSCROLL as u32,
            ID_SEARCH_EDIT,
        )?;
        SendMessageW(
            self.control("search"),
            EM_SETCUEBANNER,
            Some(WPARAM(0)),
            Some(LPARAM(w!("搜索歌曲或歌手").as_ptr() as isize)),
        );
        self.add(
            "search_button",
            w!("BUTTON"),
            "在线搜索",
            WS_TABSTOP.0,
            ID_SEARCH,
        )?;
        self.add("more", w!("BUTTON"), "加载更多", WS_TABSTOP.0, ID_MORE)?;
        self.add(
            "hint",
            w!("STATIC"),
            "添加音乐文件夹，或搜索网易云歌曲。",
            0,
            0,
        )?;
        let list = self.add(
            "list",
            w!("SysListView32"),
            "",
            WS_TABSTOP.0 | LVS_REPORT | LVS_OWNERDATA | LVS_SHOWSELALWAYS | LVS_SINGLESEL,
            0,
        )?;
        SendMessageW(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            Some(WPARAM(0)),
            Some(LPARAM(
                (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize,
            )),
        );
        SetWindowTheme(list, w!("Explorer"), PCWSTR::null())?;
        SendMessageW(
            list,
            LVM_SETBKCOLOR,
            None,
            Some(LPARAM(theme::SURFACE.0 as isize)),
        );
        SendMessageW(
            list,
            LVM_SETTEXTBKCOLOR,
            None,
            Some(LPARAM(theme::SURFACE.0 as isize)),
        );
        SendMessageW(
            list,
            LVM_SETTEXTCOLOR,
            None,
            Some(LPARAM(theme::INK.0 as isize)),
        );
        let header = HWND(SendMessageW(list, LVM_GETHEADER, None, None).0 as *mut _);
        let _ = SetWindowSubclass(header, Some(theme::header_proc), 1, 0);
        for (i, (name, width)) in [
            ("歌曲", 300),
            ("歌手", 150),
            ("专辑", 170),
            ("时长", 70),
            ("来源", 90),
        ]
        .iter()
        .enumerate()
        {
            let mut text = name.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
            let col = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH | LVCF_SUBITEM,
                cx: self.px(*width),
                pszText: PWSTR(text.as_mut_ptr()),
                iSubItem: i as i32,
                ..Default::default()
            };
            SendMessageW(
                list,
                LVM_INSERTCOLUMNW,
                Some(WPARAM(i)),
                Some(LPARAM(&col as *const _ as isize)),
            );
        }
        self.add("now", w!("STATIC"), "尚未播放", SS_ENDELLIPSIS.0 as u32, 0)?;
        self.add("prev", w!("BUTTON"), "上一首", WS_TABSTOP.0, ID_PREV)?;
        self.add("play", w!("BUTTON"), "播放", WS_TABSTOP.0, ID_PLAY)?;
        self.add("next", w!("BUTTON"), "下一首", WS_TABSTOP.0, ID_NEXT)?;
        // The label names the next action, matching the play/pause button.
        self.add(
            "shuffle",
            w!("BUTTON"),
            if self.queue.shuffle() {
                "关闭随机"
            } else {
                "开启随机"
            },
            WS_TABSTOP.0,
            ID_SHUFFLE,
        )?;
        self.add(
            "seek",
            w!("msctls_trackbar32"),
            "",
            WS_TABSTOP.0 | TBS_HORZ | TBS_NOTICKS,
            0,
        )?;
        SendMessageW(
            self.control("seek"),
            TBM_SETRANGEMAX,
            Some(WPARAM(0)),
            Some(LPARAM(1000)),
        );
        self.add("time", w!("STATIC"), "0:00 / 0:00", SS_RIGHT.0 as u32, 0)?;
        self.add("volume_label", w!("STATIC"), "音量", 0, 0)?;
        self.add(
            "volume",
            w!("msctls_trackbar32"),
            "",
            WS_TABSTOP.0 | TBS_HORZ | TBS_NOTICKS,
            0,
        )?;
        SendMessageW(
            self.control("volume"),
            TBM_SETRANGEMAX,
            Some(WPARAM(0)),
            Some(LPARAM(100)),
        );
        SendMessageW(
            self.control("volume"),
            TBM_SETPOS,
            Some(WPARAM(1)),
            Some(LPARAM(self.settings.volume as isize)),
        );
        self.player.set_volume(self.settings.volume)?;
        self.add(
            "status",
            w!("STATIC"),
            "就绪 · 双击歌曲播放 · 本地文件优先",
            SS_ENDELLIPSIS.0 as u32,
            0,
        )?;
        self.add("exit", w!("BUTTON"), "退出", WS_TABSTOP.0, ID_EXIT)?;
        let progress = self.add("progress", PROGRESS_CLASSW, "", PBS_SMOOTH as u32, 0)?;
        let _ = ShowWindow(progress, SW_HIDE);
        // Apply fonts to the completed control set, including the section heading.
        self.reset_fonts();
        let hwnd = self.hwnd.0 as usize;
        let (commands, events) = worker::spawn(
            self.data.clone(),
            self.settings.device_id.clone(),
            self.play_epoch.clone(),
            self.login_gate.clone(),
            move || {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), APP_EVENT, WPARAM(0), LPARAM(0));
            },
        );
        self.commands = Some(commands);
        self.events = Some(events);
        self.media_events()?;
        self.tray(NIM_ADD);
        DragAcceptFiles(self.hwnd, true);
        self.refresh_nav();
        self.show_local();
        self.layout();
        self.save_settings();
        self.send(Command::Restore);
        if !self.settings.folders.is_empty()
            && (self.local.is_empty() || self.pending_file.is_some())
        {
            self.scan();
        }
        if let Some(path) = self.pending_file.take() {
            self.open_file(path)?;
        }
        Ok(())
    }
    unsafe fn layout(&self) {
        let mut rect = RECT::default();
        let _ = GetClientRect(self.hwnd, &mut rect);
        let w = rect.right;
        let h = rect.bottom;
        let p = |n| self.px(n);
        let side = p(208);
        let x = p(24) + side + p(24);
        let right = w - p(24);
        let body = right - x;
        let set = |name: &str, left: i32, top: i32, width: i32, height: i32| {
            let _ = MoveWindow(
                self.control(name),
                left,
                top,
                width.max(1),
                height.max(1),
                true,
            );
        };
        set("brand", p(24), p(16), p(100), p(34));
        set("tagline", p(24), p(53), p(240), p(24));
        set("account", w - p(620), p(29), p(365), p(26));
        set("login", w - p(236), p(22), p(100), p(34));
        set("logout", w - p(124), p(22), p(100), p(34));
        set("nav_title", p(24), p(102), side, p(24));
        set("nav", p(24), p(136), side, h - p(340));
        set("folder", p(24), h - p(182), side, p(34));
        set("scan", p(24), h - p(138), side, p(34));
        set("clear", p(24), h - p(94), side, p(34));
        set("title", x, p(100), p(170), p(30));
        set("search", x + p(175), p(96), body - p(279), p(32));
        set("search_button", right - p(96), p(96), p(96), p(32));
        set("hint", x, p(142), body - p(112), p(24));
        set("more", right - p(100), p(136), p(100), p(30));
        set("list", x, p(178), body, h - p(354));
        set("progress", x, h - p(172), body, p(6));
        set("now", x, h - p(156), body, p(28));
        set("prev", x, h - p(114), p(78), p(34));
        set("play", x + p(88), h - p(114), p(78), p(34));
        set("next", x + p(176), h - p(114), p(78), p(34));
        set("shuffle", x + p(266), h - p(114), p(96), p(34));
        set("volume_label", right - p(166), h - p(108), p(40), p(24));
        set("volume", right - p(120), h - p(112), p(120), p(30));
        set("seek", x, h - p(68), body - p(145), p(28));
        set("time", right - p(138), h - p(64), p(138), p(24));
        set("status", p(24), h - p(30), w - p(120), p(22));
        set("exit", right - p(60), h - p(34), p(60), p(28));
        let widths = [body - p(405), p(130), p(135), p(65), p(75)];
        for (i, width) in widths.iter().enumerate() {
            SendMessageW(
                self.control("list"),
                LVM_SETCOLUMNWIDTH,
                Some(WPARAM(i)),
                Some(LPARAM(*width as isize)),
            );
        }
        SendMessageW(
            self.control("search"),
            EM_SETMARGINS,
            Some(WPARAM((EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize)),
            Some(LPARAM((p(12) | (p(12) << 16)) as isize)),
        );
        SendMessageW(
            self.control("nav"),
            LB_SETITEMHEIGHT,
            Some(WPARAM(0)),
            Some(LPARAM(p(42) as isize)),
        );
        let _ = InvalidateRect(Some(self.hwnd), None, false);
    }
    unsafe fn refresh_nav(&self) {
        let nav = self.control("nav");
        SendMessageW(nav, LB_RESETCONTENT, None, None);
        for text in std::iter::once("本地曲库".to_string())
            .chain(std::iter::once("我的歌单 · 刷新".to_string()))
            .chain(
                self.playlists
                    .iter()
                    .map(|p| format!("{} ({})", p.name, p.count)),
            )
        {
            let text = HSTRING::from(text);
            SendMessageW(
                nav,
                LB_ADDSTRING,
                None,
                Some(LPARAM(text.as_ptr() as isize)),
            );
        }
        SendMessageW(nav, LB_SETCURSEL, Some(WPARAM(0)), None);
    }
    unsafe fn update_list(&self) {
        let list = self.control("list");
        SendMessageW(
            list,
            LVM_SETITEMCOUNT,
            Some(WPARAM(self.displayed.len())),
            Some(LPARAM(0)),
        );
        let _ = InvalidateRect(Some(list), None, true);
        let _ = EnableWindow(self.control("more"), self.search.has_more());
    }
    unsafe fn show_local(&mut self) {
        self.local_view = true;
        self.search = SearchPager::default();
        self.view_epoch += 1;
        self.displayed = self.local.clone();
        self.text("title", "本地曲库");
        self.text(
            "hint",
            &format!("{} 首 · 支持普通音频和 NCM · 双击播放", self.local.len()),
        );
        self.update_list();
    }
    unsafe fn scan(&mut self) {
        if self.scanning {
            self.status("正在扫描本地音乐，请稍候。");
            return;
        }
        self.scanning = true;
        let bar = self.control("progress");
        let _ = SendMessageW(bar, PBM_SETRANGE32, Some(WPARAM(0)), Some(LPARAM(100)));
        let _ = SendMessageW(bar, PBM_SETPOS, Some(WPARAM(0)), Some(LPARAM(0)));
        let _ = ShowWindow(bar, SW_SHOW);
        self.status("正在扫描本地音乐… 原始文件保持不变。");
        self.send(Command::Scan(self.settings.folders.clone()));
    }
    unsafe fn pick_folder(owner: HWND) -> anyhow::Result<Option<PathBuf>> {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetOptions(FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST)?;
        dialog.SetTitle(w!("选择音乐文件夹"))?;
        if dialog.Show(Some(owner)).is_err() {
            return Ok(None);
        }
        let result = dialog.GetResult()?;
        let path = result.GetDisplayName(SIGDN_FILESYSPATH)?;
        let value = path.to_string();
        CoTaskMemFree(Some(path.0.cast()));
        Ok(Some(PathBuf::from(value?).canonicalize()?))
    }
    unsafe fn confirm_clear(owner: HWND, folders: &str) -> bool {
        let text = if folders.is_empty() {
            "还没有添加音乐文件夹。".into()
        } else {
            format!("当前音乐来源：\n\n{folders}\n\n是否清空播放器内的文件夹列表？\n音乐文件不会被删除。")
        };
        if folders.is_empty() {
            MessageBoxW(Some(owner), &HSTRING::from(text), w!("音乐文件夹"), MB_OK);
            return false;
        }
        MessageBoxW(
            Some(owner),
            &HSTRING::from(text),
            w!("管理音乐文件夹"),
            MB_YESNO | MB_ICONQUESTION,
        ) == IDYES
    }
    unsafe fn search(&mut self, append: bool) {
        let (keywords, offset) = if append {
            let Some(page) = self.search.next_page() else {
                return;
            };
            page
        } else {
            let keywords = window_text(self.control("search")).trim().to_string();
            if keywords.is_empty() {
                self.status("请输入歌曲或歌手名称。");
                return;
            }
            self.view_epoch += 1;
            self.displayed.clear();
            self.local_view = false;
            self.search.begin(self.view_epoch, keywords)
        };
        self.update_list();
        self.text("title", "网易云搜索");
        self.status("正在搜索…");
        self.send(Command::Search {
            epoch: self.view_epoch,
            keywords,
            offset,
        });
    }
    unsafe fn nav_selection(&mut self) {
        let index = SendMessageW(self.control("nav"), LB_GETCURSEL, None, None).0;
        if index == 0 {
            self.show_local();
        } else if index == 1 {
            if let Some(user) = self.user {
                self.status("正在刷新我的歌单…");
                self.send(Command::Playlists(user));
            } else {
                self.status("请先扫码登录网易云账号。");
            }
        } else if index >= 2 {
            if let Some(playlist) = self.playlists.get(index as usize - 2) {
                let id = playlist.id;
                let name = playlist.name.clone();
                self.local_view = false;
                self.view_epoch += 1;
                self.search = SearchPager::default();
                self.displayed.clear();
                self.update_list();
                self.text("title", &name);
                self.text("hint", "正在读取完整歌单…");
                self.status("正在加载歌单全部曲目，请稍候…");
                self.send(Command::Playlist {
                    epoch: self.view_epoch,
                    id,
                });
            }
        }
    }
    unsafe fn play_selected(&mut self) {
        let selected = SendMessageW(
            self.control("list"),
            LVM_GETNEXTITEM,
            Some(WPARAM(usize::MAX)),
            Some(LPARAM(LVNI_SELECTED as isize)),
        )
        .0;
        if selected < 0 {
            self.status("请先选中一首歌曲，或双击歌曲播放。");
            return;
        }
        self.queue
            .replace(self.displayed.clone(), selected as usize);
        if let Some(track) = self.displayed.get(selected as usize).cloned() {
            self.resolve(track);
        }
    }
    fn resolve(&mut self, track: Track) {
        // Stop the old track immediately. An older in-flight request cannot replace this selection.
        let _ = self.player.pause();
        let epoch = self.play_epoch.fetch_add(1, Ordering::SeqCst) + 1;
        self.intent.begin(epoch);
        self.now = None;
        let local = track.path.clone().filter(|p| p.is_file()).or_else(|| {
            local_match(&track, &self.local)
                .and_then(|s| s.path.clone())
                .filter(|p| p.is_file())
        });
        self.text(
            "now",
            &format!("正在载入：{} — {}", track.title, track.artist),
        );
        self.status("正在准备播放…");
        self.send(Command::Resolve {
            epoch,
            track,
            local,
        });
    }
    fn next(&mut self, delta: i32) {
        if let Some(track) = self.queue.advance(delta) {
            self.resolve(track);
        } else {
            let _ = self.player.pause();
            if self.queue.shuffle() {
                self.status(if delta > 0 {
                    "随机播放：队列里没有其他歌曲。"
                } else {
                    "随机播放：没有可以返回的上一首。"
                });
            } else {
                self.status(if delta > 0 {
                    "已到播放队列末尾。"
                } else {
                    "已到播放队列开头。"
                });
            }
        }
    }
    fn toggle_shuffle(&mut self) {
        self.settings.shuffle = !self.queue.shuffle();
        self.queue.set_shuffle(self.settings.shuffle);
        self.text(
            "shuffle",
            if self.settings.shuffle {
                "关闭随机"
            } else {
                "开启随机"
            },
        );
        unsafe {
            let _ = InvalidateRect(Some(self.control("shuffle")), None, true);
        }
        self.save_settings();
        self.status(if self.settings.shuffle {
            "已开启随机播放：每轮重新洗牌，一轮内不重复。"
        } else {
            "已关闭随机播放：按列表顺序播放。"
        });
    }
    unsafe fn toggle(&mut self) {
        if self.intent.pending {
            self.intent.set_playing(!self.intent.playing);
            self.text(
                "play",
                if self.intent.playing {
                    "暂停"
                } else {
                    "播放"
                },
            );
            return;
        }
        if self.now.is_none() {
            self.play_selected();
            return;
        }
        self.set_playing(!self.player.is_playing());
    }
    unsafe fn set_playing(&mut self, playing: bool) {
        self.intent.set_playing(playing);
        if self.intent.pending {
            self.text("play", if playing { "暂停" } else { "播放" });
            return;
        }
        if self.now.is_none() {
            if playing {
                self.play_selected();
            }
            return;
        }
        if playing {
            let _ = self.player.resume();
        } else {
            let _ = self.player.pause();
        }
        self.sync_play_state();
    }
    unsafe fn open_file(&mut self, path: PathBuf) -> anyhow::Result<()> {
        let (mut tracks, _) = cloudlite::library::scan(&[path.canonicalize()?]);
        let track = tracks.pop().context("此文件无法识别为音乐")?;
        self.queue.replace(vec![track.clone()], 0);
        self.resolve(track);
        Ok(())
    }
    unsafe fn media_events(&self) -> anyhow::Result<()> {
        let hwnd = self.hwnd.0 as usize;
        self.player
            .native
            .PlaybackSession()?
            .PlaybackStateChanged(&TypedEventHandler::new(move |_, _| {
                let _ = PostMessageW(
                    Some(HWND(hwnd as *mut _)),
                    MEDIA_EVENT,
                    WPARAM(0),
                    LPARAM(0),
                );
                Ok(())
            }))?;
        if let Ok(smtc) = self.player.native.SystemMediaTransportControls() {
            self.player.native.CommandManager()?.SetIsEnabled(false)?;
            smtc.SetIsEnabled(true)?;
            smtc.SetIsPlayEnabled(true)?;
            smtc.SetIsPauseEnabled(true)?;
            smtc.SetIsNextEnabled(true)?;
            smtc.SetIsPreviousEnabled(true)?;
            smtc.ButtonPressed(&TypedEventHandler::<
                SystemMediaTransportControls,
                SystemMediaTransportControlsButtonPressedEventArgs,
            >::new(move |_, args| {
                if let Some(args) = args.as_ref() {
                    let id = match args.Button()? {
                        SystemMediaTransportControlsButton::Next => ID_NEXT,
                        SystemMediaTransportControlsButton::Previous => ID_PREV,
                        SystemMediaTransportControlsButton::Play => ID_MEDIA_PLAY,
                        SystemMediaTransportControlsButton::Pause
                        | SystemMediaTransportControlsButton::Stop => ID_MEDIA_PAUSE,
                        _ => return Ok(()),
                    };
                    let _ = PostMessageW(
                        Some(HWND(hwnd as *mut _)),
                        WM_COMMAND,
                        WPARAM(id as usize),
                        LPARAM(0),
                    );
                }
                Ok(())
            }))?;
        }
        Ok(())
    }
    unsafe fn source_events(&mut self, epoch: u64) -> anyhow::Result<()> {
        if let Some((opened, ended, failed)) = self.source_tokens.take() {
            let _ = self.player.native.RemoveMediaOpened(opened);
            let _ = self.player.native.RemoveMediaEnded(ended);
            let _ = self.player.native.RemoveMediaFailed(failed);
        }
        let hwnd = self.hwnd.0 as usize;
        let ended =
            self.player
                .native
                .MediaEnded(&TypedEventHandler::<MediaPlayer, IInspectable>::new(
                    move |_, _| {
                        let _ = PostMessageW(
                            Some(HWND(hwnd as *mut _)),
                            MEDIA_ENDED,
                            WPARAM(epoch as usize),
                            LPARAM(0),
                        );
                        Ok(())
                    },
                ))?;
        let opened = self.player.native.MediaOpened(&TypedEventHandler::<
            MediaPlayer,
            IInspectable,
        >::new(move |_, _| {
            let _ = PostMessageW(
                Some(HWND(hwnd as *mut _)),
                MEDIA_EVENT,
                WPARAM(epoch as usize),
                LPARAM(0),
            );
            Ok(())
        }))?;
        let failed = self.player.native.MediaFailed(&TypedEventHandler::<
            MediaPlayer,
            MediaPlayerFailedEventArgs,
        >::new(move |_, args| {
            let code = args
                .as_ref()
                .and_then(|a| a.ExtendedErrorCode().ok())
                .map(|h| h.0 as usize)
                .unwrap_or(0);
            let _ = PostMessageW(
                Some(HWND(hwnd as *mut _)),
                MEDIA_ERROR,
                WPARAM(code),
                LPARAM(epoch as isize),
            );
            Ok(())
        }))?;
        self.source_tokens = Some((opened, ended, failed));
        Ok(())
    }
    unsafe fn sync_play_state(&self) {
        let playing = if self.intent.pending {
            self.intent.playing
        } else {
            self.player.is_playing()
        };
        self.text("play", if playing { "暂停" } else { "播放" });
        if self.visible && playing {
            SetTimer(Some(self.hwnd), 1, 500, None);
        } else {
            let _ = KillTimer(Some(self.hwnd), 1);
        }
        if let Ok(smtc) = self.player.native.SystemMediaTransportControls() {
            let _ = smtc.SetPlaybackStatus(if playing {
                windows::Media::MediaPlaybackStatus::Playing
            } else {
                windows::Media::MediaPlaybackStatus::Paused
            });
        }
        self.progress();
    }
    unsafe fn progress(&self) {
        let elapsed = self.player.position_ms();
        let total = self.player.duration_ms();
        SendMessageW(
            self.control("seek"),
            TBM_SETPOS,
            Some(WPARAM(1)),
            Some(LPARAM(if total > 0 {
                (elapsed.saturating_mul(1000) / total).min(1000) as isize
            } else {
                0
            })),
        );
        self.text("time", &format!("{} / {}", time(elapsed), time(total)));
    }
    unsafe fn tray(&self, message: NOTIFY_ICON_MESSAGE) {
        let mut info = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: TRAY_EVENT,
            hIcon: self.theme.icon,
            ..Default::default()
        };
        let title = "云简 · 双击显示播放器".encode_utf16().collect::<Vec<_>>();
        info.szTip[..title.len()].copy_from_slice(&title);
        let _ = Shell_NotifyIconW(message, &info);
    }
    unsafe fn restore(&mut self) {
        self.visible = true;
        let _ = ShowWindow(self.hwnd, SW_RESTORE);
        let _ = SetForegroundWindow(self.hwnd);
        self.sync_play_state();
    }
    unsafe fn tray_menu(&mut self) {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        for (id, text) in [
            (ID_SHOW, "显示云简"),
            (ID_PLAY, "播放 / 暂停"),
            (ID_PREV, "上一首"),
            (ID_NEXT, "下一首"),
            (
                ID_SHUFFLE,
                if self.queue.shuffle() {
                    "关闭随机播放"
                } else {
                    "开启随机播放"
                },
            ),
            (ID_EXIT, "退出"),
        ] {
            let _ = AppendMenuW(menu, MF_STRING, id as usize, &HSTRING::from(text));
        }
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let _ = SetForegroundWindow(self.hwnd);
        let command = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD,
            cursor.x,
            cursor.y,
            None,
            self.hwnd,
            None,
        )
        .0;
        let _ = DestroyMenu(menu);
        if command > 0 {
            let _ = PostMessageW(
                Some(self.hwnd),
                WM_COMMAND,
                WPARAM(command as usize),
                LPARAM(0),
            );
        }
    }
    unsafe fn begin_login(&mut self) {
        self.login_epoch += 1;
        self.login_gate.store(self.login_epoch, Ordering::SeqCst);
        if let Some(window) = self.qr_window.take() {
            let _ = DestroyWindow(window);
        }
        self.status("正在生成登录二维码…");
        self.send(Command::Login(self.login_epoch));
    }
    unsafe fn poll_events(&mut self) {
        while let Some(event) = self.events.as_ref().and_then(|rx| rx.try_recv().ok()) {
            match event {
                Event::FolderPicked(result) => {
                    let _ = EnableWindow(self.control("folder"), true);
                    match result {
                        Ok(Some(path)) => {
                            if !self.settings.folders.contains(&path) {
                                self.settings.folders.push(path);
                                self.save_settings();
                            }
                            self.scan();
                        }
                        Ok(None) => self.status("已取消选择音乐目录。"),
                        Err(error) => self.status(&format!("添加目录失败：{error}")),
                    }
                }
                Event::Account { epoch, account } => {
                    if epoch.is_some_and(|epoch| epoch != self.login_gate.load(Ordering::SeqCst)) {
                        continue;
                    }
                    self.user = account.as_ref().map(|a| a.0);
                    self.text(
                        "account",
                        &account
                            .as_ref()
                            .map(|a| format!("已登录 · {}", a.1))
                            .unwrap_or("未登录 · 本地音乐可直接播放".into()),
                    );
                    if let Some((user, _)) = account {
                        self.status("已登录，正在加载我的歌单…");
                        self.send(Command::Playlists(user));
                    } else {
                        self.playlists.clear();
                        self.refresh_nav();
                        self.status("已退出本机账号。本地音乐仍可播放。");
                    }
                }
                Event::Playlists(list) => {
                    self.playlists = list;
                    self.refresh_nav();
                    self.status(&format!(
                        "已读取 {} 个歌单。点击左侧歌单打开。",
                        self.playlists.len()
                    ));
                }
                Event::Tracks {
                    epoch,
                    tracks,
                    offset,
                    total,
                } => {
                    if epoch != self.view_epoch {
                        continue;
                    }
                    if !self.search.query.is_empty()
                        && !self.search.accept(epoch, offset, tracks.len(), total)
                    {
                        continue;
                    }
                    if offset == 0 {
                        self.displayed = tracks;
                    } else {
                        self.displayed.extend(tracks);
                    }
                    self.text(
                        "hint",
                        &format!(
                            "已加载 {} / {} 首 · 播放优先匹配本地文件",
                            self.displayed.len(),
                            total
                        ),
                    );
                    self.update_list();
                    self.status("加载完成 · 双击歌曲播放。");
                }
                Event::ScanProgress { done, total } => {
                    if total == 0 {
                        self.status(&format!("正在查找音乐文件… 已发现 {done} 首"));
                        continue;
                    }
                    let bar = self.control("progress");
                    let _ = SendMessageW(
                        bar,
                        PBM_SETRANGE32,
                        Some(WPARAM(0)),
                        Some(LPARAM(total as isize)),
                    );
                    let _ = SendMessageW(bar, PBM_SETPOS, Some(WPARAM(done)), Some(LPARAM(0)));
                    self.status(&format!("正在读取音乐信息… {done}/{total}"));
                }
                Event::Library { tracks, warnings } => {
                    self.scanning = false;
                    let _ = ShowWindow(self.control("progress"), SW_HIDE);
                    self.local = tracks;
                    if self.local_view {
                        self.show_local();
                    }
                    self.status(&if warnings.is_empty() {
                        format!("扫描完成：{} 首音乐。", self.local.len())
                    } else {
                        format!(
                            "扫描完成：{} 首；{} 个文件或目录无法读取。",
                            self.local.len(),
                            warnings.len()
                        )
                    });
                }
                Event::Ready {
                    epoch,
                    track,
                    source,
                    temp,
                    local,
                    preview,
                } => {
                    if epoch != self.play_epoch.load(Ordering::SeqCst) {
                        continue;
                    }
                    let Some(play) = self.intent.ready(epoch) else {
                        continue;
                    };
                    let result = self
                        .source_events(epoch)
                        .and_then(|_| self.player.load_source(&source, play));
                    match result {
                        Ok(()) => {
                            self.temp = temp;
                            self.now = Some(track.clone());
                            self.text(
                                "now",
                                &format!(
                                    "{} — {}   · {}{}",
                                    track.title,
                                    track.artist,
                                    if local {
                                        "本地文件"
                                    } else {
                                        "网易云在线"
                                    },
                                    if preview { " · 试听" } else { "" }
                                ),
                            );
                            if let Ok(smtc) = self.player.native.SystemMediaTransportControls() {
                                if let Ok(updater) = smtc.DisplayUpdater() {
                                    let _ = updater.SetType(MediaPlaybackType::Music);
                                    if let Ok(music) = updater.MusicProperties() {
                                        let _ = music.SetTitle(&HSTRING::from(&track.title));
                                        let _ = music.SetArtist(&HSTRING::from(&track.artist));
                                    }
                                    let _ = updater.Update();
                                }
                            }
                            self.status(if preview {
                                "当前账号仅获得试听音源。"
                            } else if local {
                                "正在播放本地文件。"
                            } else {
                                "正在播放网易云音源。"
                            });
                            self.sync_play_state();
                        }
                        Err(e) => self.status(&format!("播放失败：{e:#}")),
                    }
                }
                Event::Qr { epoch, url } => {
                    if epoch == self.login_gate.load(Ordering::SeqCst) {
                        match show_qr(self.hwnd, epoch, &url, self.scale) {
                            Ok(window) => {
                                self.qr_window = Some(window);
                                self.status("请用网易云音乐 App 扫描二维码。");
                            }
                            Err(e) => self.status(&format!("二维码显示失败：{e}")),
                        }
                    }
                }
                Event::QrStatus {
                    epoch,
                    status,
                    closed,
                } => {
                    if epoch == self.login_gate.load(Ordering::SeqCst) {
                        if let Some(window) = self.qr_window {
                            if closed {
                                self.qr_window = None;
                                let _ = DestroyWindow(window);
                            } else {
                                update_qr(window, &status);
                            }
                        }
                        self.status(&status);
                    }
                }
                Event::PlaybackError { epoch, message } => {
                    if epoch == self.play_epoch.load(Ordering::SeqCst) {
                        self.intent.pending = false;
                        self.status(&format!("播放失败：{message}"));
                        self.sync_play_state();
                    }
                }
                Event::ViewError { epoch, message } => {
                    if epoch == self.view_epoch {
                        self.search.in_flight = false;
                        self.update_list();
                        self.status(&format!("加载失败：{message}"));
                    }
                }
                Event::Error(error) => self.status(&format!("操作失败：{error}")),
            }
        }
    }
}

fn time(ms: u64) -> String {
    format!("{}:{:02}", ms / 60000, ms / 1000 % 60)
}
unsafe fn window_text(hwnd: HWND) -> String {
    let len = GetWindowTextLengthW(hwnd).max(0) as usize;
    let mut text = vec![0; len + 1];
    let copied = GetWindowTextW(hwnd, &mut text);
    String::from_utf16_lossy(&text[..copied.max(0) as usize])
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if let Some(result) = theme::control_color(message, wparam, lparam) {
        return result;
    }
    if message == WM_NCCREATE {
        let create = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut RefCell<App>;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    // Destruction can be reentrant (e.g. a command closes its own window).
    if message == WM_DESTROY {
        PostQuitMessage(0);
        return LRESULT(0);
    }
    // Common dialogs run a nested message loop. Folder selection is isolated in a
    // helper so the rich Shell UI cannot retain memory in the playback process.
    if message == WM_COMMAND && (wparam.0 & 0xffff) == ID_FOLDER as usize {
        if let Ok(app) = (*ptr).try_borrow() {
            let _ = EnableWindow(app.control("folder"), false);
            app.status("请选择音乐目录…");
            app.send(Command::PickFolder(hwnd.0 as usize));
        }
        return LRESULT(0);
    }
    if message == WM_COMMAND && (wparam.0 & 0xffff) == ID_CLEAR as usize {
        let folders = (*ptr)
            .borrow()
            .settings
            .folders
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        if App::confirm_clear(hwnd, &folders) {
            let mut app = (*ptr).borrow_mut();
            app.settings.folders.clear();
            app.save_settings();
            app.scan();
        }
        return LRESULT(0);
    }
    let Ok(mut app) = (*ptr).try_borrow_mut() else {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    };
    if app.hwnd.0.is_null() {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            let mut bounds = RECT::default();
            let _ = GetClientRect(hwnd, &mut bounds);
            app.theme.background(dc, bounds, app.scale);
            // A subtle frame surrounds the original search edit rectangle.
            let mut search = RECT::default();
            let _ = GetWindowRect(app.control("search"), &mut search);
            let mut top = POINT {
                x: search.left,
                y: search.top,
            };
            let mut bottom = POINT {
                x: search.right,
                y: search.bottom,
            };
            let _ = ScreenToClient(hwnd, &mut top);
            let _ = ScreenToClient(hwnd, &mut bottom);
            theme::rounded(
                dc,
                theme::rect(
                    top.x - app.px(1),
                    top.y - app.px(3),
                    bottom.x + app.px(1),
                    bottom.y + app.px(3),
                ),
                theme::SURFACE,
                theme::BORDER,
                app.px(8),
            );
            let _ = EndPaint(hwnd, &paint);
            LRESULT(0)
        }
        WM_DRAWITEM => {
            let item = &*(lparam.0 as *const DRAWITEMSTRUCT);
            if item.CtlType == ODT_BUTTON {
                app.theme
                    .button(item, &window_text(item.hwndItem), app.font, app.scale);
            } else if item.hwndItem == app.control("nav") {
                if item.itemID != u32::MAX {
                    let length = SendMessageW(
                        item.hwndItem,
                        LB_GETTEXTLEN,
                        Some(WPARAM(item.itemID as usize)),
                        None,
                    )
                    .0;
                    if length >= 0 {
                        let mut buffer = vec![0u16; length as usize + 1];
                        SendMessageW(
                            item.hwndItem,
                            LB_GETTEXT,
                            Some(WPARAM(item.itemID as usize)),
                            Some(LPARAM(buffer.as_mut_ptr() as isize)),
                        );
                        app.theme.nav(
                            item,
                            &String::from_utf16_lossy(&buffer[..length as usize]),
                            app.font,
                            app.scale,
                        );
                    }
                }
            } else if item.hwndItem == app.control("brand") {
                app.theme
                    .brand(item.hDC, item.rcItem, app.title_font, app.scale);
            }
            LRESULT(1)
        }
        WM_GETMINMAXINFO => {
            let info = &mut *(lparam.0 as *mut MINMAXINFO);
            info.ptMinTrackSize = POINT {
                x: app.px(880),
                y: app.px(580),
            };
            LRESULT(0)
        }
        WM_SIZE => {
            if wparam.0 == SIZE_MINIMIZED as usize {
                app.visible = false;
                let _ = ShowWindow(hwnd, SW_HIDE);
                let _ = KillTimer(Some(hwnd), 1);
            } else {
                app.layout();
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            app.scale = (wparam.0 & 0xffff) as f64 / 96.;
            app.reset_fonts();
            let rect = &*(lparam.0 as *const RECT);
            let _ = SetWindowPos(
                hwnd,
                None,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            app.layout();
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xffff) as u16;
            let notification = (wparam.0 >> 16) as u32;
            match id {
                ID_SCAN => app.scan(),
                ID_LOCAL => app.show_local(),
                ID_LOGIN => app.begin_login(),
                ID_LOGOUT => {
                    app.login_gate.store(0, Ordering::SeqCst);
                    if let Some(qr) = app.qr_window.take() {
                        let _ = DestroyWindow(qr);
                    }
                    app.send(Command::Logout);
                }
                ID_SEARCH => app.search(false),
                ID_MORE => app.search(true),
                ID_PREV => app.next(-1),
                ID_PLAY => app.toggle(),
                ID_NEXT => app.next(1),
                ID_SHUFFLE => app.toggle_shuffle(),
                ID_SHOW => app.restore(),
                ID_MEDIA_PLAY => app.set_playing(true),
                ID_MEDIA_PAUSE => app.set_playing(false),
                ID_EXIT => {
                    let _ = DestroyWindow(hwnd);
                }
                ID_NAV => {
                    if notification == LBN_SELCHANGE {
                        app.nav_selection();
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            let header = &*(lparam.0 as *const NMHDR);
            if header.hwndFrom == app.control("list") {
                match header.code {
                    NM_CUSTOMDRAW => {
                        let draw = &mut *(lparam.0 as *mut NMLVCUSTOMDRAW);
                        if draw.nmcd.dwDrawStage == CDDS_PREPAINT {
                            return LRESULT((CDRF_NOTIFYITEMDRAW | CDRF_NOTIFYPOSTPAINT) as isize);
                        }
                        if draw.nmcd.dwDrawStage == CDDS_ITEMPREPAINT {
                            draw.clrText = theme::INK;
                            draw.clrTextBk = if draw.nmcd.dwItemSpec % 2 == 0 {
                                theme::SURFACE
                            } else {
                                theme::color(0xf8fbff)
                            };
                            return LRESULT(CDRF_NOTIFYSUBITEMDRAW as isize);
                        }
                        if draw.nmcd.dwDrawStage.0 == (CDDS_ITEMPREPAINT.0 | CDDS_SUBITEM.0) {
                            draw.clrText = if draw.iSubItem == 0 {
                                theme::INK
                            } else if draw.iSubItem == 4 {
                                theme::BLUE
                            } else {
                                theme::MUTED
                            };
                            return LRESULT(CDRF_NEWFONT as isize);
                        }
                        if draw.nmcd.dwDrawStage == CDDS_POSTPAINT && app.displayed.is_empty() {
                            let mut bounds = RECT::default();
                            let _ = GetClientRect(header.hwndFrom, &mut bounds);
                            app.theme.empty(
                                draw.nmcd.hdc,
                                bounds,
                                app.font,
                                app.scale,
                                app.local_view,
                            );
                        }
                        return LRESULT(CDRF_DODEFAULT as isize);
                    }
                    LVN_GETDISPINFOW => {
                        let info = &mut *(lparam.0 as *mut NMLVDISPINFOW);
                        if info.item.mask.contains(LVIF_TEXT)
                            && !info.item.pszText.0.is_null()
                            && info.item.cchTextMax > 0
                        {
                            if let Some(track) = app.displayed.get(info.item.iItem as usize) {
                                let text = match info.item.iSubItem {
                                    0 => track.title.clone(),
                                    1 => track.artist.clone(),
                                    2 => track.album.clone(),
                                    3 => {
                                        if track.duration_ms > 0 {
                                            time(track.duration_ms)
                                        } else {
                                            "—".into()
                                        }
                                    }
                                    4 => if track.path.is_some() {
                                        if track
                                            .path
                                            .as_ref()
                                            .and_then(|p| p.extension())
                                            .is_some_and(|e| e.eq_ignore_ascii_case("ncm"))
                                        {
                                            "本地 NCM"
                                        } else {
                                            "本地"
                                        }
                                    } else {
                                        "网易云"
                                    }
                                    .into(),
                                    _ => String::new(),
                                };
                                let data = text
                                    .encode_utf16()
                                    .take(info.item.cchTextMax as usize - 1)
                                    .collect::<Vec<_>>();
                                std::ptr::copy_nonoverlapping(
                                    data.as_ptr(),
                                    info.item.pszText.0,
                                    data.len(),
                                );
                                *info.item.pszText.0.add(data.len()) = 0;
                            }
                        }
                    }
                    NM_DBLCLK => app.play_selected(),
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_HSCROLL => {
            let control = HWND(lparam.0 as *mut _);
            let code = (wparam.0 & 0xffff) as u32;
            if control == app.control("volume") {
                let volume = SendMessageW(control, TRACKBAR_GETPOS, None, None).0 as u32;
                app.settings.volume = volume.min(100);
                let _ = app.player.set_volume(volume);
                if code == TB_ENDTRACK {
                    app.save_settings();
                }
            } else if control == app.control("seek")
                && [
                    TB_ENDTRACK,
                    TB_THUMBPOSITION,
                    TB_LINEUP,
                    TB_LINEDOWN,
                    TB_PAGEUP,
                    TB_PAGEDOWN,
                ]
                .contains(&code)
            {
                let value = SendMessageW(control, TRACKBAR_GETPOS, None, None).0.max(0) as u64;
                let _ = app
                    .player
                    .seek_ms(app.player.duration_ms().saturating_mul(value) / 1000);
                app.progress();
            }
            LRESULT(0)
        }
        APP_EVENT => {
            app.poll_events();
            LRESULT(0)
        }
        MEDIA_EVENT => {
            if wparam.0 == 0 || wparam.0 as u64 == app.play_epoch.load(Ordering::SeqCst) {
                app.sync_play_state();
            }
            LRESULT(0)
        }
        MEDIA_ENDED => {
            if app.intent.ended(wparam.0 as u64) {
                app.next(1);
            }
            LRESULT(0)
        }
        MEDIA_ERROR => {
            if lparam.0 as u64 == app.play_epoch.load(Ordering::SeqCst) {
                app.status(&format!(
                    "系统播放器未能打开音频（0x{:08X}），请检查文件格式或账号权益。",
                    wparam.0 as u32
                ));
                app.sync_play_state();
            }
            LRESULT(0)
        }
        WM_TIMER => {
            app.progress();
            LRESULT(0)
        }
        QR_CLOSED => {
            let epoch = wparam.0 as u64;
            app.send(Command::CancelLogin(epoch));
            if app.login_epoch == epoch {
                app.qr_window = None;
                app.login_gate.store(0, Ordering::SeqCst);
            }
            LRESULT(0)
        }
        TRAY_EVENT => {
            match lparam.0 as u32 {
                WM_LBUTTONDBLCLK => app.restore(),
                WM_RBUTTONUP => app.tray_menu(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DROPFILES => {
            let drop = HDROP(wparam.0 as *mut _);
            let count = DragQueryFileW(drop, u32::MAX, None);
            for i in 0..count {
                let mut path = vec![0; DragQueryFileW(drop, i, None) as usize + 1];
                DragQueryFileW(drop, i, Some(&mut path));
                let path = PathBuf::from(String::from_utf16_lossy(&path[..path.len() - 1]));
                if path.is_dir() {
                    if !app.settings.folders.contains(&path) {
                        app.settings.folders.push(path);
                        app.save_settings();
                        app.scan();
                    }
                } else if let Err(e) = app.open_file(path) {
                    app.status(&e.to_string());
                }
            }
            DragFinish(drop);
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORLISTBOX | WM_CTLCOLOREDIT => {
            let dc = HDC(wparam.0 as *mut _);
            let control = HWND(lparam.0 as *mut _);
            let sidebar = control == app.control("nav") || control == app.control("nav_title");
            let surface = message == WM_CTLCOLOREDIT
                || ["now", "time", "volume_label", "volume", "seek"]
                    .iter()
                    .any(|name| control == app.control(name));
            let (background, brush) = if sidebar {
                (theme::SIDEBAR, app.theme.sidebar)
            } else if surface {
                (theme::SURFACE, app.theme.surface)
            } else {
                (theme::BACKGROUND, app.theme.background)
            };
            SetBkColor(dc, background);
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(
                dc,
                if control == app.control("title")
                    || control == app.control("now")
                    || message == WM_CTLCOLOREDIT
                {
                    theme::INK
                } else {
                    theme::MUTED
                },
            );
            LRESULT(brush.0 as isize)
        }
        WM_CLOSE => {
            app.visible = false;
            let _ = ShowWindow(hwnd, SW_HIDE);
            let _ = KillTimer(Some(hwnd), 1);
            LRESULT(0)
        }
        WM_DESTROY => {
            app.tray(NIM_DELETE);
            app.save_settings();
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

struct QrState {
    owner: HWND,
    epoch: u64,
    code: qrcode::QrCode,
    status: String,
    scale: f64,
}
unsafe fn show_qr(owner: HWND, epoch: u64, url: &str, scale: f64) -> anyhow::Result<HWND> {
    let state = Box::new(QrState {
        owner,
        epoch,
        code: qrcode::QrCode::new(url)?,
        status: "请用网易云音乐 App 扫码登录。".into(),
        scale,
    });
    let ptr = Box::into_raw(state);
    match CreateWindowExW(
        WS_EX_DLGMODALFRAME,
        w!("CloudLiteQR"),
        w!("扫码登录网易云"),
        WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_VISIBLE,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        (400. * scale) as i32,
        (450. * scale) as i32,
        Some(owner),
        None,
        Some(GetModuleHandleW(None)?.into()),
        Some(ptr.cast()),
    ) {
        Ok(window) => Ok(window),
        Err(e) => {
            drop(Box::from_raw(ptr));
            Err(e.into())
        }
    }
}
unsafe fn update_qr(window: HWND, status: &str) {
    let ptr = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut QrState;
    if !ptr.is_null() {
        (*ptr).status = status.into();
        let _ = InvalidateRect(Some(window), None, true);
    }
}
unsafe extern "system" fn qr_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut QrState;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    match message {
        WM_PAINT => {
            let state = &*ptr;
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            let mut rect = RECT::default();
            let _ = GetClientRect(hwnd, &mut rect);
            FillRect(dc, &rect, GetSysColorBrush(COLOR_WINDOW));
            let side = state.code.width();
            let module = ((rect.right - 48) / (side as i32 + 8)).max(1);
            let size = (side as i32 + 8) * module;
            let x = (rect.right - size) / 2 + 4 * module;
            let y = (24. * state.scale) as i32 + 4 * module;
            for row in 0..side {
                for col in 0..side {
                    if state.code[(col, row)] == qrcode::Color::Dark {
                        let cell = RECT {
                            left: x + col as i32 * module,
                            top: y + row as i32 * module,
                            right: x + (col as i32 + 1) * module,
                            bottom: y + (row as i32 + 1) * module,
                        };
                        FillRect(dc, &cell, HBRUSH(GetStockObject(BLACK_BRUSH).0));
                    }
                }
            }
            let mut text = state.status.encode_utf16().collect::<Vec<_>>();
            let mut text_rect = RECT {
                left: 16,
                top: y + side as i32 * module + (28. * state.scale) as i32,
                right: rect.right - 16,
                bottom: rect.bottom - 10,
            };
            SetBkMode(dc, TRANSPARENT);
            DrawTextW(dc, &mut text, &mut text_rect, DT_CENTER | DT_WORDBREAK);
            let _ = EndPaint(hwnd, &paint);
            LRESULT(0)
        }
        WM_DESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let state = Box::from_raw(ptr);
            let _ = PostMessageW(
                Some(state.owner),
                QR_CLOSED,
                WPARAM(state.epoch as usize),
                LPARAM(0),
            );
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

impl Drop for App {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.font.into());
            let _ = DeleteObject(self.title_font.into());
        }
    }
}
