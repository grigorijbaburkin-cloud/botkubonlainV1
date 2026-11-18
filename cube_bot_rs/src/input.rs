// Ввод: клавиши (W/A/S/D/Space), мышь (поворот камеры), глобальные хуки hotkey.
#![allow(dead_code)]
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use windows::Win32::Foundation::LPARAM;
use windows::Win32::UI::Input::KeyboardAndMouse::*;

pub const VK_W: u32 = 0x57;
pub const VK_A: u32 = 0x41;
pub const VK_S: u32 = 0x53;
pub const VK_D: u32 = 0x44;
pub const VK_SPACE: u32 = 0x20;
pub const VK_LBUTTON: u32 = 0x01;

// Глобальные флаги, которые выставляет хук hotkeys (читаются ботом).
pub static HOT_F8: AtomicBool = AtomicBool::new(false); // старт/стоп
pub static HOT_F9: AtomicBool = AtomicBool::new(false); // аварийный стоп
pub static HOT_F10: AtomicBool = AtomicBool::new(false); // калибровка камеры
pub static HOT_F4: AtomicBool = AtomicBool::new(false); // РОСТ <-> К ТОЧКЕ
pub static HOT_F2: AtomicBool = AtomicBool::new(false); // тест клавиш
pub static HOT_SF2: AtomicBool = AtomicBool::new(false); // проверка чтения
pub static HOT_INSERT: AtomicBool = AtomicBool::new(false); // ручная привязка области
pub static HOT_DELETE: AtomicBool = AtomicBool::new(false); // запомнить цвет воды
pub static MOUSE_DX: AtomicU32 = AtomicU32::new(0); // сумма dx мыши с прошлого опроса (i32 bits)

/// Нажатие / отпускание виртуальной клавиши. Отправляем и extended-версию,
/// чтобы игра гарантированно видела ввод при захвате курсора.
pub fn key_down(vk: u32) {
    unsafe {
        let _ = keybd_event(vk as u8, scan(vk), KEYEVENTF_EXTENDEDKEY, LPARAM(0));
    }
}
pub fn key_up(vk: u32) {
    unsafe {
        let _ = keybd_event(
            vk as u8,
            scan(vk),
            KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP,
            LPARAM(0),
        );
    }
}
fn scan(vk: u32) -> u8 {
    unsafe { MapVirtualKeyW(vk, MAPVK_VK_TO_VSC) as u8 }
}

pub fn is_down(vk: u32) -> bool {
    unsafe { GetAsyncKeyState(vk as i32) & 0x8000u16 as i16 != 0 }
}

/// Поворот камеры на dx пикселей мыши (dx>0 - вправо).
pub fn mouse_move(dx: i32) {
    if dx == 0 {
        return;
    }
    unsafe {
        let _ = SendInput(
            &[INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx,
                        dy: 0,
                        mouseData: 0,
                        dwFlags: MOUSEEVENTF_MOVE,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            }],
            std::mem::size_of::<INPUT>() as i32,
        );
    }
}

pub fn click_left() {
    unsafe {
        let _ = SendInput(
            &[
                INPUT {
                    r#type: INPUT_MOUSE,
                    Anonymous: INPUT_0 {
                        mi: MOUSEINPUT {
                            dwFlags: MOUSEEVENTF_LEFTDOWN,
                            ..Default::default()
                        },
                    },
                },
                INPUT {
                    r#type: INPUT_MOUSE,
                    Anonymous: INPUT_0 {
                        mi: MOUSEINPUT {
                            dwFlags: MOUSEEVENTF_LEFTUP,
                            ..Default::default()
                        },
                    },
                },
            ],
            std::mem::size_of::<INPUT>() as i32,
        );
    }
}

/// Все клавиши вверх - безопасно при остановке.
pub fn release_all() {
    for vk in [VK_W, VK_A, VK_S, VK_D, VK_SPACE] {
        if is_down(vk) {
            key_up(vk);
        }
    }
}

// ---------- Глобальные хуки клавиатуры и мыши для hotkeys ----------

use std::sync::atomic::AtomicI32;
use windows::Win32::Foundation::{LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, PeekMessageW, SetWindowsHookExW, TranslateMessage,
    HHOOK, KBDLLHOOKSTRUCT, MSG, MOUSEHOOKSTRUCTEX, PM_REMOVE, WH_MOUSE_LL, WM_LBUTTONDOWN,
    WM_KEYDOWN, WM_MOUSEMOVE, WM_SYSKEYDOWN,
};

static mut KB_HOOK: usize = 0;
static mut MS_HOOK: usize = 0;

unsafe extern "system" fn kbd_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let msg = wparam.0 as u32;
        if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
            let kb: &KBDLLHOOKSTRUCT = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            let shift = is_down(0x10 /*SHIFT*/);
            match kb.vkCode {
                0x71 /*F2*/ => {
                    if shift {
                        HOT_SF2.store(true, Ordering::Relaxed);
                    } else {
                        HOT_F2.store(true, Ordering::Relaxed);
                    }
                }
                0x73 /*F4*/ => HOT_F4.store(true, Ordering::Relaxed),
                0x77 /*F8*/ => HOT_F8.store(true, Ordering::Relaxed),
                0x78 /*F9*/ => HOT_F9.store(true, Ordering::Relaxed),
                0x79 /*F10*/ => HOT_F10.store(true, Ordering::Relaxed),
                0x2D /*Insert*/ => HOT_INSERT.store(true, Ordering::Relaxed),
                0x2E /*Delete*/ => HOT_DELETE.store(true, Ordering::Relaxed),
                _ => {}
            }
        }
    }
    CallNextHookEx(HHOOK::default(), code, wparam, lparam)
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let msg = wparam.0 as u32;
        if msg == WM_MOUSEMOVE {
            let mi: &MOUSEHOOKSTRUCTEX = &*(lparam.0 as *const MOUSEHOOKSTRUCTEX);
            let dx = (mi.mouseData >> 32) as i32;
            // накапливаем дельты (переполнение i32 практически недостижимо за тик)
            MOUSE_DX.fetch_add(dx as u32, Ordering::Relaxed);
        }
    }
    CallNextHookEx(HHOOK::default(), code, wparam, lparam)
}

/// Устанавливает хуки; вызывать из потока с циклом сообщений (главный поток).
pub fn install_hotkey_hook() {
    unsafe {
        let hmod = GetModuleHandleW(None).unwrap_or_default();
        if let Ok(hk) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(kbd_proc), Some(hmod), 0) {
            KB_HOOK = hk.0 as usize;
        }
        if let Ok(hk) = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(hmod), 0) {
            MS_HOOK = hk.0 as usize;
        }
    }
}

/// Вызывать каждый тик: обрабатывает сообщения очереди (нужно для LL-хука).
pub fn pump_messages() {
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

use windows::Win32::Foundation::HWND;

/// Забрать и сбросить флаг hotkey.
pub fn take(f: &AtomicBool) -> bool {
    f.swap(false, Ordering::Relaxed)
}

/// Сумма движения мыши с прошлого вызова (для самокалибровки по фактическим данным).
pub fn take_mouse_dx() -> i32 {
    MOUSE_DX.swap(0, Ordering::Relaxed) as i32
}
