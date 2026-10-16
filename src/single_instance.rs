//! A session-local desktop instance lock and a queued activation event.
use anyhow::{Context, Result, bail};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::ptr::null_mut;
use windows_sys::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT,
            WAIT_OBJECT_0, WPARAM,
        },
        System::Threading::{CreateEventW, CreateMutexW, SetEvent, WaitForSingleObject},
        UI::{
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
    core::w,
};

const WINDOW_MARKER: *const u16 = w!("XrayDeck.Desktop.Instance.1");
const ACTIVATE: u32 = WM_APP + 122;
const SUBCLASS_ID: usize = 0x5844494e;

struct Handle(HANDLE);

impl Handle {
    fn checked(raw: HANDLE) -> Result<Self> {
        if raw.is_null() {
            Err(std::io::Error::last_os_error().into())
        } else {
            Ok(Self(raw))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: this wrapper owns the handle and closes it exactly once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

pub struct Instance {
    _mutex: Handle,
    activation: Handle,
    window: HWND,
}

impl Instance {
    pub fn start() -> Result<Option<Self>> {
        Self::start_named("Local\\XrayDeck.Desktop.Instance.1", true)
    }

    fn start_named(name: &str, activate_window: bool) -> Result<Option<Self>> {
        let event_name: Vec<u16> = format!("{name}.Activate\0").encode_utf16().collect();
        let mutex_name: Vec<u16> = format!("{name}.Mutex\0").encode_utf16().collect();
        // Create/open the auto-reset event first: a second launch during startup can
        // queue activation even before the first process has created its window.
        unsafe {
            let activation = Handle::checked(CreateEventW(null_mut(), 0, 0, event_name.as_ptr()))
                .context("无法创建单例唤起事件")?;
            let raw = CreateMutexW(null_mut(), 0, mutex_name.as_ptr());
            let already_running = GetLastError() == ERROR_ALREADY_EXISTS;
            let mutex = Handle::checked(raw).context("无法创建单例锁")?;
            if already_running {
                if activate_window {
                    let mut hwnd: HWND = null_mut();
                    EnumWindows(Some(find_window), &mut hwnd as *mut HWND as LPARAM);
                    if !hwnd.is_null() {
                        let mut pid = 0;
                        GetWindowThreadProcessId(hwnd, &mut pid);
                        // Hand the user-initiated launch's foreground permission to
                        // the existing process before it handles the activation event.
                        AllowSetForegroundWindow(pid);
                    }
                }
                if SetEvent(activation.0) == 0 {
                    return Err(std::io::Error::last_os_error()).context("无法唤起已有实例");
                }
                return Ok(None);
            }
            Ok(Some(Self {
                _mutex: mutex,
                activation,
                window: null_mut(),
            }))
        }
    }

    pub fn attach(&mut self, window: &gpui_kit::Window) -> Result<()> {
        let RawWindowHandle::Win32(handle) = HasWindowHandle::window_handle(window)
            .map_err(|error| anyhow::anyhow!("无法获取窗口句柄：{error}"))?
            .as_raw()
        else {
            bail!("无法获取 Windows 窗口句柄");
        };
        self.attach_native(handle.hwnd.get() as HWND)
    }

    fn attach_native(&mut self, hwnd: HWND) -> Result<()> {
        // The subclass has no state pointer and is independent of tray availability.
        unsafe {
            if SetWindowSubclass(hwnd, Some(callback), SUBCLASS_ID, 0) == 0 {
                bail!("无法注册单例唤起回调");
            }
            self.window = hwnd;
            if SetPropW(hwnd, WINDOW_MARKER, std::ptr::dangling_mut()) == 0 {
                return Err(std::io::Error::last_os_error()).context("无法登记单例窗口");
            }
        }
        Ok(())
    }

    pub fn poll_activation(&self) {
        unsafe {
            if !self.window.is_null() && WaitForSingleObject(self.activation.0, 0) == WAIT_OBJECT_0
            {
                // Native visibility changes must happen after GPUI releases App.
                PostMessageW(self.window, ACTIVATE, 0, 0);
            }
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        if !self.window.is_null() {
            unsafe {
                RemovePropW(self.window, WINDOW_MARKER);
                RemoveWindowSubclass(self.window, Some(callback), SUBCLASS_ID);
            }
        }
        // Dropping the last mutex handle releases the lock, including on process exit.
    }
}

unsafe extern "system" fn find_window(hwnd: HWND, result: LPARAM) -> i32 {
    unsafe {
        if !GetPropW(hwnd, WINDOW_MARKER).is_null() {
            *(result as *mut HWND) = hwnd;
            return 0;
        }
    }
    1
}

unsafe extern "system" fn callback(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    _: usize,
) -> LRESULT {
    unsafe {
        if message == ACTIVATE {
            crate::tray::restore(hwnd);
            return 0;
        }
        if message == WM_NCDESTROY {
            RemovePropW(hwnd, WINDOW_MARKER);
            RemoveWindowSubclass(hwnd, Some(callback), SUBCLASS_ID);
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn name() -> String {
        static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        format!(
            "Local\\XrayDeck.Test.{}.{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn duplicate_launch_queues_activation_before_window_creation() {
        const CHILD_NAME: &str = "XRAY_DECK_SINGLE_INSTANCE_TEST_NAME";
        if let Ok(name) = std::env::var(CHILD_NAME) {
            assert!(Instance::start_named(&name, false).unwrap().is_none());
            return;
        }
        let name = name();
        let first = Instance::start_named(&name, false).unwrap().unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "single_instance::tests::duplicate_launch_queues_activation_before_window_creation",
            ])
            .env(CHILD_NAME, &name)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{}{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        unsafe {
            assert_eq!(WaitForSingleObject(first.activation.0, 0), WAIT_OBJECT_0);
            assert_ne!(WaitForSingleObject(first.activation.0, 0), WAIT_OBJECT_0);
        }
    }

    #[test]
    fn lock_is_released_when_instance_exits() {
        let name = name();
        let first = Instance::start_named(&name, false).unwrap().unwrap();
        drop(first);
        assert!(Instance::start_named(&name, false).unwrap().is_some());
    }

    #[test]
    fn activation_restores_a_hidden_or_minimized_window_without_a_tray() {
        unsafe {
            let mut first = Instance::start_named(&name(), false).unwrap().unwrap();
            let hwnd = CreateWindowExW(
                0,
                w!("STATIC"),
                w!("XrayDeck activation test"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                160,
                100,
                null_mut(),
                null_mut(),
                null_mut(),
                std::ptr::null(),
            );
            assert!(!hwnd.is_null());
            first.attach_native(hwnd).unwrap();
            for state in [SW_HIDE, SW_MINIMIZE] {
                ShowWindow(hwnd, state);
                SetEvent(first.activation.0);
                first.poll_activation();
                let mut message = MSG::default();
                while PeekMessageW(&mut message, hwnd, ACTIVATE, ACTIVATE, PM_REMOVE) != 0 {
                    DispatchMessageW(&message);
                }
                assert_ne!(IsWindowVisible(hwnd), 0);
                assert_eq!(IsIconic(hwnd), 0);
            }
            drop(first);
            assert!(GetPropW(hwnd, WINDOW_MARKER).is_null());
            DestroyWindow(hwnd);
        }
    }
}
