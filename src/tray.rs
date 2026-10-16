//! Native tray integration. Created, used and dropped on the GPUI window thread.
use anyhow::{Result, bail};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use windows_sys::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{Shell::*, WindowsAndMessaging::*},
    },
    core::w,
};

const CALLBACK: u32 = WM_APP + 120;
const HIDE: u32 = WM_APP + 121;
const ICON_ID: u32 = 1;
const SUBCLASS_ID: usize = 0x58524159;
const SHOW: usize = 1;
const EXIT: usize = 2;

pub struct Tray {
    // Box keeps the callback's address stable until the subclass is removed.
    state: Box<TrayState>,
}

struct TrayState {
    data: NOTIFYICONDATAW,
    taskbar_created: u32,
    exit_requested: Arc<AtomicBool>,
}

impl Tray {
    pub fn new(window: &gpui_kit::Window, exit_requested: Arc<AtomicBool>) -> Result<Self> {
        let RawWindowHandle::Win32(handle) = HasWindowHandle::window_handle(window)
            .map_err(|error| anyhow::anyhow!("无法获取窗口句柄：{error}"))?
            .as_raw()
        else {
            bail!("无法获取 Windows 窗口句柄");
        };
        let hwnd = handle.hwnd.get() as HWND;
        Self::attach(hwnd, exit_requested)
    }

    fn attach(hwnd: HWND, exit_requested: Arc<AtomicBool>) -> Result<Self> {
        // LR_SHARED icons belong to the executable module and remain valid for the
        // window's lifetime, even if tray registration fails or Tray is dropped.
        unsafe {
            let module = GetModuleHandleW(std::ptr::null());
            let load = |width, height| {
                LoadImageW(module, 1usize as _, IMAGE_ICON, width, height, LR_SHARED)
            };
            let small = load(GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
            let large = load(GetSystemMetrics(SM_CXICON), GetSystemMetrics(SM_CYICON));
            if small.is_null() || large.is_null() {
                bail!("无法加载应用图标：{}", std::io::Error::last_os_error());
            }
            SendMessageW(hwnd, WM_SETICON, ICON_SMALL as usize, small as isize);
            SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, large as isize);
            let mut data = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: ICON_ID,
                uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
                uCallbackMessage: CALLBACK,
                hIcon: small,
                ..Default::default()
            };
            for (dst, src) in data
                .szTip
                .iter_mut()
                .zip("XrayDeck — 点击打开，右键退出".encode_utf16())
            {
                *dst = src;
            }
            let tray = Self {
                state: Box::new(TrayState {
                    data,
                    taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
                    exit_requested,
                }),
            };
            if SetWindowSubclass(
                hwnd,
                Some(callback),
                SUBCLASS_ID,
                &*tray.state as *const TrayState as usize,
            ) == 0
            {
                bail!("无法注册托盘窗口回调");
            }
            if Shell_NotifyIconW(NIM_ADD, &tray.state.data) == 0 {
                bail!("无法添加托盘图标");
            }
            Ok(tray)
        }
    }

    pub fn hide(&self) {
        // Defer visibility changes until GPUI has returned from its close callback.
        // ShowWindow dispatches synchronous messages and could otherwise re-enter App.
        unsafe {
            PostMessageW(self.state.data.hWnd, HIDE, 0, 0);
        }
    }

    pub fn remove_icon(&self) {
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &self.state.data);
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            RemoveWindowSubclass(self.state.data.hWnd, Some(callback), SUBCLASS_ID);
            Shell_NotifyIconW(NIM_DELETE, &self.state.data);
        }
    }
}

pub(crate) unsafe fn restore(hwnd: HWND) {
    unsafe {
        ShowWindow(
            hwnd,
            if IsIconic(hwnd) != 0 {
                SW_RESTORE
            } else {
                SW_SHOW
            },
        );
        SetForegroundWindow(hwnd);
    }
}

unsafe extern "system" fn callback(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    reference: usize,
) -> LRESULT {
    // SAFETY: the boxed state outlives this subclass; callback runs on its owner thread.
    unsafe {
        let state = &*(reference as *const TrayState);
        if message == HIDE {
            // Never hide an inaccessible window if Explorer cannot recreate its icon.
            if Shell_NotifyIconW(NIM_MODIFY, &state.data) != 0
                || Shell_NotifyIconW(NIM_ADD, &state.data) != 0
            {
                ShowWindow(hwnd, SW_HIDE);
            }
            return 0;
        }
        if message == state.taskbar_created && state.taskbar_created != 0 {
            if Shell_NotifyIconW(NIM_ADD, &state.data) == 0 {
                restore(hwnd);
            }
            return 0;
        }
        if message == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(callback), SUBCLASS_ID);
            Shell_NotifyIconW(NIM_DELETE, &state.data);
        }
        if message == CALLBACK && wparam == ICON_ID as usize {
            match lparam as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => restore(hwnd),
                WM_RBUTTONUP | WM_CONTEXTMENU => {
                    let menu = CreatePopupMenu();
                    if !menu.is_null() {
                        AppendMenuW(menu, MF_STRING, SHOW, w!("打开 XrayDeck"));
                        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
                        AppendMenuW(menu, MF_STRING, EXIT, w!("退出"));
                        let mut point = POINT::default();
                        GetCursorPos(&mut point);
                        SetForegroundWindow(hwnd);
                        let command = TrackPopupMenu(
                            menu,
                            TPM_RETURNCMD | TPM_RIGHTBUTTON,
                            point.x,
                            point.y,
                            0,
                            hwnd,
                            std::ptr::null(),
                        );
                        DestroyMenu(menu);
                        PostMessageW(hwnd, WM_NULL, 0, 0);
                        match command as usize {
                            SHOW => restore(hwnd),
                            EXIT => {
                                state.exit_requested.store(true, Ordering::SeqCst);
                                // Show progress if an upgrade must finish before exit.
                                restore(hwnd);
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            return 0;
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an interactive Windows desktop with Explorer"]
    fn native_tray_hides_restores_and_recovers_after_icon_removal() {
        // An isolated native test window exercises the real subclass and Shell API
        // without loading user settings or contacting an Xray service.
        unsafe {
            let hwnd = CreateWindowExW(
                0,
                w!("STATIC"),
                w!("XrayDeck tray test"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                160,
                100,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            assert!(!hwnd.is_null());
            let tray = Tray::attach(hwnd, Arc::new(AtomicBool::new(false))).unwrap();
            // The same embedded icon must also be assigned to the title bar and taskbar.
            assert_eq!(
                SendMessageW(hwnd, WM_GETICON, ICON_SMALL as usize, 0),
                tray.state.data.hIcon as isize
            );
            assert_ne!(SendMessageW(hwnd, WM_GETICON, ICON_BIG as usize, 0), 0);
            SendMessageW(hwnd, CALLBACK, ICON_ID as usize, WM_LBUTTONUP as isize);
            assert_ne!(IsWindowVisible(hwnd), 0);
            tray.hide();
            let mut message = MSG::default();
            while PeekMessageW(&mut message, hwnd, HIDE, HIDE, PM_REMOVE) != 0 {
                DispatchMessageW(&message);
            }
            assert_eq!(IsWindowVisible(hwnd), 0);
            tray.remove_icon();
            SendMessageW(hwnd, tray.state.taskbar_created, 0, 0);
            SendMessageW(hwnd, CALLBACK, ICON_ID as usize, WM_LBUTTONUP as isize);
            assert_ne!(IsWindowVisible(hwnd), 0);
            drop(tray);
            // Removing the subclass must leave a usable native window.
            assert_ne!(IsWindow(hwnd), 0);
            DestroyWindow(hwnd);
        }
    }
}
