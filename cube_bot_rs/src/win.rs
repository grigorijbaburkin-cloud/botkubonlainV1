// Тонкая обёртка над Win32: консоль, скриншот экрана, звуки, снап таймера.
#![allow(dead_code)]
use std::ffi::c_void;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{
    SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
};
use windows::Win32::System::Threading::Sleep;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Прикрепляемся к родительской консоли (запуск из cmd) или создаём свою.
pub fn attach_or_alloc_console() {
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            let _ = AllocConsole();
            let h = GetStdHandle(STD_OUTPUT_HANDLE);
            if !h.is_invalid() {
                // Буфер побольше, чтобы история логов не пропадала.
                let _ = SetConsoleWindowInfo(h, true, &RECT { left: 4, top: 0, right: 110, bottom: 40 });
                let _ = SetConsoleCursorInfo(
                    h,
                    &CONSOLE_CURSOR_INFO { dwSize: 20, bVisible: true.into() },
                );
            }
        }
    }
}

/// Прячем курсор консоли на снимках экрана и переносим окно в правый верхний угол,
/// чтобы бот не "читал" собственный вывод вместо игры.
pub fn hide_console_from_captures() {
    unsafe {
        let title = to_wide("КУБ БОТ");
        if let Ok(hwnd) = FindWindowW(PCWSTR::null(), PCWSTR(title.as_ptr())) {
            let sw = GetSystemMetrics(SM_CXSCREEN);
            let _ = SetWindowPos(
                hwnd,
                HWND::default(),
                sw - 640,
                0,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

pub fn console_title(title: &str) {
    let w = to_wide(title);
    unsafe {
        let _ = SetConsoleTitleW(PCWSTR(w.as_ptr()));
    }
}

pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Экран как BGRA-буфер (быстрый снимок через BitBlt, ~1 мс).
pub struct Screen {
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u8>, // B,G,R,X
}

impl Screen {
    #[inline]
    pub fn rgb(&self, x: i32, y: i32) -> (u8, u8, u8) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return (0, 0, 0);
        }
        let i = ((y as usize) * self.width as usize + x as usize) * 4;
        (self.pixels[i], self.pixels[i + 1], self.pixels[i + 2])
    }
}

pub fn grab_screen() -> Option<Screen> {
    unsafe {
        let hdesktop = GetDesktopWindow();
        let w = GetSystemMetrics(SM_CXSCREEN);
        let h = GetSystemMetrics(SM_CYSCREEN);
        if w <= 0 || h <= 0 {
            return None;
        }
        let hdc = GetDC(HWND::default());
        let mem = CreateCompatibleDC(hdc);
        let bmp = CreateCompatibleBitmap(hdc, w, h);
        let old = SelectObject(mem, bmp);
        let ok = BitBlt(
            mem,
            0,
            0,
            w,
            h,
            hdc,
            0,
            0,
            SRCCOPY | CAPTUREBLT,
        );
        let mut pixels = vec![0u8; (w as usize) * (h as usize) * 4];
        let mut res = None;
        if ok.is_ok() {
            #[repr(C)]
            #[derive(Clone, Copy)]
            struct Bi {
                bmi_header: BITMAPINFOHEADER,
                bmi_colors: [u32; 16],
            }
            let mut bi = Bi {
                bmi_header: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0 as u32,
                    ..Default::default()
                },
                bmi_colors: [0; 16],
            };
            let got = GetDIBits(
                mem,
                bmp,
                0,
                h as u32,
                Some(pixels.as_mut_ptr() as *mut c_void),
                &mut bi.bmi_header as *mut _ as *mut BITMAPINFO,
                DIB_RGB_COLORS,
            );
            if got > 0 {
                res = Some(Screen { width: w, height: h, pixels });
            }
        }
        let _ = hdesktop;
        SelectObject(mem, old);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(mem);
        ReleaseDC(HWND::default(), hdc);
        res
    }
}

/// Снимок прямоугольника региона (x,y,w,h) экрана как BGRA.
pub fn grab_region(x: i32, y: i32, w: i32, h: i32) -> Option<(i32, i32, Vec<u8>)> {
    let full = grab_screen()?;
    let x = x.max(0).min(full.width - 1);
    let y = y.max(0).min(full.height - 1);
    let w = w.min(full.width - x);
    let h = h.min(full.height - y);
    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    for row in 0..h as usize {
        let src = ((y as usize + row) * full.width as usize + x as usize) * 4;
        let dst = row * w as usize * 4;
        let len = w as usize * 4;
        if src + len <= full.pixels.len() {
            out[dst..dst + len].copy_from_slice(&full.pixels[src..src + len]);
        }
    }
    Some((w, h, out))
}

pub fn save_bmp(path: &str, w: i32, h: i32, bgra: &[u8]) {
    use std::io::Write;
    let row = (w as usize * 3 + 3) / 4 * 4;
    let size = 54 + row * h as usize;
    let mut f = match std::fs::File::create(path) {
        Ok(f) => f,
        Err(_) => return,
    };
    let mut hdr = vec![0u8; 54];
    hdr[0..2].copy_from_slice(b"BM");
    hdr[2..6].copy_from_slice(&(size as u32).to_le_bytes());
    hdr[10..14].copy_from_slice(&54u32.to_le_bytes());
    hdr[14..18].copy_from_slice(&40u32.to_le_bytes());
    hdr[18..22].copy_from_slice(&(w as u32).to_le_bytes());
    hdr[22..26].copy_from_slice(&(h as u32).to_le_bytes());
    hdr[26..28].copy_from_slice(&1u16.to_le_bytes());
    hdr[28..30].copy_from_slice(&24u16.to_le_bytes());
    hdr[30..34].copy_from_slice(&0u32.to_le_bytes());
    let _ = f.write_all(&hdr);
    let mut line = vec![0u8; row];
    for y in (0..h as usize).rev() {
        for x in 0..w as usize {
            let i = (y * w as usize + x) * 4;
            line[x * 3] = bgra[i];
            line[x * 3 + 1] = bgra[i + 1];
            line[x * 3 + 2] = bgra[i + 2];
        }
        let _ = f.write_all(&line);
    }
}

/// Бип для сигналов старт/стоп. high=true - высокий (старт), false - низкий (стоп).
pub fn beep(high: bool) {
    let (f, d) = if high { (1400u32, 180u32) } else { (420u32, 320u32) };
    unsafe {
        let _ = Beep(f, d);
    }
}

pub fn sleep_ms(ms: u32) {
    unsafe { Sleep(ms) }
}

/// Не даём ПК уснуть, пока бот работает (экран нужен для чтения координат).
pub fn keep_awake(on: bool) {
    unsafe {
        let _ = SetThreadExecutionState(if on {
            ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED
        } else {
            ES_CONTINUOUS
        });
    }
}

pub fn open_in_browser(path: &str) {
    let p = to_wide(path);
    let op = to_wide("open");
    unsafe {
        ShellExecuteW(
            HWND::default(),
            PCWSTR(op.as_ptr()),
            PCWSTR(p.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

pub fn cursor_pos() -> (i32, i32) {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

extern "system" {
    fn Beep(dwFreq: u32, dwDuration: u32) -> i32;
}

/// Сохраняет текущий снимок области поиска в region.bmp (для диагностики).
pub fn save_region_bmp(path: &str) {
    // вызывается редко; просто делаем свежий снимок региона целиком снизу-слева
    if let Some(s) = grab_screen() {
        let w = (s.width / 3).clamp(320, 720);
        let h = 60i32;
        let y = s.height - h - 4;
        if let Some((rw, rh, px)) = grab_region(0, y, w, h) {
            save_bmp(path, rw, rh, &px);
        }
    }
}
