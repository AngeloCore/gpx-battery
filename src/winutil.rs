use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_APP};

/// Thread message that tells the main loop to drain the app-event channel.
pub const WM_APP_WAKE: u32 = WM_APP + 1;
/// Thread message posted by the hidden window when Windows reports a device change.
pub const WM_APP_DEVCHANGE: u32 = WM_APP + 2;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Wakes the main thread's message loop from any thread.
#[derive(Clone, Copy)]
pub struct MainWaker {
    thread_id: u32,
}

impl MainWaker {
    pub fn for_current_thread() -> Self {
        Self {
            thread_id: unsafe { GetCurrentThreadId() },
        }
    }

    pub fn wake(&self) {
        unsafe {
            PostThreadMessageW(self.thread_id, WM_APP_WAKE, 0, 0);
        }
    }
}
