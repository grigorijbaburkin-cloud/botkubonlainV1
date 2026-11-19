// Чтение координат X Y Z прямо из пикселей экрана (без Tesseract), v5.
// Строка "X: 123 Y: 45 Z: 678" в левом нижнем углу игры. Текст чисто-белый,
// фон - тёмная плашка или светлый снег/облака; отделяем по цвету и локальному контрасту.
#![allow(dead_code)]

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pos {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Pos {
    pub fn dist_xz(&self, o: &Pos) -> f64 {
        ((self.x - o.x).powi(2) + (self.z - o.z).powi(2)).sqrt()
    }
}

// ---------- 5x7 битмап цифр 0-9 (столбцы по 5 бит, строки сверху вниз) ----------
const DIGITS: [[u8; 7]; 10] = [
    [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E], // 0
    [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E], // 1
    [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F], // 2
    [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E], // 3
    [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02], // 4
    [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E], // 5
    [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E], // 6
    [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08], // 7
    [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E], // 8
    [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C], // 9
];

struct Img<'a> {
    w: usize,
    h: usize,
    px: &'a [u8], // BGRX
}

impl<'a> Img<'a> {
    #[inline]
    fn bgr(&self, x: usize, y: usize) -> (u8, u8, u8) {
        let i = (y * self.w + x) * 4;
        (self.px[i], self.px[i + 1], self.px[i + 2])
    }
}

/// Интенсивность текста в пикселе: 0..100. Два прохода: строгий (чисто белый)
/// и мягкий (локальный контраст к окружению) - чтобы снег/облака не заливали строку.
fn text_mask(img: &Img, strict: bool) -> Vec<u8> {
    let n = img.w * img.h;
    let mut m = vec![0u8; n];
    for y in 0..img.h {
        for x in 0..img.w {
            let (b, g, r) = img.bgr(x, y);
            let mx = r.max(g).max(b) as i32;
            let mn = r.min(g).min(b) as i32;
            let sat = mx - mn;
            let white = r > 200 && g > 200 && b > 200 && sat < 40;
            if strict {
                m[y * img.w + x] = if white { 100 } else { 0 };
            } else {
                // локальный контраст: светлее соседей на ~4px?
                let mut ns = 0i32;
                let mut cnt = 0i32;
                for (dx, dy) in [(-4i32, 0), (4, 0), (0, -4), (0, 4), (-3, -3), (3, 3)] {
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    if nx >= 0 && ny >= 0 && (nx as usize) < img.w && (ny as usize) < img.h {
                        let (b2, g2, r2) = img.bgr(nx as usize, ny as usize);
                        ns += (r2.max(g2).max(b2)) as i32;
                        cnt += 1;
                    }
                }
                let avg = if cnt > 0 { ns / cnt } else { 0 };
                let bright = mx > avg + 45 || white;
                m[y * img.w + x] = if bright { 100 } else { 0 };
            }
        }
    }
    m
}

/// Вертикальные проекции маски по строкам: возвращает диапазоны строк с текстом.
fn row_bands(m: &[u8], w: usize, h: usize) -> Vec<(usize, usize)> {
    let mut bands = Vec::new();
    let mut start: Option<usize> = None;
    for y in 0..h {
        let sum: usize = m[y * w..(y + 1) * w].iter().map(|&v| (v > 0) as usize).sum();
        if sum >= 3 {
            if start.is_none() {
                start = Some(y);
            }
        } else if let Some(s) = start.take() {
            if y - s >= 5 {
                bands.push((s, y));
            }
        }
    }
    if let Some(s) = start {
        bands.push((s, h));
    }
    bands
}

/// Разбить полосу текста на символы по колонкам и распознать числа X Y Z.
/// Ожидается вид: "X: 123 Y: 45 Z: 678". Шумовые строки отбрасываются (None).
fn parse_line(m: &[u8], w: usize, y0: usize, y1: usize) -> Option<Pos> {
    let hh = y1 - y0;
    if hh < 5 || hh > 24 {
        return None;
    }
    // колонки с текстом
    let mut cols: Vec<(usize, usize)> = Vec::new();
    let mut s: Option<usize> = None;
    for x in 0..w {
        let any = (y0..y1).any(|y| m[y * w + x] > 0);
        if any {
            if s.is_none() {
                s = Some(x);
            }
        } else if let Some(a) = s.take() {
            cols.push((a, x));
        }
    }
    if let Some(a) = s {
        cols.push((a, w));
    }
    if cols.len() < 6 {
        return None;
    }
    // Собираем последовательность токенов: цифры (узнаются по шаблону) и прочее.
    #[derive(PartialEq)]
    enum Tok {
        D(u8),
        Other,
    }
    let mut toks: Vec<Tok> = Vec::new();
    for (a, b) in &cols {
        let cw = b - a;
        if cw >= 2 && cw <= 10 && hh >= 5 {
            if let Some(d) = recognize_digit(m, w, *a, *b, y0, y1) {
                toks.push(Tok::D(d));
                continue;
            }
        }
        toks.push(Tok::Other);
    }
    // Ищем три группы подряд идущих цифр (X, Y, Z).
    let mut groups: Vec<String> = Vec::new();
    let mut cur = String::new();
    for t in &toks {
        match t {
            Tok::D(d) => cur.push((*d + b'0') as char),
            Tok::Other => {
                if !cur.is_empty() {
                    groups.push(std::mem::take(&mut cur));
                }
            }
        }
    }
    if !cur.is_empty() {
        groups.push(cur);
    }
    // Двоеточие "X:" даёт маленькую группу из двух точек, которую шаблон может
    // ложно принять за '1'. Отбрасываем такие шумовые группы (< 3 знаков).
    groups.retain(|g| g.len() >= 3);
    if groups.len() != 3 {
        return None;
    }
    // Правдоподобие: X,Z до 7 знаков, Y до 4
    if groups.iter().any(|g| g.len() > 7) || groups[1].len() > 4 {
        return None;
    }
    let x: f64 = groups[0].parse().ok()?;
    let y: f64 = groups[1].parse().ok()?;
    let z: f64 = groups[2].parse().ok()?;
    Some(Pos { x, y, z })
}

fn recognize_digit(m: &[u8], w: usize, a: usize, b: usize, y0: usize, y1: usize) -> Option<u8> {
    let cw = b - a;
    let ch = y1 - y0;
    if cw < 2 || ch < 5 {
        return None;
    }
    // нормализуем в 5x7 сетку, сравниваем с шаблонами (IoU)
    let mut best = (0.0f32, 0u8);
    for d in 0..10u8 {
        let mut inter = 0.0f32;
        let mut uni = 0.0f32;
        for gy in 0..7usize {
            for gx in 0..5usize {
                let px0 = a + (cw * gx) / 5;
                let px1 = a + (cw * (gx + 1)).div_ceil(5);
                let py0 = y0 + (ch * gy) / 7;
                let py1 = y0 + (ch * (gy + 1)).div_ceil(7);
                let mut on = false;
                for py in py0..py1.max(py0 + 1) {
                    for px in px0..px1.max(px0 + 1) {
                        if px < w && py < y1 && m[py * w + px] > 0 {
                            on = true;
                            break;
                        }
                    }
                    if on {
                        break;
                    }
                }
                let t = (DIGITS[d as usize][gy] >> (4 - gx)) & 1 == 1;
                if on || t {
                    uni += 1.0;
                }
                if on && t {
                    inter += 1.0;
                }
            }
        }
        let iou = if uni > 0.0 { inter / uni } else { 0.0 };
        if iou > best.0 {
            best = (iou, d);
        }
    }
    if best.0 >= 0.55 {
        Some(best.1)
    } else {
        None
    }
}

/// Главный вход: по снимку региона вернуть координаты.
/// Сначала строгий проход, потом мягкий. Берётся самая нижняя подходящая полоса
/// (строка координат в HUD внизу слева).
pub fn read_pos(px: &[u8], w: usize, h: usize) -> Option<Pos> {
    if w < 40 || h < 8 {
        return None;
    }
    let img = Img { w, h, px };
    for strict in [true, false] {
        let m = text_mask(&img, strict);
        let bands = row_bands(&m, w, h);
        // снизу вверх - HUD-строка обычно самая нижняя осмысленная
        for (y0, y1) in bands.iter().rev() {
            if let Some(p) = parse_line(&m, w, *y0, *y1) {
                return Some(p);
            }
        }
    }
    None
}

/// ASCII-превью маски - для Shift+F2 при неудаче (# - светлый пиксель).
pub fn preview(px: &[u8], w: usize, h: usize) -> String {
    let img = Img { w, h, px };
    let m = text_mask(&img, false);
    let step = (w / 100).max(1);
    let mut out = String::new();
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(step) {
            out.push(if m[y * w + x] > 0 { '#' } else { '.' });
        }
        out.push('\n');
    }
    out
}
