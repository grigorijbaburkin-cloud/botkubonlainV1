//! Куб бот (Rust) для игры "Куб Онлайн - Выживание с друзьями" (Яндекс Игры). Только Windows.
//!
//! Координаты читаются прямо из пикселей (без Tesseract): захват экрана через GDI + сравнение
//! символов с эталонами. Одно чтение занимает около миллисекунды.
//!
//! Хоткеи: F8 старт/стоп, F9 СТОП, F4 режим, F10 калибровка, F2 тест клавиш, Shift+F2 проверка чтения.

#![allow(non_snake_case, non_camel_case_types, dead_code)]

mod ai {
//! Помощник на ИИ (Hugging Face Inference Providers, совместимый с OpenAI chat/completions).
//! Бот делает снимок игры и просит vision-модель подсказать, куда повернуть, чтобы обойти преграду.
//! Запрос отправляется через curl.exe (он есть в Windows 10/11), внешних библиотек не нужно.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Sender;

pub const PROMPT: &str = "You control a player in a Minecraft-like first-person game. \
The center of the image (the crosshair) is the direction I want to travel, but cliffs, walls, mountains or \
other terrain may block the straight path. Look at the terrain and decide how much to rotate the camera so \
that running straight afterwards leads AROUND the obstacle toward the goal: prefer gaps in walls, gentle \
slopes or staircase-like steps of height 1 block, and open flat ground; avoid cliffs higher than 1 block. \
The horizontal field of view is about 90 degrees, so something at the left edge of the image is about -45 \
and at the right edge about +45; if the best way is behind or to the side, answer with a bigger angle. \
Answer with ONE line only in exactly this format: TURN=<integer degrees from -180 to 180> \
(positive = turn right, negative = turn left, 0 = straight ahead is fine).";

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut c: Vec<u8> = Vec::with_capacity(4 + data.len());
    c.extend_from_slice(kind);
    c.extend_from_slice(data);
    out.extend_from_slice(&c);
    out.extend_from_slice(&crc32(&c).to_be_bytes());
}

/// PNG без сжатия (stored-блоки deflate): просто и без библиотек.
pub fn png_rgb(w: usize, h: usize, rgb: &[u8]) -> Vec<u8> {
    let mut raw: Vec<u8> = Vec::with_capacity((w * 3 + 1) * h);
    for y in 0..h {
        raw.push(0);
        raw.extend_from_slice(&rgb[y * w * 3..(y + 1) * w * 3]);
    }
    let mut z: Vec<u8> = vec![0x78, 0x01];
    let mut pos = 0usize;
    loop {
        let n = (raw.len() - pos).min(65535);
        let last = pos + n >= raw.len();
        z.push(if last { 1 } else { 0 });
        z.extend_from_slice(&(n as u16).to_le_bytes());
        z.extend_from_slice(&(!(n as u16)).to_le_bytes());
        z.extend_from_slice(&raw[pos..pos + n]);
        pos += n;
        if last {
            break;
        }
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut out: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr: Vec<u8> = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len() / 3 * 4 + 4);
    for c in data.chunks(3) {
        let b0 = c[0] as u32;
        let b1 = if c.len() > 1 { c[1] as u32 } else { 0 };
        let b2 = if c.len() > 2 { c[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        s.push(T[((n >> 18) & 63) as usize] as char);
        s.push(T[((n >> 12) & 63) as usize] as char);
        s.push(if c.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        s.push(if c.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    s
}

fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => {}
            c => o.push(c),
        }
    }
    o
}

fn extract_content(t: &str) -> Option<String> {
    let m = t.find("\"message\"").unwrap_or(0);
    let c = t[m..].find("\"content\"")? + m;
    let b: Vec<char> = t[c + 9..].chars().collect();
    let mut i = 0usize;
    while i < b.len() && (b[i] == ':' || b[i].is_whitespace()) {
        i += 1;
    }
    if i >= b.len() || b[i] != '"' {
        return None;
    }
    i += 1;
    let mut out = String::new();
    while i < b.len() {
        let ch = b[i];
        if ch == '"' {
            return Some(out);
        }
        if ch == '\\' && i + 1 < b.len() {
            i += 1;
            match b[i] {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => {}
                'u' => {
                    i += 4;
                    out.push('?');
                }
                o => out.push(o),
            }
        } else {
            out.push(ch);
        }
        i += 1;
    }
    None
}

/// Достаёт угол из ответа вида "TURN=-35".
pub fn parse_turn(text: &str) -> Option<f64> {
    let up = text.to_uppercase();
    let i = up.find("TURN")?;
    let rest: Vec<char> = up[i + 4..].chars().collect();
    let mut k = 0usize;
    while k < rest.len() && !(rest[k].is_ascii_digit() || rest[k] == '-' || rest[k] == '+') {
        k += 1;
        if k > 8 {
            return None;
        }
    }
    let mut s = String::new();
    if k < rest.len() && (rest[k] == '-' || rest[k] == '+') {
        if rest[k] == '-' {
            s.push('-');
        }
        k += 1;
    }
    while k < rest.len() && rest[k].is_ascii_digit() {
        s.push(rest[k]);
        k += 1;
    }
    let v: f64 = s.parse().ok()?;
    Some(v.clamp(-180.0, 180.0))
}

/// Токен: переменная окружения HF_TOKEN или файл hf_token.txt в папке настроек.
pub fn load_token(dir: &Path) -> Option<String> {
    if let Ok(t) = std::env::var("HF_TOKEN") {
        if !t.trim().is_empty() {
            return Some(t.trim().to_string());
        }
    }
    let t = std::fs::read_to_string(dir.join("hf_token.txt")).ok()?;
    let t = t.trim().to_string();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

fn ask_blocking(dir: &Path, token: &str, url: &str, model: &str, png: &[u8], prompt: &str) -> Result<String, String> {
    let mut body = String::with_capacity(png.len() * 4 / 3 + 1024);
    body.push_str("{\"model\":\"");
    body.push_str(&json_escape(model));
    body.push_str("\",\"max_tokens\":600,\"temperature\":0.2,\"messages\":[{\"role\":\"user\",\"content\":[");
    body.push_str("{\"type\":\"text\",\"text\":\"");
    body.push_str(&json_escape(prompt));
    body.push_str("\"},{\"type\":\"image_url\",\"image_url\":{\"url\":\"data:image/png;base64,");
    body.push_str(&base64(png));
    body.push_str("\"}}]}]}");
    std::fs::write(dir.join("ai_req.json"), body).map_err(|e| e.to_string())?;
    // токен кладём во временный файл заголовков, чтобы он не светился в списке процессов
    let hdr = format!("Authorization: Bearer {}\r\nContent-Type: application/json\r\n", token);
    std::fs::write(dir.join("ai_hdr.txt"), hdr).map_err(|e| e.to_string())?;
    let out = Command::new("curl.exe")
        .current_dir(dir)
        .arg("-s")
        .arg("-m")
        .arg("60")
        .arg("-X")
        .arg("POST")
        .arg(url)
        .arg("-H")
        .arg("@ai_hdr.txt")
        .arg("--data-binary")
        .arg("@ai_req.json")
        .output();
    let _ = std::fs::remove_file(dir.join("ai_hdr.txt"));
    let _ = std::fs::remove_file(dir.join("ai_req.json"));
    let out = out.map_err(|e| format!("curl.exe не запустился: {}", e))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if text.trim().is_empty() {
        return Err(format!("пустой ответ. {}", String::from_utf8_lossy(&out.stderr)));
    }
    match extract_content(&text) {
        Some(c) => Ok(c),
        None => Err(text.chars().take(300).collect()),
    }
}

/// Запускает запрос в отдельном потоке; результат придёт в канал как "OK:текст" или "ERR:причина".
pub fn ask(dir: PathBuf, token: String, url: String, model: String, png: Vec<u8>, prompt: String, tx: Sender<String>) {
    std::thread::spawn(move || {
        let r = ask_blocking(&dir, &token, &url, &model, &png, &prompt);
        let _ = tx.send(match r {
            Ok(t) => format!("OK:{}", t),
            Err(e) => format!("ERR:{}", e),
        });
    });
}

}

mod glyphs {
// Эталоны символов (сетка 9x13, 0..255). Сгенерировано из скриншота игры.
// Формат: (символ, пропорция ширина/высота * 1000, данные)
// Девятка ('9') собрана из двух независимых образцов: перевёрнутая шестёрка и шрифт Liberation Sans Bold.
pub const GLYPH_DATA: &[(char, u16, [u8; 117])] = &[
    ('X', 846, [157,130,30,0,0,0,59,154,101,76,248,111,0,0,20,153,241,43,30,198,218,51,0,62,240,137,13,0,85,243,110,20,189,224,69,0,0,13,153,236,135,248,136,0,0,0,0,87,229,244,212,53,0,0,0,0,32,200,255,185,0,0,0,0,0,113,242,250,231,92,0,0,0,45,193,216,47,243,157,18,0,0,86,246,69,9,143,238,80,0,37,221,185,34,0,56,227,179,25,139,241,101,0,0,2,116,242,55,239,172,13,0,0,0,61,211,209]),
    (':', 154, [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,229,229,229,229,224,219,219,219,219,255,255,255,255,249,243,243,243,243,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,18,18,18,18,16,14,14,14,14,219,219,219,219,213,207,207,207,207,255,255,255,255,249,243,243,243,243]),
    ('-', 385, [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,72,82,120,120,120,120,120,90,82,185,199,255,255,255,255,255,210,199,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]),
    ('7', 615, [167,167,167,167,167,167,167,167,167,253,253,253,253,253,253,253,253,253,0,0,0,0,0,12,86,244,225,0,0,0,0,0,58,179,229,60,0,0,0,0,18,114,229,158,0,0,0,0,0,104,224,187,0,0,0,0,0,67,179,218,122,0,0,0,0,0,142,240,158,0,0,0,0,0,63,190,227,125,0,0,0,0,0,145,232,179,64,0,0,0,0,0,184,251,128,0,0,0,0,0,4,192,255,128,0,0,0,0,0,47,205,246,120,0,0,0,0]),
    ('8', 692, [0,0,40,157,221,161,46,0,0,0,187,239,233,169,233,239,195,0,18,251,223,0,0,0,227,251,30,126,255,181,0,0,0,169,255,147,22,255,221,0,0,0,221,255,38,0,94,241,199,122,195,241,100,0,0,0,187,253,243,253,195,0,0,0,199,221,66,0,58,221,205,14,143,253,147,0,0,0,129,255,169,211,255,94,0,0,0,76,255,221,193,255,167,0,0,0,151,255,209,52,251,241,153,72,149,237,251,74,0,38,205,249,231,247,213,52,0]),
    ('5', 692, [78,167,167,167,167,167,167,149,0,159,253,249,247,247,247,247,221,0,179,255,78,0,0,0,0,0,0,197,255,34,0,0,0,0,0,0,213,255,6,0,0,0,0,0,0,233,255,191,203,223,213,153,0,0,249,255,249,177,135,243,249,195,0,6,6,0,0,0,0,253,249,0,0,0,0,0,0,0,203,255,124,0,0,0,0,0,0,227,255,131,181,201,34,0,0,24,251,253,48,106,255,221,161,163,217,255,191,0,0,74,173,243,251,235,112,0,0]),
    ('5', 615, [167,167,167,167,167,167,167,152,50,253,253,250,248,247,247,247,229,104,255,239,59,0,0,0,0,0,0,255,236,58,0,0,0,0,0,0,255,232,57,0,0,0,0,0,0,255,248,190,197,218,215,176,83,0,255,255,240,163,156,215,251,218,68,6,3,1,0,0,48,160,250,215,0,0,0,0,0,0,64,254,251,0,0,0,0,0,5,74,255,255,201,181,45,0,0,59,181,253,243,223,247,206,164,167,206,244,229,60,0,100,187,235,251,235,160,21,0]),
    ('Y', 846, [112,154,60,0,0,0,0,95,163,44,244,134,10,0,0,56,204,218,20,162,235,60,0,0,107,246,66,0,80,238,132,8,45,205,195,30,0,9,148,239,23,80,243,88,0,0,0,91,231,177,221,181,34,0,0,0,0,172,246,239,113,0,0,0,0,0,77,241,191,12,0,0,0,0,0,20,234,185,0,0,0,0,0,0,13,233,185,0,0,0,0,0,0,13,233,185,0,0,0,0,0,0,13,233,185,0,0,0,0,0,0,13,233,185,0,0,0]),
    (':', 231, [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,161,161,161,229,229,229,118,118,118,183,183,183,255,255,255,139,139,139,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,18,18,18,0,0,0,153,153,153,219,219,219,110,110,110,183,183,183,255,255,255,139,139,139]),
    ('1', 692, [0,0,0,141,167,133,0,0,0,0,86,231,249,253,207,0,0,0,126,239,157,175,255,211,0,0,0,118,6,0,118,255,211,0,0,0,0,0,0,112,255,211,0,0,0,0,0,0,112,255,211,0,0,0,0,0,0,112,255,211,0,0,0,0,0,0,112,255,211,0,0,0,0,0,0,112,255,211,0,0,0,0,0,0,112,255,211,0,0,0,0,0,0,112,255,211,0,0,0,90,137,141,217,255,243,137,137,104,187,249,249,253,255,255,249,249,205]),
    ('4', 769, [0,0,0,0,73,159,167,33,0,0,0,0,39,175,252,253,51,0,0,0,0,97,232,240,254,51,0,0,0,58,213,140,165,254,51,0,0,15,128,168,23,151,254,51,0,0,47,217,105,0,151,254,51,0,18,190,169,0,0,151,254,51,0,77,209,41,0,0,151,254,51,0,229,208,46,16,54,189,254,94,5,255,247,221,221,226,245,255,231,171,24,24,24,24,61,191,254,102,6,0,0,0,0,0,151,254,51,0,0,0,0,0,0,151,254,51,0]),
    ('2', 692, [0,0,54,181,217,124,2,0,0,0,221,241,251,249,247,235,116,0,94,253,207,0,0,60,251,237,0,74,118,6,0,0,0,231,255,38,0,0,0,0,0,0,247,255,0,0,0,0,0,0,175,255,213,0,0,0,0,0,171,249,243,0,0,0,0,0,179,249,233,0,0,0,0,0,187,249,195,0,0,0,0,0,191,251,169,0,0,0,0,0,120,251,235,0,0,0,0,0,0,235,255,249,187,177,177,177,177,94,249,255,255,255,255,255,255,255,157]),
    ('Z', 769, [22,167,167,167,167,167,167,167,109,74,253,253,253,253,253,253,253,181,0,0,0,0,0,55,206,250,45,0,0,0,0,0,129,243,128,10,0,0,0,0,83,217,189,33,0,0,0,0,43,179,224,62,0,0,0,0,12,120,237,94,0,0,0,0,0,66,233,151,20,0,0,0,0,38,209,199,59,0,0,0,0,15,167,227,106,0,0,0,0,0,104,241,162,0,0,0,0,0,0,226,254,232,186,177,177,177,177,168,246,255,255,255,255,255,255,255,246]),
    (':', 154, [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,229,229,229,229,225,221,221,221,221,255,255,255,255,252,249,249,249,249,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,18,18,18,18,17,16,16,16,16,219,219,219,219,215,211,211,211,211,255,255,255,255,252,249,249,249,249]),
    ('-', 385, [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,60,72,120,120,120,120,120,116,114,165,183,255,255,255,255,255,245,243,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]),
    ('1', 692, [0,0,0,147,167,94,0,0,0,0,112,233,251,253,163,0,0,0,155,239,139,195,255,165,0,0,0,129,0,0,155,255,165,0,0,0,0,0,0,149,255,165,0,0,0,0,0,0,149,255,165,0,0,0,0,0,0,149,255,165,0,0,0,0,0,0,149,255,165,0,0,0,0,0,0,149,255,165,0,0,0,0,0,0,149,255,165,0,0,0,0,0,0,149,255,165,0,0,0,104,137,141,227,255,231,137,137,72,205,249,249,253,255,253,249,249,159]),
    ('0', 692, [0,0,52,183,227,203,98,0,0,0,100,241,251,243,253,245,159,0,0,239,249,40,0,0,247,247,0,58,255,207,0,0,0,145,255,147,118,255,169,0,0,0,84,255,217,149,255,145,0,0,0,56,255,233,171,255,126,0,0,0,34,255,235,155,255,139,0,0,0,44,255,235,126,255,163,0,0,0,66,255,223,76,255,195,0,0,0,122,255,167,0,249,237,0,0,0,221,251,20,0,137,253,205,153,207,251,191,0,0,0,112,235,249,241,151,0,0]),
    ('0', 692, [0,0,133,211,227,169,0,0,0,0,205,249,253,243,249,235,18,0,76,253,215,0,0,106,255,213,0,217,255,68,0,0,0,255,253,0,251,251,0,0,0,0,255,255,34,255,249,0,0,0,0,239,255,76,255,247,0,0,0,0,221,255,106,255,249,0,0,0,0,229,255,86,255,251,0,0,0,0,249,255,46,231,253,34,0,0,0,253,255,0,108,255,177,0,0,102,255,229,0,0,233,245,173,163,235,255,60,0,0,0,189,249,251,219,58,0,0]),
    ('5', 615, [163,166,167,167,167,167,167,155,76,249,252,250,248,247,247,247,233,135,251,254,64,0,0,0,0,0,0,251,254,64,0,0,0,0,0,0,253,255,64,0,0,0,0,0,0,253,255,192,196,217,217,183,97,0,255,255,244,171,154,207,250,223,100,6,6,2,0,0,35,135,251,223,0,0,0,0,0,0,61,244,253,0,0,0,0,0,0,62,250,255,199,197,49,0,0,48,160,254,247,211,249,210,166,164,200,240,235,92,0,93,179,232,251,238,171,40,0]),
    ('3', 692, [0,0,56,179,219,131,10,0,0,0,215,241,249,249,247,237,120,0,126,253,183,0,0,74,253,237,0,30,78,0,0,0,0,225,255,30,0,0,0,0,0,0,247,247,0,0,0,0,36,122,215,227,52,0,0,0,0,205,243,237,24,0,0,0,0,0,42,118,209,215,145,0,0,0,0,0,0,0,221,247,46,26,62,0,0,0,0,175,255,157,177,229,108,0,0,0,221,255,143,44,251,235,179,153,199,249,245,0,0,38,147,235,251,253,195,16,0]),
    ('2', 692, [0,0,0,106,209,189,76,0,0,0,52,235,245,249,249,243,231,0,0,205,255,100,0,0,177,255,133,0,98,120,0,0,0,46,255,227,0,0,0,0,0,0,124,255,203,0,0,0,0,0,0,231,255,94,0,0,0,0,12,217,253,179,0,0,0,0,30,219,253,163,0,0,0,0,52,221,251,100,0,0,0,0,24,225,249,54,0,0,0,0,0,203,255,141,0,0,0,0,0,82,251,255,227,179,177,177,177,177,124,255,255,255,255,255,255,255,255]),
    ('3', 692, [0,0,38,163,221,147,24,0,0,0,193,239,249,249,249,237,161,0,68,251,207,0,0,32,251,245,0,8,72,0,0,0,0,201,255,88,0,0,0,0,0,0,239,249,0,0,0,0,18,110,205,235,82,0,0,0,0,177,241,243,62,0,0,0,0,0,24,108,197,221,159,0,0,0,0,0,0,0,193,249,98,20,56,0,0,0,0,124,255,193,137,229,143,0,0,0,195,255,189,0,247,241,183,153,193,247,249,44,0,20,133,225,251,255,209,42,0]),
    ('6', 615, [0,0,75,162,211,194,113,16,0,0,153,227,249,250,249,245,208,22,70,227,228,92,7,30,124,247,193,205,249,110,23,0,0,0,0,0,251,251,63,0,0,0,0,0,0,255,236,94,131,196,204,163,65,0,255,251,231,199,170,191,245,225,157,255,255,145,41,0,7,77,252,235,255,250,62,0,0,0,58,235,253,247,252,72,5,0,0,60,243,255,159,243,178,57,0,9,81,252,249,0,207,240,202,166,177,226,245,179,0,0,140,226,249,247,214,111,0]),
    ('6', 692, [0,0,0,159,221,197,88,0,0,0,26,231,249,247,249,243,189,0,0,213,251,141,0,0,215,247,36,34,253,217,0,0,0,0,0,0,124,255,183,0,0,0,0,0,0,159,255,145,86,207,211,151,0,0,187,255,237,219,141,215,247,221,0,181,255,229,0,0,0,179,255,126,155,255,179,0,0,0,64,255,217,110,255,201,0,0,0,70,255,229,0,249,239,10,0,0,175,255,201,0,141,253,213,151,181,243,247,16,0,0,96,229,249,253,207,24,0]),
    ('4', 769, [0,0,0,0,10,108,167,54,3,0,0,0,0,116,245,253,115,8,0,0,0,64,205,235,246,117,8,0,0,9,117,172,147,233,117,8,0,0,67,213,99,110,233,117,8,0,29,173,146,0,111,234,117,8,2,63,200,59,0,111,234,117,8,21,208,139,0,0,111,234,117,8,112,231,113,18,18,140,244,181,29,161,251,231,221,221,238,253,244,223,2,24,24,24,28,145,245,184,38,0,0,0,0,0,114,236,126,9,0,0,0,0,0,111,234,117,8]),
    ('9', 615, [0,111,214,247,249,226,140,0,0,179,245,226,177,166,202,240,207,0,249,252,81,9,0,57,178,243,159,255,243,60,0,0,5,72,252,247,253,235,58,0,0,0,62,250,255,235,252,77,7,0,41,145,255,255,157,225,245,191,170,199,231,251,255,0,65,163,204,196,131,94,236,255,0,0,0,0,0,0,63,251,251,0,0,0,0,0,23,110,249,205,193,247,124,30,7,92,228,227,70,22,208,245,249,250,249,227,153,0,0,16,113,194,211,162,75,0,0]),
    ('9', 692, [0,24,207,253,249,229,96,0,0,16,247,243,181,151,213,253,141,0,201,255,175,0,0,10,239,249,0,229,255,70,0,0,0,201,255,110,217,255,64,0,0,0,179,255,155,126,255,179,0,0,0,229,255,181,0,221,247,215,141,219,237,255,187,0,0,151,211,207,86,145,255,159,0,0,0,0,0,0,183,255,124,0,0,0,0,0,0,217,253,34,36,247,215,0,0,141,251,213,0,0,189,243,249,247,249,231,26,0,0,0,88,197,221,159,0,0,0]),
    ('9', 583, [0,30,112,210,243,181,73,0,0,90,195,232,215,156,233,239,167,0,229,245,160,36,25,40,164,222,139,255,210,110,0,0,0,146,249,233,255,195,98,0,0,0,146,255,255,255,227,124,0,0,0,146,255,255,202,238,188,96,50,110,194,251,255,87,166,207,210,141,140,192,238,255,0,55,113,156,131,19,128,232,252,40,48,30,0,0,1,145,241,213,153,197,138,34,27,45,165,207,92,112,203,227,205,169,234,231,158,0,0,50,131,217,239,153,59,0,0]),
    ('9', 667, [0,5,142,223,227,149,38,0,0,79,215,246,220,224,243,211,113,0,227,251,96,37,37,120,248,206,0,255,252,63,0,0,60,183,228,38,255,251,63,0,0,40,143,236,102,255,254,63,0,0,63,189,241,146,185,246,156,93,93,155,235,242,154,67,169,227,203,167,156,183,240,131,0,49,131,151,94,52,118,233,76,37,53,14,0,0,57,177,224,11,138,215,88,36,37,121,244,194,0,77,217,243,215,223,243,189,47,0,0,33,159,226,215,117,0,0,0]),
    ('9', 667, [0,0,123,211,234,191,96,0,0,44,210,244,247,250,250,240,198,0,219,250,148,68,42,97,204,238,125,254,255,80,8,0,16,97,252,229,255,255,64,0,0,0,64,255,255,255,255,89,13,0,23,110,255,255,168,244,181,126,111,134,189,255,255,47,154,222,230,204,165,164,255,255,0,29,117,153,119,50,64,255,252,33,50,14,0,0,18,100,249,206,117,206,88,42,41,102,213,227,75,37,209,245,245,247,248,230,174,0,0,0,137,218,228,166,62,0,0]),
];

}

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::ffi::c_void;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::thread::sleep;
use std::time::{Duration, Instant};

// ============================================================================
//  WinAPI (объявлено вручную, внешние библиотеки не нужны)
// ============================================================================
type HANDLE = *mut c_void;

#[repr(C)]
struct BITMAPINFOHEADER {
    biSize: u32,
    biWidth: i32,
    biHeight: i32,
    biPlanes: u16,
    biBitCount: u16,
    biCompression: u32,
    biSizeImage: u32,
    biXPelsPerMeter: i32,
    biYPelsPerMeter: i32,
    biClrUsed: u32,
    biClrImportant: u32,
}

#[repr(C)]
struct BITMAPINFO {
    header: BITMAPINFOHEADER,
    colors: [u32; 1],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct KEYBDINPUT {
    wVk: u16,
    wScan: u16,
    dwFlags: u32,
    time: u32,
    dwExtraInfo: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MOUSEINPUT {
    dx: i32,
    dy: i32,
    mouseData: u32,
    dwFlags: u32,
    time: u32,
    dwExtraInfo: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
union INPUT_U {
    mi: MOUSEINPUT,
    ki: KEYBDINPUT,
}

#[repr(C)]
struct INPUT {
    type_: u32,
    u: INPUT_U,
}

#[link(name = "user32")]
extern "system" {
    fn SetProcessDPIAware() -> i32;
    fn GetSystemMetrics(index: i32) -> i32;
    fn GetDC(hwnd: HANDLE) -> HANDLE;
    fn ReleaseDC(hwnd: HANDLE, hdc: HANDLE) -> i32;
    fn GetAsyncKeyState(vk: i32) -> i16;
    fn SendInput(count: u32, inputs: *const INPUT, size: i32) -> u32;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateCompatibleDC(hdc: HANDLE) -> HANDLE;
    fn CreateCompatibleBitmap(hdc: HANDLE, w: i32, h: i32) -> HANDLE;
    fn SelectObject(hdc: HANDLE, obj: HANDLE) -> HANDLE;
    fn BitBlt(dst: HANDLE, x: i32, y: i32, w: i32, h: i32, src: HANDLE, sx: i32, sy: i32, rop: u32) -> i32;
    fn GetDIBits(hdc: HANDLE, bmp: HANDLE, start: u32, lines: u32, bits: *mut c_void, bmi: *mut BITMAPINFO, usage: u32) -> i32;
    fn DeleteObject(obj: HANDLE) -> i32;
    fn DeleteDC(hdc: HANDLE) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn SetConsoleOutputCP(cp: u32) -> i32;
    fn Beep(freq: u32, ms: u32) -> i32;
}

#[link(name = "winmm")]
extern "system" {
    fn timeBeginPeriod(period: u32) -> u32;
}

fn beep(freq: u32, ms: u32) {
    unsafe {
        Beep(freq, ms);
    }
}

// ============================================================================
//  Ввод: клавиши (со скан-кодами) и мышь
// ============================================================================
const SC_W: u16 = 0x11;
const SC_A: u16 = 0x1E;
const SC_S: u16 = 0x1F;
const SC_D: u16 = 0x20;
const SC_SPACE: u16 = 0x39;

fn send(inp: &INPUT) {
    unsafe {
        SendInput(1, inp as *const INPUT, std::mem::size_of::<INPUT>() as i32);
    }
}

fn key(sc: u16, down: bool) {
    let flags: u32 = 0x0008 | if down { 0 } else { 0x0002 }; // SCANCODE | KEYUP
    let inp = INPUT {
        type_: 1,
        u: INPUT_U {
            ki: KEYBDINPUT { wVk: 0, wScan: sc, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    };
    send(&inp);
}

fn kd(sc: u16) {
    key(sc, true);
}

fn ku(sc: u16) {
    key(sc, false);
}

fn mouse_move(dx: i32) {
    if dx == 0 {
        return;
    }
    let inp = INPUT {
        type_: 0,
        u: INPUT_U {
            mi: MOUSEINPUT { dx, dy: 0, mouseData: 0, dwFlags: 0x0001, time: 0, dwExtraInfo: 0 },
        },
    };
    send(&inp);
}

fn mouse_button(down: bool) {
    let inp = INPUT {
        type_: 0,
        u: INPUT_U {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: if down { 0x0002 } else { 0x0004 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    send(&inp);
}

fn key_down(vk: i32) -> bool {
    unsafe { (GetAsyncKeyState(vk) as u16) & 0x8000 != 0 }
}

// ============================================================================
//  Хоткеи
// ============================================================================
#[derive(Clone, Copy)]
struct Hk {
    vk: i32,
    shift: bool,
}

const HK_TOGGLE: usize = 0;
const HK_STOP: usize = 1;
const HK_MODE: usize = 2;
const HK_CALIB: usize = 3;
const HK_KEYTEST: usize = 4;
const HK_READTEST: usize = 5;
const HK_WATERCAL: usize = 6;
const HK_PICKHUD: usize = 7;

// F8, F9, F4, F10, F2, Shift+F2 (эти F-клавиши браузер сам не использует)
const HOTKEYS: [Hk; 8] = [
    Hk { vk: 0x77, shift: false },
    Hk { vk: 0x78, shift: false },
    Hk { vk: 0x73, shift: false },
    Hk { vk: 0x79, shift: false },
    Hk { vk: 0x71, shift: false },
    Hk { vk: 0x71, shift: true },
    Hk { vk: 0x2E, shift: false }, // Delete - запомнить цвет воды (F6 в браузере уводит фокус в адресную строку)
    Hk { vk: 0x2D, shift: false }, // Insert - указать, где в игре строка координат
];

fn hk_down(h: Hk) -> bool {
    key_down(h.vk) && key_down(0x10) == h.shift && !key_down(0x11) && !key_down(0x12)
}

// ============================================================================
//  Захват экрана (GDI)
// ============================================================================
struct Capture {
    sdc: HANDLE,
    mdc: HANDLE,
    bmp: HANDLE,
    w: i32,
    h: i32,
    buf: Vec<u8>, // BGRA, сверху вниз
}

impl Capture {
    fn new(w: i32, h: i32) -> Capture {
        let w = w.max(1);
        let h = h.max(1);
        unsafe {
            let sdc = GetDC(std::ptr::null_mut());
            let mdc = CreateCompatibleDC(sdc);
            let bmp = CreateCompatibleBitmap(sdc, w, h);
            Capture { sdc, mdc, bmp, w, h, buf: vec![0u8; (w as usize) * (h as usize) * 4] }
        }
    }

    /// Снимает прямоугольник экрана (x, y, w, h) в self.buf.
    fn grab(&mut self, x: i32, y: i32) -> bool {
        unsafe {
            let old = SelectObject(self.mdc, self.bmp);
            let ok = BitBlt(self.mdc, 0, 0, self.w, self.h, self.sdc, x, y, 0x00CC0020); // SRCCOPY
            SelectObject(self.mdc, old);
            if ok == 0 {
                return false;
            }
            let mut bmi = BITMAPINFO {
                header: BITMAPINFOHEADER {
                    biSize: 40,
                    biWidth: self.w,
                    biHeight: -self.h, // отрицательная высота = сверху вниз
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: 0,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                colors: [0u32; 1],
            };
            let lines = GetDIBits(
                self.sdc,
                self.bmp,
                0,
                self.h as u32,
                self.buf.as_mut_ptr() as *mut c_void,
                &mut bmi as *mut BITMAPINFO,
                0,
            );
            lines != 0
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.bmp);
            DeleteDC(self.mdc);
            ReleaseDC(std::ptr::null_mut(), self.sdc);
        }
    }
}

fn save_bmp(path: &PathBuf, w: usize, h: usize, bgra: &[u8]) {
    let row = (w * 3 + 3) & !3usize;
    let size = 54 + row * h;
    let mut out: Vec<u8> = Vec::with_capacity(size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&((row * h) as u32).to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for y in (0..h).rev() {
        for x in 0..w {
            let i = (y * w + x) * 4;
            out.push(bgra[i]);
            out.push(bgra[i + 1]);
            out.push(bgra[i + 2]);
        }
        for _ in 0..(row - w * 3) {
            out.push(0);
        }
    }
    let _ = std::fs::write(path, out);
}

// ============================================================================
//  Распознавание координат по эталонам символов
// ============================================================================
const GW: usize = 9;
const GH: usize = 13;
const NG: usize = GW * GH;
type Grid = [f32; NG];

const MASK_T: u8 = 170; // пиксель "светлый" (текст), если min(R,G,B) больше
const ACCEPT: f32 = 0.30; // макс. расстояние до эталона
const SHIFTS: [f32; 5] = [-0.6, -0.3, 0.0, 0.3, 0.6];

fn inten(mn: u8) -> f32 {
    ((mn as f32 - 128.0) / 127.0).clamp(0.0, 1.0)
}

fn shifted(g: &Grid, dx: i32) -> Grid {
    let mut out = [0f32; NG];
    for y in 0..GH {
        for x in 0..GW {
            let sx = x as i32 - dx;
            if sx >= 0 && sx < GW as i32 {
                out[y * GW + x] = g[y * GW + sx as usize];
            }
        }
    }
    out
}

fn fshift(g: &Grid, d: f32) -> Grid {
    if d == 0.0 {
        return *g;
    }
    let n = d.floor();
    let f = d - n;
    let n = n as i32;
    let a = shifted(g, n);
    let b = shifted(g, n + 1);
    let mut out = [0f32; NG];
    for i in 0..NG {
        out[i] = a[i] * (1.0 - f) + b[i] * f;
    }
    out
}

fn blur(g: &Grid) -> Grid {
    let mut out = [0f32; NG];
    for y in 0..GH as i32 {
        for x in 0..GW as i32 {
            let mut s = 0f32;
            for dy in -1..=1i32 {
                for dx in -1..=1i32 {
                    let yy = y + dy;
                    let xx = x + dx;
                    if yy >= 0 && yy < GH as i32 && xx >= 0 && xx < GW as i32 {
                        s += g[yy as usize * GW + xx as usize];
                    }
                }
            }
            out[y as usize * GW + x as usize] = s / 9.0;
        }
    }
    out
}

/// Приводит блок символа [xa..=xb] x [top..top+h) к сетке GW x GH (усреднение по площади).
fn sample_grid(mn: &[u8], w: usize, top: usize, h: usize, xa: usize, xb: usize) -> Grid {
    let sw = xb - xa + 1;
    let mut out = [0f32; NG];
    for gy in 0..GH {
        let ya = gy as f32 * h as f32 / GH as f32;
        let yb = (gy + 1) as f32 * h as f32 / GH as f32;
        for gx in 0..GW {
            let xs = gx as f32 * sw as f32 / GW as f32;
            let xe = (gx + 1) as f32 * sw as f32 / GW as f32;
            let mut acc = 0f32;
            let mut ws = 0f32;
            let y_start = ya.floor() as usize;
            let y_end = (yb.ceil() as usize).min(h);
            for yy in y_start..y_end {
                let wy = yb.min((yy + 1) as f32) - ya.max(yy as f32);
                if wy <= 0.0 {
                    continue;
                }
                let x_start = xs.floor() as usize;
                let x_end = (xe.ceil() as usize).min(sw);
                for xx in x_start..x_end {
                    let wx = xe.min((xx + 1) as f32) - xs.max(xx as f32);
                    if wx <= 0.0 {
                        continue;
                    }
                    let v = inten(mn[(top + yy) * w + xa + xx]);
                    acc += v * wx * wy;
                    ws += wx * wy;
                }
            }
            out[gy * GW + gx] = if ws > 0.0 { acc / ws } else { 0.0 };
        }
    }
    out
}

fn rms(a: &Grid, b: &Grid) -> f32 {
    let mut s = 0f32;
    for i in 0..NG {
        let d = a[i] - b[i];
        s += d * d;
    }
    (s / NG as f32).sqrt()
}

/// Минимум в окне (2r+1)x(2r+1): для оценки фона вокруг пикселя.
fn min_filter(src: &[u8], w: usize, h: usize, r: usize) -> Vec<u8> {
    let mut tmp = vec![255u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let a = x.saturating_sub(r);
            let b = (x + r).min(w - 1);
            let mut m = 255u8;
            for xx in a..=b {
                let v = src[y * w + xx];
                if v < m {
                    m = v;
                }
            }
            tmp[y * w + x] = m;
        }
    }
    let mut out = vec![255u8; w * h];
    for y in 0..h {
        let a = y.saturating_sub(r);
        let b = (y + r).min(h - 1);
        for x in 0..w {
            let mut m = 255u8;
            for yy in a..=b {
                let v = tmp[yy * w + x];
                if v < m {
                    m = v;
                }
            }
            out[y * w + x] = m;
        }
    }
    out
}

struct Tmpl {
    ch: char,
    asp: f32,
    g: Grid,
}

struct Parsed {
    x: i32,
    y: i32,
    z: i32,
    x0: usize,
    x1: usize,
    top: usize,
    h: usize,
}

struct Recognizer {
    tmpl: Vec<Tmpl>,
    mn: Vec<u8>,
    verbose: bool,
    dbg: Vec<String>,
    last_w: usize,
    last_bands: Vec<(usize, usize)>,
}

fn split_blob(cnt: &[u32], a: usize, b: usize, h: usize, depth: u32, out: &mut Vec<(usize, usize)>) {
    let w = b - a + 1;
    if (w as f32) > 1.0 * (h as f32) && depth < 2 && w >= 8 {
        let lo = a + w * 3 / 10;
        let hi = b - w * 3 / 10;
        let mut best = lo;
        for x in lo..=hi {
            if cnt[x] < cnt[best] {
                best = x;
            }
        }
        split_blob(cnt, a, best, h, depth + 1, out);
        split_blob(cnt, best + 1, b, h, depth + 1, out);
    } else {
        out.push((a, b));
    }
}

fn parse_xyz(chars: &[char]) -> Option<(i32, i32, i32)> {
    let mut i = 0usize;
    let mut vals = [0i32; 3];
    let letters = ['X', 'Y', 'Z'];
    for k in 0..3 {
        while i < chars.len() && chars[i] != letters[k] {
            i += 1;
        }
        if i >= chars.len() {
            return None;
        }
        i += 1;
        if i < chars.len() && (chars[i] == ':' || chars[i] == '?') {
            i += 1;
        }
        let start = i;
        while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '-') {
            i += 1;
        }
        if i == start || i - start > 9 {
            return None;
        }
        let s: String = chars[start..i].iter().collect();
        vals[k] = s.parse::<i32>().ok()?;
    }
    Some((vals[0], vals[1], vals[2]))
}

impl Recognizer {
    fn new() -> Recognizer {
        let mut tmpl = Vec::new();
        for &(ch, asp1000, data) in glyphs::GLYPH_DATA.iter() {
            let mut g = [0f32; NG];
            for i in 0..NG {
                g[i] = data[i] as f32 / 255.0;
            }
            for &d in SHIFTS.iter() {
                tmpl.push(Tmpl { ch, asp: asp1000 as f32 / 1000.0, g: blur(&fshift(&g, d)) });
            }
        }
        Recognizer { tmpl, mn: Vec::new(), verbose: false, dbg: Vec::new(), last_w: 0, last_bands: Vec::new() }
    }

    fn classify(&self, w: usize, top: usize, bh: usize, xa: usize, xb: usize, accept: f32) -> Option<char> {
        let g = blur(&sample_grid(&self.mn, w, top, bh, xa, xb));
        let asp = (xb - xa + 1) as f32 / bh as f32;
        let mut best = f32::MAX;
        let mut best_ch = '?';
        for t in self.tmpl.iter() {
            let d = rms(&g, &t.g) + 0.6 * (asp - t.asp).abs();
            if d < best {
                best = d;
                best_ch = t.ch;
            }
        }
        if best < accept {
            Some(best_ch)
        } else {
            None
        }
    }

    fn try_band(&self, w: usize, top: usize, bh: usize) -> (Option<Parsed>, String) {
        let mut cnt = vec![0u32; w];
        for y in top..top + bh {
            let row = &self.mn[y * w..(y + 1) * w];
            for x in 0..w {
                if row[x] > MASK_T {
                    cnt[x] += 1;
                }
            }
        }
        let mut blobs: Vec<(usize, usize)> = Vec::new();
        let mut start: Option<usize> = None;
        for x in 0..w {
            let on = cnt[x] > 0;
            match (on, start) {
                (true, None) => start = Some(x),
                (false, Some(a)) => {
                    blobs.push((a, x - 1));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(a) = start {
            blobs.push((a, w - 1));
        }
        let mut fin: Vec<(usize, usize)> = Vec::new();
        for &(a, b) in blobs.iter() {
            split_blob(&cnt, a, b, bh, 0, &mut fin);
        }
        if fin.len() < 9 || fin.len() > 40 {
            return (None, String::new());
        }
        // два прохода: строгий, потом помягче (другой масштаб/сглаживание шрифта)
        let mut text = String::new();
        for &accept in [ACCEPT, 0.46f32].iter() {
            let mut chars: Vec<char> = Vec::with_capacity(fin.len());
            for &(a, b) in fin.iter() {
                chars.push(self.classify(w, top, bh, a, b, accept).unwrap_or('?'));
            }
            text = chars.iter().collect();
            if let Some((x, y, z)) = parse_xyz(&chars) {
                if y >= -64 && y <= 600 {
                    return (Some(Parsed { x, y, z, x0: fin[0].0, x1: fin[fin.len() - 1].1, top, h: bh }), text);
                }
            }
        }
        (None, text)
    }

    /// Рисует в тексте, что бот видит в первых подходящих строках кадра (# - светлый пиксель).
    fn dump(&self) -> String {
        let mut out = String::new();
        let w = self.last_w;
        if w == 0 {
            return out;
        }
        let mut shown = 0;
        for &(top, bot) in self.last_bands.iter() {
            let bh = bot - top + 1;
            if bh < 8 || bh > 40 {
                continue;
            }
            out.push_str(&format!("  --- строка y={} высота={} ---\n", top, bh));
            for y in top..=bot {
                let mut line = String::from("  ");
                for x in 0..w.min(110) {
                    line.push(if self.mn[y * w + x] > MASK_T { '#' } else { '.' });
                }
                out.push_str(&line);
                out.push('\n');
            }
            shown += 1;
            if shown >= 2 {
                break;
            }
        }
        out
    }

    /// Ищет в кадре строку "X: .. Y: .. Z: .." и читает числа.
    fn parse_frame(&mut self, buf: &[u8], w: usize, h: usize) -> Option<Parsed> {
        self.mn.clear();
        self.last_w = w;
        for i in 0..w * h {
            let b = buf[i * 4];
            let g = buf[i * 4 + 1];
            let r = buf[i * 4 + 2];
            let lo = b.min(g).min(r);
            let hi = b.max(g).max(r);
            // текст белый (нейтральный): цветные яркие пиксели фона отбрасываем
            self.mn.push(if hi - lo > 24 { 0 } else { lo }); // текст чисто белый; розоватый снег, песок и т.п. отбрасываем
        }
        // локальный контраст: текст заметно ярче фона рядом. Ровный светлый фон (снег, облака, песок
        // под полупрозрачной плашкой) раньше заливал всю строку "светлыми" пикселями и ломал чтение.
        let lm = min_filter(&self.mn, w, h, 4);
        for i in 0..w * h {
            if self.mn[i] > MASK_T && (self.mn[i] as i32 - lm[i] as i32) < 35 {
                self.mn[i] = 0;
            }
        }
        let mut bands: Vec<(usize, usize)> = Vec::new();
        let mut start: Option<usize> = None;
        for y in 0..h {
            let any = self.mn[y * w..(y + 1) * w].iter().filter(|&&v| v > MASK_T).count() >= 2;
            match (any, start) {
                (true, None) => start = Some(y),
                (false, Some(s)) => {
                    bands.push((s, y - 1));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            bands.push((s, h - 1));
        }
        self.last_bands = bands.clone();
        for &(top, bot) in bands.iter() {
            let bh = bot - top + 1;
            if bh < 8 || bh > 40 {
                continue;
            }
            let (res, text) = self.try_band(w, top, bh);
            if self.verbose && !text.is_empty() && self.dbg.len() < 12 {
                self.dbg.push(format!("строка y={} высота={}: {}", top, bh, text));
            }
            if res.is_some() {
                return res;
            }
        }
        None
    }
}

type Region = (i32, i32, i32, i32);

struct Reader {
    cap: Capture,
    rec: Recognizer,
    cands: Vec<Region>,
    ci: usize,
    lock: Region,
    locked: bool,
    fresh: bool,
    ok_streak: u32,
    fails: u32,
    sw: i32,
    sh: i32,
}

impl Reader {
    fn new(sw: i32, sh: i32, user_region: Option<Region>, saved: Option<Region>) -> Reader {
        let mut cands: Vec<Region> = Vec::new();
        match user_region {
            Some(r) => cands.push(r),
            None => {
                if let Some(l) = saved {
                    cands.push(l); // найденная в прошлый раз строка координат
                }
                let f = |v: i32, k: f64| (v as f64 * k) as i32;
                cands.push((0, f(sh, 0.82), f(sw, 0.11), f(sh, 0.07)));
                cands.push((0, f(sh, 0.81), f(sw, 0.12), f(sh, 0.09)));
                cands.push((0, f(sh, 0.75), f(sw, 0.14), f(sh, 0.22)));
                cands.push((0, f(sh, 0.60), f(sw, 0.30), f(sh, 0.40)));
                cands.push((0, 0, sw, sh)); // последний шанс: весь экран (строка координат могла оказаться в другом месте)
            }
        }
        Reader {
            cap: Capture::new(1, 1),
            rec: Recognizer::new(),
            cands,
            ci: 0,
            lock: (0, 0, 1, 1),
            locked: false,
            fresh: false,
            ok_streak: 0,
            fails: 0,
            sw,
            sh,
        }
    }

    /// Возвращает (X, Y, Z). После первого успеха "приклеивается" к найденной строке (очень быстро).
    fn read(&mut self) -> Option<(i32, i32, i32)> {
        let (rx, ry, rw, rh) = if self.locked { self.lock } else { self.cands[self.ci] };
        let rx = rx.max(0);
        let ry = ry.max(0);
        let rw = rw.min(self.sw - rx).max(1);
        let rh = rh.min(self.sh - ry).max(1);
        if self.cap.w != rw || self.cap.h != rh {
            self.cap = Capture::new(rw, rh);
        }
        if !self.cap.grab(rx, ry) {
            return None;
        }
        blank_console(&mut self.cap.buf, rx, ry, rw, rh);
        let parsed = self.rec.parse_frame(&self.cap.buf, rw as usize, rh as usize);
        match parsed {
            Some(p) => {
                if !self.locked {
                    let m = ((p.h / 4) as i32).max(2); // запас по высоте, чтобы не цеплять соседние строки
                    let x0 = (rx + p.x0 as i32 - 6).max(0);
                    let y0 = (ry + p.top as i32 - m).max(0);
                    let w = ((p.x1 - p.x0 + 1) as i32 + 60).min(self.sw - x0).max(1);
                    let h = (p.h as i32 + 2 * m).min(self.sh - y0).max(1);
                    self.lock = (x0, y0, w, h);
                    self.locked = true;
                }
                self.fails = 0;
                self.ok_streak += 1;
                if self.locked && self.ok_streak == 40 {
                    self.fresh = true; // запоминаем область, только когда 40 чтений подряд удались
                }
                Some((p.x, p.y, p.z))
            }
            None => {
                self.ok_streak = 0;
                if self.locked {
                    self.fails += 1;
                    if self.fails > 40 {
                        self.locked = false; // потеряли строку - ищем заново
                        self.fails = 0;
                    }
                } else {
                    self.ci = (self.ci + 1) % self.cands.len();
                }
                None
            }
        }
    }
}

// ============================================================================
//  Настройки
// ============================================================================
#[derive(Clone)]
struct Cfg {
    mode_target: bool,
    tx: f64,
    tz: f64,
    kpd: f64,
    calibrated: bool,
    swim: bool,
    break_blocks: bool,
    region: Option<Region>,
    radius: f64,
    water: Option<(i32, i32, i32)>,
    locked: Option<Region>,
    hop: bool,
    ai: bool,
    ai_model: String,
    ai_url: String,
    view: bool,
}

impl Cfg {
    fn default() -> Cfg {
        Cfg {
            mode_target: false,
            tx: 1_000_000.0,
            tz: 1_000_000.0,
            kpd: 0.1,
            calibrated: false,
            swim: true,
            break_blocks: false,
            region: None,
            radius: 3.0,
            water: None,
            locked: None,
            hop: true,
            ai: false,
            ai_model: String::from("Qwen/Qwen2.5-VL-72B-Instruct"),
            ai_url: String::from("https://router.huggingface.co/v1/chat/completions"),
            view: true,
        }
    }

    fn dir() -> PathBuf {
        let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        let dir = PathBuf::from(base).join("cube_bot");
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn path() -> PathBuf {
        Cfg::dir().join("rust.ini")
    }

    fn load() -> Cfg {
        let mut c = Cfg::default();
        if let Ok(s) = std::fs::read_to_string(Cfg::path()) {
            for line in s.lines() {
                let mut it = line.splitn(2, '=');
                let k = it.next().unwrap_or("").trim();
                let v = it.next().unwrap_or("").trim();
                match k {
                    "mode" => c.mode_target = v == "target",
                    "target_x" => {
                        if let Ok(f) = v.parse::<f64>() {
                            c.tx = f;
                        }
                    }
                    "target_z" => {
                        if let Ok(f) = v.parse::<f64>() {
                            c.tz = f;
                        }
                    }
                    "deg_per_px" => {
                        if let Ok(f) = v.parse::<f64>() {
                            c.kpd = f;
                        }
                    }
                    "radius" => {
                        if let Ok(f) = v.parse::<f64>() {
                            c.radius = f.max(1.0);
                        }
                    }
                    "calibrated" => c.calibrated = v == "true",
                    "swim" => c.swim = v == "true",
                    "break_blocks" => c.break_blocks = v == "true",
                    "hop" => c.hop = v == "true",
                    "ai" => c.ai = v == "true",
                    "view" => c.view = v == "true",
                    "ai_url" => {
                        if v.starts_with("http") {
                            c.ai_url = v.to_string();
                        }
                    }
                    "ai_model" => {
                        if !v.is_empty() {
                            c.ai_model = v.to_string();
                        }
                    }
                    "water" => {
                        let p: Vec<i32> = v.split(',').filter_map(|s| s.trim().parse::<i32>().ok()).collect();
                        if p.len() == 3 {
                            c.water = Some((p[0], p[1], p[2]));
                        }
                    }
                    "locked" => {
                        let p: Vec<i32> = v.split(',').filter_map(|s| s.trim().parse::<i32>().ok()).collect();
                        if p.len() == 4 {
                            c.locked = Some((p[0], p[1], p[2], p[3]));
                        }
                    }
                    "region" => {
                        let p: Vec<i32> = v.split(',').filter_map(|s| s.trim().parse::<i32>().ok()).collect();
                        if p.len() == 4 {
                            c.region = Some((p[0], p[1], p[2], p[3]));
                        }
                    }
                    _ => {}
                }
            }
        }
        c
    }

    fn save(&self) {
        let mut s = String::new();
        s.push_str(&format!("mode={}\n", if self.mode_target { "target" } else { "grow" }));
        s.push_str(&format!("target_x={}\n", self.tx));
        s.push_str(&format!("target_z={}\n", self.tz));
        s.push_str(&format!("deg_per_px={}\n", self.kpd));
        s.push_str(&format!("radius={}\n", self.radius));
        s.push_str(&format!("calibrated={}\n", self.calibrated));
        s.push_str(&format!("swim={}\n", self.swim));
        s.push_str(&format!("hop={}\n", self.hop));
        s.push_str(&format!("ai={}\n", self.ai));
        s.push_str(&format!("view={}\n", self.view));
        s.push_str(&format!("ai_model={}\n", self.ai_model));
        s.push_str(&format!("ai_url={}\n", self.ai_url));
        s.push_str(&format!("break_blocks={}\n", self.break_blocks));
        if let Some((r, g, b)) = self.water {
            s.push_str(&format!("water={},{},{}\n", r, g, b));
        }
        if let Some((a, b, c, d)) = self.locked {
            s.push_str(&format!("locked={},{},{},{}\n", a, b, c, d));
        }
        if let Some((a, b, c, d)) = self.region {
            s.push_str(&format!("region={},{},{},{}\n", a, b, c, d));
        }
        let _ = std::fs::write(Cfg::path(), s);
    }
}

// ============================================================================
//  Вспомогательная математика
// ============================================================================
fn hyp(a: f64, b: f64) -> f64 {
    (a * a + b * b).sqrt()
}

fn adiff(a: f64, b: f64) -> f64 {
    (a - b + 180.0).rem_euclid(360.0) - 180.0
}

/// Скорость (блоков/сек) по линейной регрессии положения от времени.
fn velocity(h: &[(f64, f64, f64)]) -> (f64, f64) {
    let n = h.len() as f64;
    if n < 2.0 {
        return (0.0, 0.0);
    }
    let (mut st, mut sx, mut sz) = (0.0, 0.0, 0.0);
    for &(t, x, z) in h.iter() {
        st += t;
        sx += x;
        sz += z;
    }
    let (mt, mx, mz) = (st / n, sx / n, sz / n);
    let (mut vt, mut cx, mut cz) = (0.0, 0.0, 0.0);
    for &(t, x, z) in h.iter() {
        let dt = t - mt;
        vt += dt * dt;
        cx += dt * (x - mx);
        cz += dt * (z - mz);
    }
    if vt < 1e-9 {
        (0.0, 0.0)
    } else {
        (cx / vt, cz / vt)
    }
}

/// Боковая клавиша: +1 = D, -1 = A, 0 = отпущена.
fn set_strafe(cur: &mut i32, want: i32) {
    if *cur != want {
        if *cur > 0 {
            ku(SC_D);
        } else if *cur < 0 {
            ku(SC_A);
        }
        if want > 0 {
            kd(SC_D);
        } else if want < 0 {
            kd(SC_A);
        }
        *cur = want;
    }
}

fn norm_deg(a: f64) -> f64 {
    (a + 180.0).rem_euclid(360.0) - 180.0
}

struct Rng(u64);

impl Rng {
    fn new() -> Rng {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(88172645463325252);
        Rng(t | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn range(&mut self, a: i32, b: i32) -> i32 {
        a + (self.next() % ((b - a + 1) as u64)) as i32
    }
    fn sign(&mut self) -> i32 {
        if self.next() & 1 == 0 {
            1
        } else {
            -1
        }
    }
}

// ============================================================================
//  Бот
// ============================================================================
const GROW: (f64, f64) = (1.0, 1.0); // РОСТ: X+ и Z+
const WINDOW: f64 = 0.9; // раз в столько сек: доворот камеры или "застряли"
const STUCK_SPEED: f64 = 1.2; // блоков/сек: медленнее = застряли (на суше)
const STUCK_SPEED_WATER: f64 = 0.35; // то же в воде
const MAX_SPEED: f64 = 25.0; // быстрее физически нельзя = ошибка распознавания
const JUMP_SLACK: f64 = 4.0;
const ARRIVE: f64 = 3.0;
const WATER_FRACTION: f64 = 0.5;

// ---------- живая карта: бот пишет файлы, страница view.html их рисует ----------
const VIEW_HTML: &str = r####"<!doctype html>
<html lang="ru"><head><meta charset="utf-8"><title>Куб бот - карта</title>
<style>
html,body{margin:0;height:100%;background:#12151a;color:#dde3ea;font:13px system-ui,Segoe UI,sans-serif;overflow:hidden}
#c{display:block;width:100vw;height:100vh;cursor:grab}
#hud{position:fixed;left:10px;top:10px;background:rgba(10,12,16,.78);padding:8px 12px;border-radius:8px;line-height:1.55;min-width:230px}
#hud b{color:#fff}
#hud button{background:#2a3340;color:#dde3ea;border:1px solid #3d4a5c;border-radius:5px;padding:2px 9px;margin-right:4px;cursor:pointer}
#leg{position:fixed;right:10px;bottom:10px;background:rgba(10,12,16,.78);padding:8px 12px;border-radius:8px;line-height:1.6}
.sw{display:inline-block;width:11px;height:11px;margin-right:6px;vertical-align:-1px;border-radius:2px}
.warn{color:#ff8a65}
</style></head><body>
<canvas id="c"></canvas>
<div id="hud">
  <div><b>Куб бот</b> <span id="st"></span></div>
  <div id="pos">ждём данные...</div>
  <div id="tg"></div>
  <div><button id="bf">Следить: да</button><button id="bz1">+</button><button id="bz2">-</button></div>
</div>
<div id="leg">
  <div><span class="sw" style="background:#2e7d4f"></span>пройдено</div>
  <div><span class="sw" style="background:#d9473f"></span>стена (найдена бот.)</div>
  <div><span class="sw" style="background:#4dd0e1"></span>след бота</div>
  <div><span class="sw" style="background:#ffd54f"></span>путь A*</div>
  <div><span class="sw" style="background:#ff9800"></span>точка, на которую рулит</div>
  <div><span class="sw" style="background:#e040fb"></span>цель</div>
</div>
<script>
var POS=null, MAPD=null, zoom=6, follow=true, vx=0, vz=0;
var lastT=-1, lastChange=Date.now();
var cv=document.getElementById('c'), cx=cv.getContext('2d');
function load(name){
  var s=document.createElement('script');
  s.src=name+'?t='+Date.now();
  s.onload=function(){ s.remove(); };
  s.onerror=function(){ s.remove(); };
  document.head.appendChild(s);
}
setInterval(function(){ load('view_pos.js'); }, 500);
setInterval(function(){ load('view_map.js'); }, 3000);
load('view_pos.js'); load('view_map.js');

function resize(){ cv.width=window.innerWidth; cv.height=window.innerHeight; }
window.addEventListener('resize', resize); resize();

cv.addEventListener('wheel', function(e){
  e.preventDefault();
  zoom = Math.max(1.5, Math.min(40, zoom * (e.deltaY < 0 ? 1.15 : 1/1.15)));
}, {passive:false});
var drag=null;
cv.addEventListener('mousedown', function(e){ drag={x:e.clientX,y:e.clientY}; cv.style.cursor='grabbing'; });
window.addEventListener('mouseup', function(){ drag=null; cv.style.cursor='grab'; });
window.addEventListener('mousemove', function(e){
  if(!drag) return;
  if(follow){ follow=false; document.getElementById('bf').textContent='Следить: нет'; if(POS){ vx=POS.px; vz=POS.pz; } }
  vx -= (e.clientX-drag.x)/zoom; vz -= (e.clientY-drag.y)/zoom;
  drag={x:e.clientX,y:e.clientY};
});
document.getElementById('bf').onclick=function(){
  follow=!follow; this.textContent='Следить: '+(follow?'да':'нет');
  if(!follow && POS){ vx=POS.px; vz=POS.pz; }
};
document.getElementById('bz1').onclick=function(){ zoom=Math.min(40,zoom*1.3); };
document.getElementById('bz2').onclick=function(){ zoom=Math.max(1.5,zoom/1.3); };

function draw(){
  var W=cv.width, H=cv.height;
  cx.fillStyle='#12151a'; cx.fillRect(0,0,W,H);
  var ctrx=0, ctrz=0;
  if(POS){ ctrx=follow?POS.px:vx; ctrz=follow?POS.pz:vz; } else if(MAPD){ ctrx=MAPD.cx; ctrz=MAPD.cz; }
  function sx(x){ return W/2+(x-ctrx)*zoom; }
  function sz(z){ return H/2+(z-ctrz)*zoom; }
  // сетка по 10 блоков
  if(zoom>=3){
    cx.strokeStyle='rgba(255,255,255,0.05)'; cx.lineWidth=1; cx.beginPath();
    var x0=Math.floor((ctrx-W/2/zoom)/10)*10, x1=ctrx+W/2/zoom;
    for(var gx=x0; gx<=x1; gx+=10){ cx.moveTo(sx(gx-0.5),0); cx.lineTo(sx(gx-0.5),H); }
    var z0=Math.floor((ctrz-H/2/zoom)/10)*10, z1=ctrz+H/2/zoom;
    for(var gz=z0; gz<=z1; gz+=10){ cx.moveTo(0,sz(gz-0.5)); cx.lineTo(W,sz(gz-0.5)); }
    cx.stroke();
  }
  var cs=Math.max(1,zoom);
  if(MAPD){
    cx.fillStyle='#2e7d4f';
    var v=MAPD.v; for(var i=0;i<v.length;i+=2){ cx.fillRect(sx(v[i]-0.5), sz(v[i+1]-0.5), cs, cs); }
    cx.fillStyle='#d9473f';
    var b=MAPD.b; for(var j=0;j<b.length;j+=2){ cx.fillRect(sx(b[j]-0.5), sz(b[j+1]-0.5), cs, cs); }
  }
  if(POS){
    var t=POS.tr;
    if(t && t.length>=4){
      cx.strokeStyle='#4dd0e1'; cx.lineWidth=Math.max(1.5,zoom*0.25); cx.beginPath();
      cx.moveTo(sx(t[0]),sz(t[1]));
      for(var k=2;k<t.length;k+=2){ cx.lineTo(sx(t[k]),sz(t[k+1])); }
      cx.stroke();
    }
    var p=POS.p;
    if(p && p.length>=4){
      cx.strokeStyle='#ffd54f'; cx.lineWidth=Math.max(2,zoom*0.35); cx.setLineDash([zoom*1.2,zoom*0.8]); cx.beginPath();
      cx.moveTo(sx(POS.px),sz(POS.pz));
      for(var m=2;m<p.length;m+=2){ cx.lineTo(sx(p[m]),sz(p[m+1])); }
      cx.stroke(); cx.setLineDash([]);
    }
    if(POS.cr){
      cx.fillStyle='#ff9800'; cx.beginPath(); cx.arc(sx(POS.cr[0]),sz(POS.cr[1]),Math.max(4,zoom*0.6),0,6.2832); cx.fill();
    }
    if(POS.tx!==null){
      var tx=sx(POS.tx), tz=sz(POS.tz);
      var inside=(tx>20&&tx<W-20&&tz>20&&tz<H-20);
      cx.fillStyle='#e040fb'; cx.strokeStyle='#e040fb'; cx.lineWidth=2;
      if(inside){
        cx.beginPath(); cx.arc(tx,tz,9,0,6.2832); cx.stroke();
        cx.beginPath(); cx.moveTo(tx-14,tz); cx.lineTo(tx+14,tz); cx.moveTo(tx,tz-14); cx.lineTo(tx,tz+14); cx.stroke();
      } else {
        var dx=tx-W/2, dz=tz-H/2, len=Math.sqrt(dx*dx+dz*dz)||1;
        var k2=Math.min((W/2-30)/Math.abs(dx||1e-9),(H/2-30)/Math.abs(dz||1e-9));
        var ex=W/2+dx*k2, ez=H/2+dz*k2, ang=Math.atan2(dz,dx);
        cx.beginPath(); cx.moveTo(ex,ez);
        cx.lineTo(ex-18*Math.cos(ang-0.45),ez-18*Math.sin(ang-0.45));
        cx.lineTo(ex-18*Math.cos(ang+0.45),ez-18*Math.sin(ang+0.45));
        cx.closePath(); cx.fill();
        cx.font='12px sans-serif'; cx.fillText('цель '+Math.round(POS.dist)+' бл.', Math.min(W-90,Math.max(8,ex-30)), Math.min(H-8,Math.max(14,ez-10)));
      }
    }
    // бот
    var px=sx(POS.px), pz=sz(POS.pz);
    if(POS.yaw!==null){
      var a=POS.yaw*Math.PI/180, L=Math.max(18,zoom*3);
      cx.strokeStyle='#ffffff'; cx.lineWidth=2.5; cx.beginPath(); cx.moveTo(px,pz); cx.lineTo(px+Math.cos(a)*L,pz+Math.sin(a)*L); cx.stroke();
    }
    cx.fillStyle=POS.wat?'#42a5f5':'#ffffff'; cx.strokeStyle='#000'; cx.lineWidth=2;
    cx.beginPath(); cx.arc(px,pz,Math.max(5,zoom*0.7),0,6.2832); cx.fill(); cx.stroke();
  }
  // HUD
  var posEl=document.getElementById('pos'), tgEl=document.getElementById('tg'), stEl=document.getElementById('st');
  if(POS){
    if(POS.t!==lastT){ lastT=POS.t; lastChange=Date.now(); }
    var stale=(Date.now()-lastChange)>3000;
    posEl.innerHTML='X: <b>'+POS.px.toFixed(0)+'</b> &nbsp; Z: <b>'+POS.pz.toFixed(0)+'</b>';
    tgEl.innerHTML=(POS.tx!==null? 'до цели: <b>'+Math.round(POS.dist)+'</b> бл. ': 'режим РОСТ ')+' &nbsp; путь: '+(POS.p?POS.p.length/2:0)+' кл.';
    var flags=[]; if(POS.fol) flags.push('обход стены'); if(POS.wat) flags.push('в воде');
    stEl.innerHTML=stale?'<span class="warn">нет данных (бот остановлен?)</span>':(flags.length?flags.join(', '):'бежит');
  }
  requestAnimationFrame(draw);
}
draw();
</script></body></html>
"####;

fn write_view_html() {
    let dir = Cfg::dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("view.html"), VIEW_HTML);
}

fn open_view() {
    let p = Cfg::dir().join("view.html");
    let _ = Command::new("cmd").arg("/C").arg("start").arg("").arg(p.as_os_str()).spawn();
}

fn push_pairs(s: &mut String, v: &[(i32, i32)]) {
    s.push('[');
    for (i, c) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&c.0.to_string());
        s.push(',');
        s.push_str(&c.1.to_string());
    }
    s.push(']');
}

fn write_atomic(name: &str, content: &str) {
    let dir = Cfg::dir();
    let _ = std::fs::create_dir_all(&dir);
    let tmp = dir.join(format!("{}.tmp", name));
    if std::fs::write(&tmp, content).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join(name));
    }
}

fn view_write_pos(
    t: u64,
    x: f64,
    z: f64,
    yaw: Option<f64>,
    tgt: Option<(f64, f64)>,
    carrot: Option<(f64, f64)>,
    follow: bool,
    water: bool,
    dist: f64,
    plan: &[(i32, i32)],
    trail: &[(i32, i32)],
) {
    let mut s = String::with_capacity(4096);
    let yaw_s = match yaw {
        Some(y) => format!("{:.1}", y),
        None => String::from("null"),
    };
    s.push_str(&format!("window.POS={{t:{},px:{:.1},pz:{:.1},yaw:{},", t, x, z, yaw_s));
    match tgt {
        Some((a, b)) => s.push_str(&format!("tx:{:.0},tz:{:.0},", a, b)),
        None => s.push_str("tx:null,tz:null,"),
    }
    match carrot {
        Some((a, b)) => s.push_str(&format!("cr:[{:.1},{:.1}],", a, b)),
        None => s.push_str("cr:null,"),
    }
    s.push_str(&format!("fol:{},wat:{},dist:{:.0},p:", follow as u8, water as u8, dist));
    push_pairs(&mut s, plan);
    s.push_str(",tr:");
    push_pairs(&mut s, trail);
    s.push_str("};");
    write_atomic("view_pos.js", &s);
}

fn view_write_map(world: &World, cx: i32, cz: i32) {
    let mut v: Vec<(i32, i32)> = Vec::new();
    let mut b: Vec<(i32, i32)> = Vec::new();
    for c in world.visited.keys() {
        if (c.0 - cx).abs() <= 80 && (c.1 - cz).abs() <= 80 {
            v.push(*c);
        }
    }
    for c in world.blocked.keys() {
        if (c.0 - cx).abs() <= 80 && (c.1 - cz).abs() <= 80 && world.is_blocked(*c, world.t) {
            b.push(*c);
        }
    }
    let mut s = String::with_capacity(v.len() * 9 + b.len() * 9 + 64);
    s.push_str(&format!("window.MAPD={{cx:{},cz:{},v:", cx, cz));
    push_pairs(&mut s, &v);
    s.push_str(",b:");
    push_pairs(&mut s, &b);
    s.push_str("};");
    write_atomic("view_map.js", &s);
}

#[repr(C)]
struct WinRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[link(name = "user32")]
extern "system" {
    #[link_name = "GetAncestor"]
    fn cb_get_ancestor(hwnd: *mut c_void, flags: u32) -> *mut c_void;
    #[link_name = "GetWindowRect"]
    fn cb_get_window_rect(hwnd: *mut c_void, r: *mut WinRect) -> i32;
    #[link_name = "SetWindowPos"]
    fn cb_set_window_pos(hwnd: *mut c_void, after: *mut c_void, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
    #[link_name = "IsWindowVisible"]
    fn cb_is_visible(hwnd: *mut c_void) -> i32;
    #[link_name = "GetCursorPos"]
    fn cb_get_cursor_pos(p: *mut CurPt) -> i32;
}

#[repr(C)]
struct CurPt {
    x: i32,
    y: i32,
}

fn cursor_pos() -> (i32, i32) {
    unsafe {
        let mut p = CurPt { x: 0, y: 0 };
        cb_get_cursor_pos(&mut p);
        (p.x, p.y)
    }
}

#[link(name = "kernel32")]
extern "system" {
    #[link_name = "GetConsoleWindow"]
    fn cb_get_console_window() -> *mut c_void;
}

/// Окно, в котором показан вывод бота (в Windows Terminal - его настоящее окно, а не скрытое псевдо-окно консоли).
fn console_root() -> *mut c_void {
    unsafe {
        let h = cb_get_console_window();
        if h.is_null() {
            return h;
        }
        let root = cb_get_ancestor(h, 3); // GA_ROOTOWNER
        if root.is_null() {
            h
        } else {
            root
        }
    }
}

fn console_rect() -> Option<(i32, i32, i32, i32)> {
    unsafe {
        let h = console_root();
        if h.is_null() || cb_is_visible(h) == 0 {
            return None;
        }
        let mut r = WinRect { left: 0, top: 0, right: 0, bottom: 0 };
        if cb_get_window_rect(h, &mut r) == 0 || r.right - r.left < 50 || r.bottom - r.top < 50 {
            return None;
        }
        Some((r.left, r.top, r.right, r.bottom))
    }
}

/// Переносит окно консоли бота в правый верхний угол: оно не должно закрывать строку координат игры.
fn move_console_away(sw: i32) -> bool {
    unsafe {
        let h = console_root();
        if h.is_null() {
            return false;
        }
        let mut r = WinRect { left: 0, top: 0, right: 0, bottom: 0 };
        if cb_get_window_rect(h, &mut r) == 0 {
            return false;
        }
        let w = r.right - r.left;
        let x = (sw - w - 10).max(0);
        // SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE
        cb_set_window_pos(h, std::ptr::null_mut(), x, 30, 0, 0, 0x0001 | 0x0004 | 0x0010) != 0
    }
}

/// Закрашивает чёрным часть снимка, которую перекрывает окно консоли бота (чтобы бот не читал свой же вывод).
fn blank_console(buf: &mut [u8], rx: i32, ry: i32, rw: i32, rh: i32) {
    if let Some((l, t, r, b)) = console_rect() {
        let x0 = (l - rx).max(0);
        let x1 = (r - rx).min(rw);
        let y0 = (t - ry).max(0);
        let y1 = (b - ry).min(rh);
        if x0 < x1 && y0 < y1 {
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = ((y * rw + x) * 4) as usize;
                    if i + 2 < buf.len() {
                        buf[i] = 0;
                        buf[i + 1] = 0;
                        buf[i + 2] = 0;
                    }
                }
            }
        }
    }
}

#[repr(C)]
struct SysTime {
    year: u16,
    month: u16,
    dow: u16,
    day: u16,
    hour: u16,
    min: u16,
    sec: u16,
    ms: u16,
}

#[link(name = "kernel32")]
extern "system" {
    fn SetThreadExecutionState(flags: u32) -> u32;
    fn GetLocalTime(t: *mut SysTime);
}

/// Не даёт ПК уснуть и не гасит экран, пока бот работает (экран нужен для чтения координат).
fn keep_awake(on: bool) {
    // ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED
    unsafe {
        SetThreadExecutionState(if on { 0x8000_0003 } else { 0x8000_0000 });
    }
}

// ============================================================================
//  Карта мира, которую бот строит сам, и A* (как в Baritone, но по опыту, а не по данным игры)
// ============================================================================
struct World {
    visited: HashMap<(i32, i32), i32>, // клетка -> высота Y, где стояли (значит, проходима)
    blocked: HashMap<(i32, i32), f64>, // клетка -> время, когда в неё упёрлись
    ver: u32,                          // растёт при изменении стен (повод перестроить путь)
    t: f64,                            // последнее известное время (для сохранения карты)
}

impl World {
    fn new() -> World {
        World { visited: HashMap::new(), blocked: HashMap::new(), ver: 0, t: 0.0 }
    }

    fn map_path() -> PathBuf {
        Cfg::dir().join("map.txt")
    }

    /// Карта, сохранённая в прошлый раз (пройденные клетки и стены). Загруженные стены - постоянные.
    fn load() -> World {
        let mut w = World::new();
        if let Ok(t) = std::fs::read_to_string(World::map_path()) {
            for line in t.lines() {
                let p: Vec<&str> = line.split(',').collect();
                if p.len() >= 3 {
                    if let (Ok(x), Ok(z)) = (p[1].parse::<i32>(), p[2].parse::<i32>()) {
                        if p[0] == "v" && p.len() >= 4 {
                            if let Ok(y) = p[3].parse::<i32>() {
                                w.visited.insert((x, z), y);
                            }
                        } else if p[0] == "b" {
                            w.blocked.insert((x, z), 1.0e12);
                        }
                    }
                }
            }
        }
        w
    }

    fn save(&self, now: f64) {
        if self.visited.len() > 600_000 {
            return;
        }
        let mut s = String::with_capacity(self.visited.len() * 16);
        for (c, y) in &self.visited {
            s.push_str(&format!("v,{},{},{}\n", c.0, c.1, y));
        }
        for (c, &t) in &self.blocked {
            if now - t < 1800.0 {
                s.push_str(&format!("b,{},{}\n", c.0, c.1));
            }
        }
        let _ = std::fs::create_dir_all(Cfg::dir());
        let _ = std::fs::write(World::map_path(), s);
    }

    fn visit(&mut self, c: (i32, i32), y: i32) {
        self.visited.insert(c, y);
        if self.blocked.remove(&c).is_some() {
            self.ver += 1;
        }
    }

    fn is_blocked(&self, c: (i32, i32), now: f64) -> bool {
        if self.visited.contains_key(&c) {
            return false;
        }
        match self.blocked.get(&c) {
            Some(&t) => now - t < 1800.0, // свежие отметки "стена" живут 30 минут (вдруг это был игрок)
            None => false,
        }
    }

    /// Упёрлись, двигаясь курсом heading_deg из клетки (x, z): помечаем полосу стены впереди.
    fn mark_bump(&mut self, x: f64, z: f64, heading_deg: f64, now: f64, half: i32) {
        let h = heading_deg.to_radians();
        let (ux, uz) = (h.cos(), h.sin());
        let (px, pz) = (-uz, ux);
        let own = (x.round() as i32, z.round() as i32);
        for d in [1.0f64, 2.0] {
            for k in -half..=half {
                let cx = x + ux * d + px * k as f64;
                let cz = z + uz * d + pz * k as f64;
                let c = (cx.round() as i32, cz.round() as i32);
                if c == own || self.visited.contains_key(&c) {
                    continue;
                }
                self.blocked.insert(c, now);
            }
        }
        self.ver += 1;
    }

    fn line_free(&self, a: (f64, f64), b: (f64, f64), now: f64) -> bool {
        let d = hyp(b.0 - a.0, b.1 - a.1);
        let n = (d * 2.0).ceil() as i32;
        for i in 1..=n {
            let t = i as f64 / n as f64;
            let c = ((a.0 + (b.0 - a.0) * t).round() as i32, (a.1 + (b.1 - a.1) * t).round() as i32);
            if self.is_blocked(c, now) {
                return false;
            }
        }
        true
    }

    /// A* по клеткам 1x1 (8 направлений). Неизвестные клетки считаются проходимыми, известные
    /// стены - нет, уже пройденные чуть дешевле, у стен небольшой штраф. Если до цели не дойти -
    /// путь до ближайшей к ней достижимой клетки.
    fn plan(&self, from: (i32, i32), goal: (i32, i32), now: f64) -> Vec<(i32, i32)> {
        const R: i32 = 90;
        const MAXN: usize = 15000;
        let hfun = |c: (i32, i32)| -> f64 {
            let dx = (c.0 - goal.0).abs() as f64;
            let dz = (c.1 - goal.1).abs() as f64;
            (dx + dz) + (1.4142 - 2.0) * dx.min(dz)
        };
        let key = |f: f64| (f * 1000.0) as i64;
        let mut g: HashMap<(i32, i32), f64> = HashMap::new();
        let mut parent: HashMap<(i32, i32), (i32, i32)> = HashMap::new();
        let mut closed: HashSet<(i32, i32)> = HashSet::new();
        let mut open: BinaryHeap<Reverse<(i64, (i32, i32))>> = BinaryHeap::new();
        g.insert(from, 0.0);
        open.push(Reverse((key(hfun(from)), from)));
        let mut best = from;
        let mut best_h = hfun(from);
        let mut n = 0usize;
        while let Some(Reverse((_, c))) = open.pop() {
            if closed.contains(&c) {
                continue;
            }
            closed.insert(c);
            n += 1;
            let hc = hfun(c);
            if hc < best_h {
                best_h = hc;
                best = c;
            }
            if hc < 1.5 || n > MAXN {
                break;
            }
            let gc = g[&c];
            for dx in -1..=1i32 {
                for dz in -1..=1i32 {
                    if dx == 0 && dz == 0 {
                        continue;
                    }
                    let nb = (c.0 + dx, c.1 + dz);
                    if (nb.0 - from.0).abs() > R || (nb.1 - from.1).abs() > R {
                        continue;
                    }
                    if closed.contains(&nb) || self.is_blocked(nb, now) {
                        continue;
                    }
                    if dx != 0 && dz != 0 && (self.is_blocked((c.0 + dx, c.1), now) || self.is_blocked((c.0, c.1 + dz), now)) {
                        continue; // не режем углы
                    }
                    if let (Some(&yc), Some(&yn)) = (self.visited.get(&c), self.visited.get(&nb)) {
                        let dy = yn - yc;
                        if dy >= 2 || dy <= -4 {
                            continue; // слишком высокий уступ / обрыв
                        }
                    }
                    let mut step = if dx != 0 && dz != 0 { 1.4142 } else { 1.0 };
                    if self.visited.contains_key(&nb) {
                        step *= 0.85;
                    }
                    if self.is_blocked((nb.0 + 1, nb.1), now)
                        || self.is_blocked((nb.0 - 1, nb.1), now)
                        || self.is_blocked((nb.0, nb.1 + 1), now)
                        || self.is_blocked((nb.0, nb.1 - 1), now)
                    {
                        step += 0.6;
                    }
                    let ng = gc + step;
                    if ng < *g.get(&nb).unwrap_or(&f64::MAX) {
                        g.insert(nb, ng);
                        parent.insert(nb, c);
                        open.push(Reverse((key(ng + hfun(nb)), nb))); // эвристика с весом 1: быстрее, путь почти оптимальный
                    }
                }
            }
        }
        let mut path = vec![best];
        let mut cur = best;
        while let Some(&pr) = parent.get(&cur) {
            path.push(pr);
            cur = pr;
            if path.len() > 20000 {
                break;
            }
        }
        path.reverse();
        path
    }

    /// Точка, на которую рулим: самая дальняя точка пути (до ~12 клеток вперёд), до которой
    /// можно дойти по прямой, не задев известных стен.
    fn carrot(&self, from_f: (f64, f64), path: &[(i32, i32)], now: f64) -> Option<(f64, f64)> {
        if path.len() < 2 {
            return None;
        }
        let mut ni = 0usize;
        let mut nd = f64::MAX;
        for (i, c) in path.iter().enumerate() {
            let d = hyp(c.0 as f64 - from_f.0, c.1 as f64 - from_f.1);
            if d < nd {
                nd = d;
                ni = i;
            }
        }
        let end = (ni + 12).min(path.len() - 1);
        let mut i = end;
        while i > ni {
            let c = path[i];
            let cf = (c.0 as f64, c.1 as f64);
            if self.line_free(from_f, cf, now) {
                return Some(cf);
            }
            i -= 1;
        }
        let c = path[(ni + 1).min(path.len() - 1)];
        Some((c.0 as f64, c.1 as f64))
    }
}

impl Drop for World {
    fn drop(&mut self) {
        // карта сохраняется при любом выходе из режима бега (стоп, прибытие, ошибка)
        self.save(self.t);
    }
}

struct Stop;
type Res<T> = Result<T, Stop>;

struct App {
    cfg: Cfg,
    reader: Reader,
    water_cap: Capture,
    prev: [bool; 8],
    yaw: Option<f64>,
    ai_test_req: bool,
    carrot: Option<(f64, f64)>,
    last_y: i32,
    swimming: bool,
    t0: Instant,
    sw: i32,
    sh: i32,
    rng: Rng,
    rx: Receiver<String>,
}

impl App {
    fn new(cfg: Cfg, rx: Receiver<String>) -> App {
        let (sw, sh) = unsafe { (GetSystemMetrics(0), GetSystemMetrics(1)) };
        let reader = Reader::new(sw, sh, cfg.region, cfg.locked);
        let water_cap = Capture::new((sw as f64 * 0.4) as i32, (sh as f64 * 0.25) as i32);
        App { cfg, reader, water_cap, prev: [false; 8], yaw: None, ai_test_req: false, carrot: None, last_y: 0, swimming: false, t0: Instant::now(), sw, sh, rng: Rng::new(), rx }
    }

    fn now(&self) -> f64 {
        self.t0.elapsed().as_secs_f64()
    }

    fn pressed(&mut self, i: usize) -> bool {
        let d = hk_down(HOTKEYS[i]);
        let r = d && !self.prev[i];
        self.prev[i] = d;
        r
    }

    /// Команды из консоли: go X Z | grow | radius N | status
    fn poll_cmds(&mut self) {
        while let Ok(line) = self.rx.try_recv() {
            let line = line.replace(',', " ").replace(';', " ");
            let w: Vec<&str> = line.split_whitespace().collect();
            if w.is_empty() {
                continue;
            }
            match w[0].to_lowercase().as_str() {
                "go" | "target" | "t" | "цель" | "иди" if w.len() >= 3 => {
                    match (w[1].parse::<f64>(), w[2].parse::<f64>()) {
                        (Ok(x), Ok(z)) => {
                            self.cfg.tx = x;
                            self.cfg.tz = z;
                            self.cfg.mode_target = true;
                            self.cfg.save();
                            println!("\nНовая цель: X={} Z={} (режим К ТОЧКЕ). Старт: F8.", x, z);
                        }
                        _ => println!("\nНе понял числа. Пример: go 1500 -2000"),
                    }
                }
                "grow" | "рост" => {
                    self.cfg.mode_target = false;
                    self.cfg.save();
                    println!("\nРежим: РОСТ.");
                }
                "radius" | "r" if w.len() >= 2 => {
                    if let Ok(r) = w[1].parse::<f64>() {
                        self.cfg.radius = r.max(1.0);
                        self.cfg.save();
                        println!("\nРадиус прибытия: {} блоков.", self.cfg.radius);
                    }
                }
                "view" | "карта" => {
                    open_view();
                    println!("\nКарта открыта в браузере. ВАЖНО: после этого кликни обратно в игру (клавиши бота идут в активное окно).");
                }
                "token" if w.len() >= 2 => {
                    let p = Cfg::dir().join("hf_token.txt");
                    let _ = std::fs::write(&p, w[1]);
                    println!("\nТокен сохранён в {} (только у тебя на ПК). Включить ИИ: ai on, проверить: ai test", p.display());
                }
                "ai" if w.len() >= 2 => match w[1].to_lowercase().as_str() {
                    "on" | "вкл" => {
                        self.cfg.ai = true;
                        self.cfg.save();
                        println!("\nИИ-помощник включён (модель {}).", self.cfg.ai_model);
                    }
                    "off" | "выкл" => {
                        self.cfg.ai = false;
                        self.cfg.save();
                        println!("\nИИ-помощник выключен.");
                    }
                    "test" => self.ai_test_req = true,
                    "gemini" => {
                        self.cfg.ai_url = String::from("https://generativelanguage.googleapis.com/v1beta/openai/chat/completions");
                        self.cfg.ai_model = String::from("gemini-3.5-flash-lite");
                        self.cfg.save();
                        println!("\nПресет Google Gemini: модель {}. Ключ из aistudio.google.com: token ВАШ_КЛЮЧ, потом ai test. Другая модель: model ИМЯ", self.cfg.ai_model);
                    }
                    "hf" => {
                        self.cfg.ai_url = String::from("https://router.huggingface.co/v1/chat/completions");
                        self.cfg.ai_model = String::from("Qwen/Qwen2.5-VL-72B-Instruct");
                        self.cfg.save();
                        println!("\nПресет Hugging Face (нужны кредиты на аккаунте).");
                    }
                    "url" => {
                        if w.len() >= 3 {
                            self.cfg.ai_url = w[2].to_string();
                            self.cfg.save();
                            println!("\nАдрес API: {}", self.cfg.ai_url);
                        } else {
                            println!("\nТекущий адрес API: {}", self.cfg.ai_url);
                        }
                    }
                    _ => println!("\nai on | ai off | ai test | ai gemini | ai hf | ai url АДРЕС"),
                },
                "model" if w.len() >= 2 => {
                    self.cfg.ai_model = w[1].to_string();
                    self.cfg.save();
                    println!("\nМодель ИИ: {}", self.cfg.ai_model);
                }
                "status" | "s" => {
                    println!(
                        "\nРежим: {}. Цель X={} Z={}. Радиус {}.",
                        if self.cfg.mode_target { "К ТОЧКЕ" } else { "РОСТ" },
                        self.cfg.tx,
                        self.cfg.tz,
                        self.cfg.radius
                    );
                }
                _ => println!("\nКоманды: go X Z | grow | radius N | status"),
            }
        }
    }

    fn should_stop(&mut self) -> bool {
        self.poll_cmds();
        let a = self.pressed(HK_STOP);
        let b = self.pressed(HK_TOGGLE);
        a || b
    }

    /// Пауза с проверкой стоп-клавиш каждые 5 мс.
    fn wait(&mut self, ms: u64) -> Res<()> {
        let end = Instant::now() + Duration::from_millis(ms);
        loop {
            if self.should_stop() {
                return Err(Stop);
            }
            let now = Instant::now();
            if now >= end {
                return Ok(());
            }
            let left = end - now;
            sleep(left.min(Duration::from_millis(5)));
        }
    }

    fn read_xz(&mut self) -> Option<(f64, f64)> {
        let raw = self.reader.read();
        if let Some((_, y, _)) = raw {
            self.last_y = y;
        }
        let r = raw.map(|(x, _y, z)| (x as f64, z as f64));
        if self.reader.fresh {
            // строка координат найдена и зафиксирована - запоминаем, чтобы в следующий раз начать сразу с неё
            self.reader.fresh = false;
            self.cfg.locked = Some(self.reader.lock);
            self.cfg.save();
        }
        r
    }

    /// Два чтения подряд почти совпадают (стоим на месте) - можно верить.
    fn read_stable(&mut self) -> Res<Option<(f64, f64)>> {
        let mut prev: Option<(f64, f64)> = None;
        for _ in 0..12 {
            if let Some(p) = self.read_xz() {
                if let Some(q) = prev {
                    if hyp(p.0 - q.0, p.1 - q.1) <= 3.0 {
                        return Ok(Some(p));
                    }
                }
                prev = Some(p);
            }
            self.wait(40)?;
        }
        Ok(None)
    }

    fn release_all(&mut self) {
        for &k in [SC_W, SC_A, SC_S, SC_D, SC_SPACE].iter() {
            ku(k);
        }
        mouse_button(false);
        self.swimming = false;
    }

    /// Средний цвет центра экрана, доля "синих" пикселей, доля пикселей цвета воды (если цвет запомнен по Delete).
    fn water_stats(&mut self) -> Option<(f64, f64, f64, f64, f64)> {
        let x = (self.sw as f64 * 0.3) as i32;
        let y = (self.sh as f64 * 0.45) as i32;
        if !self.water_cap.grab(x, y) {
            return None;
        }
        let w = self.water_cap.w as usize;
        let h = self.water_cap.h as usize;
        let wref = self.cfg.water;
        let (mut sr, mut sg, mut sb) = (0f64, 0f64, 0f64);
        let (mut total, mut blue, mut near) = (0u32, 0u32, 0u32);
        let mut yy = 0usize;
        while yy < h {
            let mut xx = 0usize;
            while xx < w {
                let i = (yy * w + xx) * 4;
                let b = self.water_cap.buf[i] as i32;
                let g = self.water_cap.buf[i + 1] as i32;
                let r = self.water_cap.buf[i + 2] as i32;
                sr += r as f64;
                sg += g as f64;
                sb += b as f64;
                total += 1;
                if b > r + 40 && b > 90 {
                    blue += 1;
                }
                if let Some((wr, wg, wb)) = wref {
                    if (r - wr).abs() <= 60 && (g - wg).abs() <= 60 && (b - wb).abs() <= 60 {
                        near += 1;
                    }
                }
                xx += 12;
            }
            yy += 12;
        }
        if total == 0 {
            return None;
        }
        let t = total as f64;
        Some((sr / t, sg / t, sb / t, blue as f64 / t, near as f64 / t))
    }

    fn in_water(&mut self) -> bool {
        match self.water_stats() {
            Some((_, _, _, fb, fr)) => {
                if self.cfg.water.is_some() {
                    fr > 0.55
                } else {
                    fb > 0.5
                }
            }
            None => false,
        }
    }

    /// Delete: стоишь В ВОДЕ (экран залит водой) - бот запоминает её цвет.
    fn water_cal(&mut self) {
        match self.water_stats() {
            Some((r, g, b, _, _)) => {
                self.cfg.water = Some((r.round() as i32, g.round() as i32, b.round() as i32));
                self.cfg.save();
                println!("Цвет воды запомнен: R={:.0} G={:.0} B={:.0}. (Сбросить: cube_bot.exe --clear-water)", r, g, b);
            }
            None => println!("Не удалось снять экран."),
        }
    }

    // ---------- проверка чтения ----------
    /// Что видит бот на всём экране: уменьшенный снимок и левый нижний угол в полном размере (консоль закрашена).
    fn save_screens(&mut self) {
        let (sw, sh) = (self.sw, self.sh);
        let mut cap = Capture::new(sw, sh);
        if !cap.grab(0, 0) {
            return;
        }
        blank_console(&mut cap.buf, 0, 0, sw, sh);
        let (swu, shu) = (sw as usize, sh as usize);
        let ow = 1280usize.min(swu);
        let oh = (shu * ow / swu).max(1);
        let mut rgb: Vec<u8> = Vec::with_capacity(ow * oh * 3);
        for oy in 0..oh {
            let sy = oy * shu / oh;
            for ox in 0..ow {
                let sx = ox * swu / ow;
                let i = (sy * swu + sx) * 4;
                rgb.push(cap.buf[i + 2]);
                rgb.push(cap.buf[i + 1]);
                rgb.push(cap.buf[i]);
            }
        }
        let dir = Cfg::dir();
        let _ = std::fs::write(dir.join("screen_small.png"), ai::png_rgb(ow, oh, &rgb));
        let cw = 800usize.min(swu);
        let ch = 360usize.min(shu);
        let y0 = shu - ch;
        let mut rgb2: Vec<u8> = Vec::with_capacity(cw * ch * 3);
        for y in y0..shu {
            for x in 0..cw {
                let i = (y * swu + x) * 4;
                rgb2.push(cap.buf[i + 2]);
                rgb2.push(cap.buf[i + 1]);
                rgb2.push(cap.buf[i]);
            }
        }
        let _ = std::fs::write(dir.join("hud_corner.png"), ai::png_rgb(cw, ch, &rgb2));
        println!("  Что бот видит на экране (консоль закрашена): {}", dir.join("screen_small.png").display());
        println!("  Левый нижний угол в полном размере: {}", dir.join("hud_corner.png").display());
    }

    /// Insert: курсор наведён на строку координат в игре - запоминаем область.
    fn pick_hud(&mut self) {
        let (cx, cy) = cursor_pos();
        let x = (cx - 80).max(0);
        let y = (cy - 45).max(0);
        let w = 520i32.min(self.sw - x).max(1);
        let h = 90i32.min(self.sh - y).max(1);
        self.cfg.region = Some((x, y, w, h));
        self.cfg.locked = None;
        self.cfg.save();
        self.reader = Reader::new(self.sw, self.sh, self.cfg.region, None);
        println!("Область координат задана вручную: x={} y={} ширина={} высота={}. Проверяю чтение...", x, y, w, h);
        self.read_test();
    }

    fn read_test(&mut self) {
        self.reader.rec.verbose = true;
        self.reader.rec.dbg.clear();
        let n = 100;
        let mut ok = 0;
        let mut last: Option<(i32, i32, i32)> = None;
        let t = Instant::now();
        for _ in 0..n {
            if let Some(p) = self.reader.read() {
                ok += 1;
                last = Some(p);
            }
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
        self.reader.rec.verbose = false;
        match last {
            Some((x, y, z)) => println!(
                "Читаю: X={} Y={} Z={}  (сверь с цифрами в игре!)  {}/{} чтений успешно, {:.2} мс на чтение (~{:.0} раз/сек)",
                x,
                y,
                z,
                ok,
                n,
                ms,
                1000.0 / ms.max(0.001)
            ),
            None => {
                println!("НЕ ЧИТАЮ координаты. Видны ли они на экране (левый нижний угол, строка X: Y: Z:)?");
                for l in self.reader.rec.dbg.iter() {
                    println!("  что увидел: {}", l);
                }
                println!("  Окно консоли бота закрашивается на снимке (бот не читает свой вывод). Если игра не видна на экране - сверни другие окна.");
                println!("  Что бот видит в области (# светлый пиксель; пришли скриншот, если не читает):");
                print!("{}", self.reader.rec.dump());
                self.save_screens();
            }
        }
        let p = Cfg::dir().join("region.bmp");
        save_bmp(&p, self.reader.cap.w as usize, self.reader.cap.h as usize, &self.reader.cap.buf);
        println!("  Снимок области сохранён: {}", p.display());
    }

    // ---------- тест клавиш ----------
    fn key_test(&mut self) {
        println!("Тест: через 1.5 сек бег вперёд 2 сек, потом камера повернётся вправо. Игра должна быть в фокусе.");
        let res = self.key_test_inner();
        self.release_all();
        if res.is_ok() {
            println!("Если бежал, но камера не повернулась - кликни в игру (курсор должен пропасть).");
        }
    }

    fn key_test_inner(&mut self) -> Res<()> {
        self.wait(1500)?;
        kd(SC_W);
        self.wait(2000)?;
        ku(SC_W);
        mouse_move(300);
        Ok(())
    }

    // ---------- калибровка ----------
    fn run_forward(&mut self, secs: f64) -> Res<Vec<(f64, f64, f64)>> {
        let mut v: Vec<(f64, f64, f64)> = Vec::new();
        let t0 = self.now();
        kd(SC_W);
        let mut res: Res<()> = Ok(());
        while self.now() - t0 < secs {
            if let Some((x, z)) = self.read_xz() {
                let t = self.now();
                v.push((t, x, z));
            }
            if let Err(e) = self.wait(4) {
                res = Err(e);
                break;
            }
        }
        ku(SC_W);
        res?;
        Ok(v)
    }

    fn measure_heading(&mut self) -> Res<Option<f64>> {
        if self.read_stable()?.is_none() {
            return Ok(None);
        }
        let samples = self.run_forward(1.4)?;
        self.wait(300)?;
        if samples.is_empty() {
            return Ok(None);
        }
        let t0 = samples[0].0;
        let used: Vec<(f64, f64, f64)> = samples.iter().filter(|s| s.0 - t0 >= 0.3).cloned().collect();
        if used.len() < 5 {
            return Ok(None);
        }
        let first = used[0];
        let last = used[used.len() - 1];
        if hyp(last.1 - first.1, last.2 - first.2) < 3.0 {
            return Ok(None);
        }
        let (vx, vz) = velocity(&used);
        Ok(Some(vz.atan2(vx).to_degrees()))
    }

    fn calibrate_inner(&mut self) -> Res<bool> {
        println!("Калибровка: бот пробежит ~15 блоков вперёд с поворотами камеры. Нужно свободное место.");
        self.wait(1200)?;
        let h0 = self.measure_heading()?;
        mouse_move(250);
        self.wait(300)?;
        let h1 = self.measure_heading()?;
        mouse_move(-500);
        self.wait(300)?;
        let h2 = self.measure_heading()?;
        let (h0, h1, h2) = match (h0, h1, h2) {
            (Some(a), Some(b), Some(c)) => (a, b, c),
            _ => {
                println!("Калибровка не удалась: не видно движения или не читаются координаты (Shift+F2, F2).");
                return Ok(false);
            }
        };
        let k1 = adiff(h1, h0) / 250.0;
        let k2 = adiff(h1, h2) / 500.0;
        let ratio = if k2 != 0.0 { (k1 / k2).abs() } else { 0.0 };
        if !(k1 * k2 > 0.0 && ratio > 0.4 && ratio < 2.5 && k1.abs() > 0.01) {
            println!(
                "Калибровка не удалась (k1={:.3}, k2={:.3}). Упёрся в препятствие? Повтори на открытом месте (F10).",
                k1, k2
            );
            return Ok(false);
        }
        self.cfg.kpd = (k1 + k2) / 2.0;
        self.cfg.calibrated = true;
        self.cfg.save();
        println!("Калибровка готова: {:.3} град на пиксель мыши.", self.cfg.kpd);
        beep(1200, 100);
        Ok(true)
    }

    fn calibrate_cmd(&mut self) {
        let r = self.calibrate_inner();
        self.release_all();
        if r.is_err() {
            println!("Остановлено.");
        }
    }

    // ---------- ИИ-помощник ----------
    /// Снимок игрового окна (без рекламы справа), уменьшенный до 512 px по ширине, в PNG.
    fn snapshot_png(&mut self) -> Vec<u8> {
        let gx = 0i32;
        let gy = (self.sh as f64 * 0.09) as i32;
        let gw = ((self.sw as f64 * 0.87) as i32).max(16);
        let gh = ((self.sh as f64 * 0.80) as i32).max(16);
        let mut cap = Capture::new(gw, gh);
        if !cap.grab(gx, gy) {
            return Vec::new();
        }
        blank_console(&mut cap.buf, gx, gy, gw, gh);
        let ow = 512usize;
        let oh = ((gh as usize) * ow / (gw as usize)).max(1);
        let mut rgb: Vec<u8> = Vec::with_capacity(ow * oh * 3);
        for oy in 0..oh {
            let sy = oy * (gh as usize) / oh;
            for ox in 0..ow {
                let sx = ox * (gw as usize) / ow;
                let i = (sy * (gw as usize) + sx) * 4;
                rgb.push(cap.buf[i + 2]);
                rgb.push(cap.buf[i + 1]);
                rgb.push(cap.buf[i]);
            }
        }
        ai::png_rgb(ow, oh, &rgb)
    }

    /// Стоит на месте, смотрит на цель, снимает экран и спрашивает ИИ. Возвращает угол (+ вправо).
    fn ai_consult(&mut self, x: f64, z: f64) -> Res<Option<f64>> {
        let token = match ai::load_token(&Cfg::dir()) {
            Some(t) => t,
            None => {
                println!("\nИИ: нет токена. Введи в консоли:  token hf_xxxxxxxx  (или переменная HF_TOKEN). ИИ выключен.");
                self.cfg.ai = false;
                self.cfg.save();
                return Ok(None);
            }
        };
        ku(SC_W);
        println!("\nИИ: остановился, смотрю на цель и делаю снимок...");
        let d = match self.yaw {
            Some(y) => adiff(self.goal_heading(x, z), y),
            None => 0.0,
        };
        self.turn(d);
        self.wait(350)?;
        let png = self.snapshot_png();
        self.turn(-d);
        if png.is_empty() {
            println!("ИИ: не удалось снять экран.");
            return Ok(None);
        }
        let _ = std::fs::write(Cfg::dir().join("ai_last.png"), &png);
        let (tx, rx) = channel::<String>();
        ai::ask(Cfg::dir(), token, self.cfg.ai_url.clone(), self.cfg.ai_model.clone(), png, ai::PROMPT.to_string(), tx);
        println!("ИИ: запрос отправлен ({}), жду ответ...", self.cfg.ai_model);
        let t0 = Instant::now();
        let reply = loop {
            match rx.try_recv() {
                Ok(r) => break r,
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => break String::from("ERR:поток запроса завершился"),
            }
            if t0.elapsed() > Duration::from_secs(75) {
                break String::from("ERR:таймаут 75 с");
            }
            self.wait(50)?;
        };
        if let Some(text) = reply.strip_prefix("OK:") {
            let short: String = text.chars().take(120).collect();
            match ai::parse_turn(text) {
                Some(a) => {
                    println!("ИИ ответил: {} -> поворот {:.0} град (+ вправо)", short.trim(), a);
                    Ok(Some(a))
                }
                None => {
                    println!("ИИ ответил, но без TURN=...: {}", short.trim());
                    Ok(None)
                }
            }
        } else {
            let msg = reply.strip_prefix("ERR:").unwrap_or(&reply);
            let short: String = msg.chars().take(300).collect();
            println!("ИИ: ошибка: {}", short);
            self.log(&format!("ИИ ошибка: {}", short));
            let low = short.to_lowercase();
            if low.contains("credits") || low.contains("quota") || low.contains("402") || low.contains("billing") {
                println!("ИИ: на этом сервисе кончились бесплатные лимиты. Попробуй другой: в консоли  ai gemini  (Google, есть бесплатный тариф), затем  token КЛЮЧ  и  ai test.");
            }
            Ok(None)
        }
    }

    fn ai_test(&mut self) {
        self.yaw = None;
        println!("Тест ИИ: через 3 с снимок экрана (игра должна быть на экране, не в консоли).");
        let _ = self.ai_test_inner();
        println!("Снимок, который ушёл ИИ: {}", Cfg::dir().join("ai_last.png").display());
    }

    fn ai_test_inner(&mut self) -> Res<()> {
        self.wait(3000)?;
        match self.ai_consult(0.0, 0.0)? {
            Some(a) => println!("ИИ советует повернуть на {:.0} град.", a),
            None => println!("ИИ не дал угла."),
        }
        Ok(())
    }

    // ---------- обход препятствий ----------
    /// Поворот камеры на deg градусов. Заодно обновляет собственную оценку курса (yaw).
    fn turn(&mut self, deg: f64) {
        let px = (deg / self.cfg.kpd).round().clamp(-3000.0, 3000.0) as i32;
        let mut rem = px;
        while rem != 0 {
            let step = rem.clamp(-250, 250);
            mouse_move(step);
            rem -= step;
            if rem != 0 {
                sleep(Duration::from_millis(3));
            }
        }
        if let Some(y) = self.yaw {
            self.yaw = Some(norm_deg(y + px as f64 * self.cfg.kpd));
        }
    }

    fn unstick(&mut self, level: u32, sign: i32, water: bool) -> Res<()> {
        let s: f64 = (if level >= 4 && level % 2 == 0 { -sign } else { sign }) as f64;
        let ang = match level {
            1 => 0.0,
            2 => 55.0,
            3 => 100.0,
            _ => 140.0,
        };
        if water {
            // в воде пробел и W остаются зажатыми, не пятимся
            kd(SC_W);
            kd(SC_SPACE);
            if level == 1 {
                let side = if s > 0.0 { SC_D } else { SC_A };
                kd(side);
                self.wait(600)?;
                ku(side);
            } else {
                if level >= 3 && self.cfg.break_blocks {
                    mouse_button(true);
                    self.wait(1200)?;
                    mouse_button(false);
                }
                self.turn(s * ang);
                self.wait(900)?;
            }
            self.swimming = true;
            return Ok(());
        }
        if level == 1 {
            // прыжок (пробел зажимаем, а не двойное нажатие - иначе включится полёт)
            kd(SC_SPACE);
            self.wait(700)?;
            ku(SC_SPACE);
        } else if level == 2 {
            ku(SC_W);
            kd(SC_S);
            self.wait(400)?;
            ku(SC_S);
            let side = if s > 0.0 { SC_D } else { SC_A };
            kd(side);
            kd(SC_SPACE);
            self.wait(600)?;
            ku(side);
            ku(SC_SPACE);
            self.turn(s * ang);
            kd(SC_W);
        } else {
            ku(SC_W);
            if self.cfg.break_blocks {
                mouse_button(true);
                self.wait(1500)?;
                mouse_button(false);
            }
            kd(SC_S);
            self.wait(if level >= 4 { 700 } else { 500 })?;
            ku(SC_S);
            self.turn(s * ang);
            kd(SC_W);
            kd(SC_SPACE);
            self.wait(800)?;
            ku(SC_SPACE);
        }
        self.swimming = false;
        Ok(())
    }

    /// Куда рулим сейчас: на ближайшую точку пути A* (если есть), иначе прямо на цель.
    fn want_heading(&self, x: f64, z: f64) -> f64 {
        if let Some((gx, gz)) = self.carrot {
            if hyp(gx - x, gz - z) > 0.5 {
                return (gz - z).atan2(gx - x).to_degrees();
            }
        }
        self.goal_heading(x, z)
    }

    /// Истинное направление на цель (без учёта обходов).
    fn goal_heading(&self, x: f64, z: f64) -> f64 {
        if self.cfg.mode_target {
            (self.cfg.tz - z).atan2(self.cfg.tx - x).to_degrees()
        } else {
            GROW.1.atan2(GROW.0).to_degrees()
        }
    }

    /// Промежуточная цель для A*: сама цель, а если далеко - точка в 70 блоках по направлению к ней.
    fn plan_goal(&self, x: f64, z: f64) -> (i32, i32) {
        let (tx, tz) = if self.cfg.mode_target {
            (self.cfg.tx, self.cfg.tz)
        } else {
            let n = hyp(GROW.0, GROW.1);
            (x + 70.0 * GROW.0 / n, z + 70.0 * GROW.1 / n)
        };
        let d = hyp(tx - x, tz - z);
        if d <= 70.0 {
            (tx.round() as i32, tz.round() as i32)
        } else {
            ((x + (tx - x) / d * 70.0).round() as i32, (z + (tz - z) / d * 70.0).round() as i32)
        }
    }

    // ---------- главный цикл ----------
    /// Журнал событий для утра: %APPDATA%\cube_bot\log.txt
    fn log(&self, msg: &str) {
        let mut t = SysTime { year: 0, month: 0, dow: 0, day: 0, hour: 0, min: 0, sec: 0, ms: 0 };
        unsafe {
            GetLocalTime(&mut t);
        }
        let line = format!("{:02}.{:02} {:02}:{:02}:{:02}  {}\n", t.day, t.month, t.hour, t.min, t.sec, msg);
        let _ = std::fs::create_dir_all(Cfg::dir());
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(Cfg::dir().join("log.txt")) {
            let _ = f.write_all(line.as_bytes());
        }
    }

    fn run(&mut self) {
        beep(900, 80);
        keep_awake(true);
        self.log("СТАРТ");
        let r = self.run_inner();
        self.release_all();
        keep_awake(false);
        let _ = r;
        self.log("СТОП");
        println!("\nОстановлено.");
        beep(500, 150);
    }

    fn run_inner(&mut self) -> Res<()> {
        if self.cfg.mode_target {
            println!("Старт: К ТОЧКЕ X={} Z={}. Стоп: F9 или F8.", self.cfg.tx, self.cfg.tz);
        } else {
            println!("Старт: РОСТ (X и Z растут). Стоп: F9 или F8.");
        }
        self.wait(800)?;
        let p = match self.read_stable()? {
            Some(p) => p,
            None => {
                println!("Не читаю координаты - старт отменён. Нажми Shift+F2 и смотри подсказки.");
                return Ok(());
            }
        };
        println!("Координаты читаются: X={:.0} Z={:.0}  (совпадает с экраном?)", p.0, p.1);
        if !self.cfg.calibrated {
            if !self.calibrate_inner()? {
                return Ok(());
            }
        }

        // стартовый курс: пробежать ~1.4 с, узнать, куда смотрит камера, и сразу повернуть на цель
        self.yaw = None;
        if let Some(h) = self.measure_heading()? {
            self.yaw = Some(h);
        }
        if let Some(yw) = self.yaw {
            if let Some(p2) = self.read_stable()? {
                let err = adiff(self.want_heading(p2.0, p2.1), yw);
                if err.abs() > 2.0 {
                    self.turn(err);
                    self.wait(150)?;
                }
            }
        }

        let mut hist: Vec<(f64, f64, f64)> = Vec::new();
        let mut last_good = self.read_xz().unwrap_or(p);
        let mut last_t = self.now();
        let mut jump_cand: Option<(f64, f64)> = None;
        let mut jump_n = 0u32;
        let mut jump_t0 = 0.0f64;
        let mut stuck_level = 0u32;
        let mut bad_since: Option<f64> = None;
        let mut walking = true;
        let mut water = false;
        let mut wpos = 0u32;
        let mut wneg = 0u32;
        let mut last_water = 0.0f64;
        let mut last_print = 0.0f64;
        let mut last_steer = 0.0f64;
        let mut rej_t = -10.0f64;
        let mut detour_until = 0.0f64;
        let mut bad_meas = 0u32;
        // обход преград "на ощупь": скольжение вдоль стены (W + A/D) с расширяющимся поиском края
        let mut strafe = 0i32;
        let mut follow = false; // режим следования вдоль стены
        let mut fs = 1i32; // в какую сторону вращать курс "от стены" (+1/-1)
        let mut f_t0 = 0.0f64;
        let mut slide_cell = (0i32, 0i32);
        let mut last_bump = 0.0f64;
        let mut bump_cnt = 0u32;
        let mut last_hug = 0.0f64;
        let mut last_probe = 0.0f64;
        let mut probe_until = 0.0f64;
        let mut probe_start = (0.0f64, 0.0f64);
        let mut probe_turn = 0.0f64;
        let mut pref_side = 0i32;
        let mut region_side: HashMap<(i32, i32), i32> = HashMap::new();
        let mut best_dist = f64::MAX;
        let mut best_t = self.now();
        let mut last_ai = -100.0f64;
        let mut world = World::load();
        println!("Карта: загружено {} пройденных клеток и {} стен (файл map.txt). Журнал: log.txt в папке настроек.", world.visited.len(), world.blocked.len());
        let mut last_save = self.now();
        let mut stall_n = 0u32;
        let mut trail: Vec<(i32, i32)> = Vec::new();
        let mut last_view = -100.0f64;
        let mut last_vmap = -100.0f64;
        let mut last_vn = usize::MAX;
        let mut last_vver = u32::MAX;
        let mut view_t = 0u64;
        let mut plan: Vec<(i32, i32)> = Vec::new();
        let mut plan_ver = u32::MAX;
        let mut last_plan = -100.0f64;
        let mut bump_streak = 0u32;
        let mut last_bump_t = -100.0f64;
        self.carrot = None;
        self.swimming = false;
        kd(SC_W);

        loop {
            self.wait(3)?;
            let now = self.now();

            // вода: 3 подряд "да" - входим, 10 подряд "нет" (~1 с) - выходим (без дребезга пробела)
            if self.cfg.swim && now - last_water > 0.1 {
                last_water = now;
                if self.in_water() {
                    wpos += 1;
                    wneg = 0;
                } else {
                    wneg += 1;
                    wpos = 0;
                }
                if !water && wpos >= 3 {
                    water = true;
                    println!("\nВ воде - плыву (пробел зажат).");
                } else if water && wneg >= 10 {
                    water = false;
                    println!("\nВышли из воды.");
                }
            }

            let (x, z) = match self.read_xz() {
                Some(p) => p,
                None => {
                    // реклама / меню / пауза: отпускаем всё и ждём координаты
                    let b = *bad_since.get_or_insert(now);
                    if walking && now - b > 1.5 {
                        ku(SC_W);
                        set_strafe(&mut strafe, 0);
                        if self.swimming {
                            ku(SC_SPACE);
                            self.swimming = false;
                        }
                        follow = false;
                        probe_until = 0.0;
                        walking = false;
                        println!("\nНе вижу координаты (реклама/меню?) - жду.");
                        self.log("координаты не видны (реклама/меню/отключение?) - жду");
                    }
                    if now - b > 1800.0 {
                        println!("\nКоординаты не читаются 30 минут, стоп.");
                        self.log("координаты не читались 30 минут - остановка");
                        return Ok(());
                    }
                    continue;
                }
            };
            bad_since = None;

            // фильтр ошибочных чтений: скачок быстрее физического предела принимаем,
            // только если новое значение стабильно держится >= 0.25 с (телепорт/респавн)
            let limit = (now - last_t).max(0.02) * MAX_SPEED + JUMP_SLACK;
            if hyp(x - last_good.0, z - last_good.1) > limit {
                match jump_cand {
                    Some(c) if hyp(x - c.0, z - c.1) <= 2.0 => jump_n += 1,
                    _ => {
                        jump_cand = Some((x, z));
                        jump_n = 1;
                        jump_t0 = now;
                    }
                }
                if jump_n < 4 || now - jump_t0 < 0.25 {
                    if now - rej_t > 5.0 {
                        println!("\nПропустил ошибочное чтение координат.");
                        rej_t = now;
                    }
                    continue;
                }
                println!("\nПозиция резко изменилась (телепорт/респавн) - принимаю.");
                self.log(&format!("позиция резко изменилась (смерть/респавн/телепорт): X={:.0} Z={:.0}", x, z));
                self.yaw = None; // после респавна камера могла сброситься - курс определим заново
                self.carrot = None;
                plan.clear();
                hist.clear();
                follow = false;
                        probe_until = 0.0;
                set_strafe(&mut strafe, 0);
            }
            jump_cand = None;
            jump_n = 0;
            last_good = (x, z);
            last_t = now;

            if !walking {
                kd(SC_W);
                walking = true;
                hist.clear();
                println!("\nКоординаты снова видны, бегу дальше.");
                self.log("координаты снова видны, бегу дальше");
            }

            // пробел: в воде всегда; на суше - если включён авто-прыжок (перепрыгивает ступеньки в 1 блок)
            let want_space = water || self.cfg.hop;
            if want_space != self.swimming {
                if want_space {
                    kd(SC_SPACE);
                } else {
                    ku(SC_SPACE);
                }
                self.swimming = want_space;
            }

            let target = self.cfg.mode_target;
            if target {
                let dist = hyp(self.cfg.tx - x, self.cfg.tz - z);
                if dist < self.cfg.radius {
                    println!("\nДобежали до цели! X={:.0} Z={:.0}", x, z);
                    self.log(&format!("ДОБЕЖАЛ до цели X={:.0} Z={:.0}", x, z));
                    beep(1500, 120);
                    beep(1800, 200);
                    return Ok(());
                }
                // сторож: долго нет прогресса к цели (тупик/залив) - уходим в сторону на несколько секунд
                if dist < best_dist - 3.0 {
                    best_dist = dist;
                    best_t = now;
                    stall_n = 0;
                } else if now - best_t > 60.0 && (!follow || now - f_t0 > 150.0) {
                    // сторож с нарастающей настойчивостью (чтобы можно было оставить на ночь)
                    best_t = now;
                    best_dist = dist;
                    stall_n += 1;
                    follow = false;
                    probe_until = 0.0;
                    set_strafe(&mut strafe, 0);
                    if stall_n >= 2 {
                        world.blocked.clear(); // вдруг часть отметок "стена" ошибочна
                        world.ver += 1;
                    }
                    if stall_n >= 3 {
                        last_ai = -100.0; // при следующем обходе сразу спросить ИИ (если включён)
                    }
                    let sg = self.rng.sign() as f64;
                    let ang = (100.0 + 20.0 * (stall_n.min(5) as f64 - 2.0).max(0.0)).min(180.0);
                    println!("\nНет прогресса - ухожу в сторону на {:.0} град (попытка {}).", ang, stall_n);
                    self.log(&format!("нет прогресса к цели, попытка {} (X={:.0} Z={:.0}, до цели {:.0})", stall_n, x, z, dist));
                    self.turn(sg * ang);
                    detour_until = now + 6.0 + 3.0 * stall_n.min(5) as f64;
                    hist.clear();
                }
            }

            if now - last_print > 0.25 {
                last_print = now;
                let wtxt = if water { "  ВОДА" } else { "" };
                let stxt = format!(
                    "{}{}",
                    if follow { "  ОБХОД" } else { "" },
                    if plan.len() > 1 { format!("  путь {} кл.", plan.len()) } else { String::new() }
                );
                if target {
                    print!("\rX={:.0} Z={:.0} до цели={:.0}{}{}        ", x, z, hyp(self.cfg.tx - x, self.cfg.tz - z), wtxt, stxt);
                } else {
                    print!("\rX={:.0} Z={:.0} (растут){}{}        ", x, z, wtxt, stxt);
                }
                let _ = std::io::stdout().flush();
            }

            hist.push((now, x, z));
            let cut = hist.iter().take_while(|h| now - h.0 > 2.5).count();
            if cut > 0 {
                hist.drain(0..cut);
            }

            // ---- карта и A*: запоминаем, где стояли, и раз в 1.5 с (или при новой стене) строим путь
            let cell = (x.round() as i32, z.round() as i32);
            world.visit(cell, self.last_y);
            world.t = now;
            if now - last_save > 60.0 {
                last_save = now;
                world.save(now);
            }
            if follow {
                self.carrot = None;
                last_plan = -100.0;
            } else if now - last_plan > 0.8 || world.ver != plan_ver {
                last_plan = now;
                plan_ver = world.ver;
                let (gx, gz) = self.plan_goal(x, z);
                plan = world.plan(cell, (gx, gz), now);
                self.carrot = world.carrot((x, z), &plan, now);
            }

            if trail.last() != Some(&cell) {
                trail.push(cell);
                if trail.len() > 3000 {
                    trail.drain(0..500);
                }
            }
            if self.cfg.view {
                if now - last_view > 0.5 {
                    last_view = now;
                    view_t += 1;
                    let tgt = if self.cfg.mode_target { Some((self.cfg.tx, self.cfg.tz)) } else { None };
                    let dist = if self.cfg.mode_target { hyp(self.cfg.tx - x, self.cfg.tz - z) } else { 0.0 };
                    let tstart = trail.len().saturating_sub(600);
                    view_write_pos(view_t, x, z, self.yaw, tgt, self.carrot, follow, water, dist, &plan, &trail[tstart..]);
                }
                if now - last_vmap > 4.0 && (world.visited.len() != last_vn || world.ver != last_vver) {
                    last_vmap = now;
                    last_vn = world.visited.len();
                    last_vver = world.ver;
                    view_write_map(&world, cell.0, cell.1);
                }
            }

            // ---- режим обхода: идём вдоль стены (контакт чувствуем по падению скорости)
            if follow {
                let wr = self.goal_heading(x, z).to_radians();
                let (ux, uz) = (wr.cos(), wr.sin());
                if probe_until > 0.0 {
                    // идёт проба: повернулись на цель и бежим 0.8 с - свободен ли путь
                    if now >= probe_until {
                        probe_until = 0.0;
                        let prog = (x - probe_start.0) * ux + (z - probe_start.1) * uz;
                        if prog >= 2.0 {
                            follow = false;
                            pref_side = fs;
                            region_side.insert(slide_cell, fs);
                            println!("\nПуть к цели свободен - обошёл преграду за {:.0} с.", now - f_t0);
                            self.log(&format!("обошёл преграду за {:.0} с (X={:.0} Z={:.0})", now - f_t0, x, z));
                            best_t = now;
                        } else {
                            self.turn(-probe_turn); // путь закрыт - возвращаемся вдоль стены
                            last_bump = now;
                            last_hug = now;
                            last_probe = now;
                        }
                        hist.clear();
                        hist.push((now, x, z));
                    }
                } else {
                    let i0 = hist.iter().position(|h| h.0 >= now - 0.45).unwrap_or(0);
                    let mut did = false;
                    if hist.len() - i0 >= 3 && hist[0].0 <= now - 0.45 && now - last_bump > 0.5 {
                        let (vx, vz) = velocity(&hist[i0..]);
                        let thr = if water { 0.4 } else { 1.0 };
                        if hyp(vx, vz) < thr {
                            // упёрлись: отворачиваем от стены, при повторах сильнее (35, 70, 105, 140 градусов)
                            bump_cnt = if now - last_bump < 2.5 { (bump_cnt + 1).min(4) } else { 1 };
                            self.turn(fs as f64 * 35.0 * bump_cnt as f64);
                            last_bump = now;
                            last_hug = now;
                            hist.clear();
                            hist.push((now, x, z));
                            did = true;
                        }
                    }
                    if !did {
                        if now - last_probe > 2.5 && now - last_bump > 1.0 {
                            if let Some(y) = self.yaw {
                                // раз в 2.5 с пробуем побежать прямо на цель
                                let d = adiff(self.goal_heading(x, z), y);
                                probe_turn = d;
                                self.turn(d);
                                probe_start = (x, z);
                                probe_until = now + 0.8;
                                last_probe = now;
                                hist.clear();
                                hist.push((now, x, z));
                            } else {
                                last_probe = now;
                            }
                        } else if now - last_hug > 0.7 && now - last_bump > 0.5 {
                            // прижимаемся к стене, чтобы огибать её изгибы и углы
                            self.turn(-(fs as f64) * 20.0);
                            last_hug = now;
                            hist.clear();
                            hist.push((now, x, z));
                        }
                    }
                }
            }

            // ---- ИИ-помощник: если обход затянулся - спросить совета по снимку экрана
            if self.cfg.ai && follow && probe_until == 0.0 && now - f_t0 > 10.0 && now - last_ai > 30.0 {
                let ans = self.ai_consult(x, z)?;
                if let Some(a) = ans {
                    if a.abs() >= 5.0 {
                        let sgn = if self.cfg.kpd >= 0.0 { 1.0 } else { -1.0 };
                        if let Some(y) = self.yaw {
                            let err = adiff(self.goal_heading(x, z) + sgn * a, y);
                            self.turn(err);
                        }
                        follow = false;
                        probe_until = 0.0;
                        detour_until = self.now() + 6.0;
                        println!("Иду по совету ИИ 6 секунд.");
                    }
                }
                kd(SC_W);
                last_ai = self.now();
                hist.clear();
                hist.push((self.now(), x, z));
                continue;
            }

            // ---- застряли? (в воде окно длиннее: там медленно)
            if !follow {
                let win = if water { 1.6 } else { 0.9 };
                if hist[0].0 <= now - win {
                    let i0 = hist.iter().position(|h| h.0 >= now - win).unwrap_or(0);
                    let (vx, vz) = velocity(&hist[i0..]);
                    let speed = hyp(vx, vz);
                    let min_speed = if water { STUCK_SPEED_WATER } else { STUCK_SPEED };
                    if speed < min_speed {
                        if !self.cfg.hop && !water && stuck_level == 0 {
                            // авто-прыжок выключен: сначала один прыжок (вдруг ступенька)
                            stuck_level = 1;
                            println!("\nЗастряли - пробую прыгнуть.");
                            kd(SC_SPACE);
                            self.wait(700)?;
                            ku(SC_SPACE);
                            hist.clear();
                            continue;
                        }
                        stuck_level = 0;
                        // карта: помечаем стену впереди и пробуем построить обход по A*
                        let hd = match self.yaw {
                            Some(yw) => yw,
                            None => self.goal_heading(x, z),
                        };
                        bump_streak = if now - last_bump_t < 8.0 { bump_streak + 1 } else { 1 };
                        last_bump_t = now;
                        world.mark_bump(x, z, hd, now, (2 * bump_streak.min(4)) as i32);
                        {
                            let (gx, gz) = self.plan_goal(x, z);
                            plan = world.plan(cell, (gx, gz), now);
                            plan_ver = world.ver;
                            last_plan = now;
                            self.carrot = world.carrot((x, z), &plan, now);
                        }
                        if let Some((cx, cz)) = self.carrot {
                            let bdir = (cz - z).atan2(cx - x).to_degrees();
                            if adiff(bdir, hd).abs() >= 30.0 && bump_streak < 4 {
                                println!("\nПреграда - карта строит обход (путь {} клеток, попытка {}).", plan.len(), bump_streak);
                                hist.clear();
                                hist.push((now, x, z));
                                continue;
                            }
                        }
                        if self.cfg.break_blocks {
                            mouse_button(true);
                            self.wait(1200)?;
                            mouse_button(false);
                        }
                        slide_cell = ((x / 32.0).floor() as i32, (z / 32.0).floor() as i32);
                        fs = if let Some(&sd) = region_side.get(&slide_cell) {
                            sd
                        } else if pref_side != 0 {
                            pref_side
                        } else {
                            self.rng.sign()
                        };
                        follow = true;
                        f_t0 = now;
                        last_bump = now;
                        last_hug = now;
                        last_probe = now;
                        probe_until = 0.0;
                        bump_cnt = 0;
                        self.log(&format!("преграда: иду вдоль стены (X={:.0} Z={:.0})", x, z));
                        println!("\nПреграда - иду вдоль неё, держась стены ({}).", if fs > 0 { "поворот +" } else { "поворот -" });
                        self.turn(fs as f64 * 70.0);
                        hist.clear();
                        hist.push((now, x, z));
                        continue;
                    }
                    stuck_level = 0;
                }
            }

            // ---- курс и руление раз в 0.25 с
            if now - last_steer >= 0.25 {
                last_steer = now;

                // уточняем собственную оценку курса по реальному движению (при боковом скольжении - нельзя)
                if !follow && hist.len() >= 5 && hist[0].0 <= now - 0.8 {
                    let (vx, vz) = velocity(&hist);
                    let need = if water { 1.5 } else { 3.0 };
                    if hyp(vx, vz) >= need {
                        let meas = vz.atan2(vx).to_degrees();
                        match self.yaw {
                            None => self.yaw = Some(meas),
                            Some(y) => {
                                let d = adiff(meas, y);
                                if d.abs() < 60.0 {
                                    self.yaw = Some(norm_deg(y + 0.4 * d));
                                    bad_meas = 0;
                                } else {
                                    bad_meas += 1;
                                    if bad_meas >= 6 {
                                        self.yaw = Some(meas);
                                        bad_meas = 0;
                                        println!("\nКурс расходится с камерой. Если камера не крутится - кликни внутрь игры и нажми F10.");
                                    }
                                }
                            }
                        }
                    }
                }

                if !follow && now >= detour_until {
                    if let Some(y) = self.yaw {
                        let err = adiff(self.want_heading(x, z), y);
                        if err.abs() > 2.5 {
                            let step = (err * 0.85).clamp(-90.0, 90.0);
                            self.turn(step);
                            if step.abs() > 4.0 {
                                // после заметного поворота старые точки не годятся для оценки курса
                                hist.clear();
                                hist.push((now, x, z));
                            }
                        }
                    }
                }
            }
        }
    }

    fn help(&self) {
        println!("=== Куб бот (Rust) ===");
        println!("Хоткеи (игра должна быть в фокусе):");
        println!("  F8  - старт / стоп");
        println!("  F9  - СТОП (аварийный)");
        println!("  F4  - сменить режим: РОСТ (X и Z растут) <-> К ТОЧКЕ");
        println!("  F10 - калибровка камеры");
        println!("  F2  - тест клавиш и камеры");
        println!("  Shift+F2 - проверить чтение координат (покажет скорость)");
        println!("  Delete - стоя В ВОДЕ запомнить цвет воды (точнее плавание)");
        println!("  Insert - если бот не находит координаты: Esc, наведи курсор на строку X: Y: Z: в игре, нажми Insert");
        println!("  Авто-прыжок: {} (--hop / --no-hop). Обход гор и стен: автоматически.", if self.cfg.hop { "вкл" } else { "выкл" });
        println!(
            "Режим: {}. Цель: X={} Z={}. Экран: {}x{}. Откалиброван: {}.",
            if self.cfg.mode_target { "К ТОЧКЕ" } else { "РОСТ" },
            self.cfg.tx,
            self.cfg.tz,
            self.sw,
            self.sh,
            if self.cfg.calibrated { "да" } else { "нет (при первом старте сам откалибруется)" }
        );
        println!("Карта в реальном времени: открой {} (или команда view)", Cfg::dir().join("view.html").display());
        println!("Команды в консоли (введи и Enter): go X Z  - новая цель | grow | radius N | status");
        println!("ИИ: token КЛЮЧ | ai on | ai off | ai test | ai gemini | ai hf | model ИМЯ  (сейчас: {}, {})", if self.cfg.ai { "вкл" } else { "выкл" }, self.cfg.ai_model);
        println!("Порядок: кликни внутрь игры -> Shift+F2 -> F2 -> F10 -> F8.");
    }
}

fn parse_args(cfg: &mut Cfg) -> bool {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0usize;
    let mut changed = false;
    while i < args.len() {
        match args[i].as_str() {
            "--target" if i + 2 < args.len() => {
                if let (Ok(x), Ok(z)) = (args[i + 1].parse::<f64>(), args[i + 2].parse::<f64>()) {
                    cfg.tx = x;
                    cfg.tz = z;
                    cfg.mode_target = true;
                    changed = true;
                }
                i += 2;
            }
            "--region" if i + 4 < args.len() => {
                let v: Vec<i32> = (1..=4).filter_map(|k| args[i + k].parse::<i32>().ok()).collect();
                if v.len() == 4 {
                    cfg.region = Some((v[0], v[1], v[2], v[3]));
                    changed = true;
                }
                i += 4;
            }
            "--no-view" => {
                cfg.view = false;
                changed = true;
            }
            "--view" => {
                cfg.view = true;
                changed = true;
            }
            "--ai" => {
                cfg.ai = true;
                changed = true;
            }
            "--no-ai" => {
                cfg.ai = false;
                changed = true;
            }
            "--ai-url" if i + 1 < args.len() => {
                cfg.ai_url = args[i + 1].clone();
                changed = true;
                i += 1;
            }
            "--ai-model" if i + 1 < args.len() => {
                cfg.ai_model = args[i + 1].clone();
                changed = true;
                i += 1;
            }
            "--hop" => {
                cfg.hop = true;
                changed = true;
            }
            "--no-hop" => {
                cfg.hop = false;
                changed = true;
            }
            "--clear-water" => {
                cfg.water = None;
                changed = true;
            }
            "--auto-region" => {
                cfg.region = None;
                cfg.locked = None;
                changed = true;
            }
            "--recalibrate" => {
                cfg.calibrated = false;
                changed = true;
            }
            "--swim" => {
                cfg.swim = true;
                changed = true;
            }
            "--no-swim" => {
                cfg.swim = false;
                changed = true;
            }
            "--break" => {
                cfg.break_blocks = true;
                changed = true;
            }
            "--radius" if i + 1 < args.len() => {
                if let Ok(r) = args[i + 1].parse::<f64>() {
                    cfg.radius = r.max(1.0);
                    changed = true;
                }
                i += 1;
            }
            "--grow" => {
                cfg.mode_target = false;
                changed = true;
            }
            _ => {}
        }
        i += 1;
    }
    changed
}

fn main() {
    unsafe {
        SetProcessDPIAware();
        SetConsoleOutputCP(65001);
        timeBeginPeriod(1);
    }
    let mut cfg = Cfg::load();
    if parse_args(&mut cfg) {
        cfg.save();
    }
    let (tx_ch, rx_ch) = channel::<String>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut line = String::new();
        loop {
            line.clear();
            if stdin.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            if tx_ch.send(line.trim().to_string()).is_err() {
                break;
            }
        }
    });
    write_view_html();
    let mut app = App::new(cfg, rx_ch);
    if move_console_away(app.sw) {
        println!("Окно консоли перенесено в правый верхний угол экрана (чтобы не закрывать координаты игры).");
    }
    app.help();

    loop {
        app.poll_cmds();
        if app.pressed(HK_TOGGLE) {
            app.run();
        } else if app.pressed(HK_STOP) {
            println!("Бот не запущен.");
        } else if app.pressed(HK_MODE) {
            app.cfg.mode_target = !app.cfg.mode_target;
            app.cfg.save();
            if app.cfg.mode_target {
                println!("Режим: К ТОЧКЕ (X={} Z={}). Цель меняется так: cube_bot.exe --target X Z", app.cfg.tx, app.cfg.tz);
            } else {
                println!("Режим: РОСТ (X и Z растут).");
            }
        } else if app.pressed(HK_CALIB) {
            app.calibrate_cmd();
        } else if app.pressed(HK_KEYTEST) {
            app.key_test();
        } else if app.pressed(HK_READTEST) {
            app.read_test();
        } else if app.ai_test_req {
            app.ai_test_req = false;
            app.ai_test();
        } else if app.pressed(HK_PICKHUD) {
            app.pick_hud();
        } else if app.pressed(HK_WATERCAL) {
            app.water_cal();
        }
        sleep(Duration::from_millis(15));
    }
}
