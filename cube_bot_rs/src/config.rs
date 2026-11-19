// Настройки: аргументы командной строки + rust.ini в %APPDATA%\cube_bot.
// Значения сохраняются между запусками (target, mode, radius, locked region, water color).
#![allow(dead_code)]
use std::collections::HashMap;

pub const APP_DIR_KEY: &str = "APPDATA";

pub fn app_dir() -> String {
    let base = std::env::var(APP_DIR_KEY).unwrap_or_else(|_| ".".to_string());
    let dir = format!("{}\\cube_bot", base);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn ini_path() -> String {
    format!("{}\\rust.ini", app_dir())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Grow,    // X и Z растут
    ToPoint, // бежать к цели target
}

#[derive(Clone, Copy, Debug)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

pub struct Config {
    pub mode: Mode,
    pub target: (f64, f64),
    pub radius: f64,
    pub region: Option<Region>, // None = автоопределение
    pub recalibrate: bool,
    pub swim: bool,
    pub hop: bool,  // авто-прыжок на ступеньках
    pub brk: bool, // ломать блок ЛКМ при долгом застревании
    pub view: bool,
    pub ai: bool,
    // восстановленные из ini:
    pub locked_region: Option<Region>,
    pub water_color: Option<(u8, u8, u8)>,
    pub mouse_sens: f64, // пикселей мыши на 1 градус поворота
    pub side_bias: f64,  // запомненная сторона обхода (+1 вправо / -1 влево)
}

impl Config {
    pub fn load_from_args() -> Config {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let ini = read_ini(&ini_path());

        let mut cfg = Config {
            mode: match ini.get("mode").map(|s| s.as_str()) {
                Some("grow") => Mode::Grow,
                _ => Mode::ToPoint,
            },
            target: ini
                .get("target")
                .and_then(|s| {
                    let mut it = s.split_whitespace();
                    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
                })
                .unwrap_or((1_000_000.0, 1_000_000.0)),
            radius: ini.get("radius").and_then(|s| s.parse().ok()).unwrap_or(3.0),
            region: None,
            recalibrate: false,
            swim: ini.get("swim").map(|s| s == "1").unwrap_or(true),
            hop: ini.get("hop").map(|s| s == "1").unwrap_or(true),
            brk: ini.get("brk").map(|s| s == "1").unwrap_or(false),
            view: ini.get("view").map(|s| s == "1").unwrap_or(true),
            ai: ini.get("ai").map(|s| s == "1").unwrap_or(false),
            locked_region: ini.get("locked").and_then(parse_region),
            water_color: ini.get("water").and_then(|s| {
                let v: Vec<u8> = s.split(',').filter_map(|p| p.parse().ok()).collect();
                if v.len() == 3 {
                    Some((v[0], v[1], v[2]))
                } else {
                    None
                }
            }),
            mouse_sens: ini.get("sens").and_then(|s| s.parse().ok()).unwrap_or(3.5),
            side_bias: ini.get("side").and_then(|s| s.parse().ok()).unwrap_or(0.0),
        };

        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--target" if i + 2 < args.len() => {
                    if let (Ok(x), Ok(z)) =
                        (args[i + 1].parse::<f64>(), args[i + 2].parse::<f64>())
                    {
                        cfg.target = (x, z);
                        cfg.mode = Mode::ToPoint;
                    }
                    i += 2;
                }
                "--grow" => cfg.mode = Mode::Grow,
                "--radius" if i + 1 < args.len() => {
                    if let Ok(r) = args[i + 1].parse() {
                        cfg.radius = r;
                    }
                    i += 1;
                }
                "--region" if i + 3 < args.len() => {
                    let v: Vec<i32> =
                        args[i + 1..i + 4].iter().filter_map(|s| s.parse().ok()).collect();
                    if v.len() == 3 {
                        cfg.region = Some(Region { x: v[0], y: v[1], w: v[2], h: v[3] });
                    }
                    i += 3;
                }
                "--auto-region" => {
                    cfg.region = Some(Region { x: -1, y: -1, w: -1, h: -1 }); // маркер сброса
                    cfg.locked_region = None;
                }
                "--recalibrate" => cfg.recalibrate = true,
                "--no-swim" => cfg.swim = false,
                "--swim" => cfg.swim = true,
                "--no-hop" => cfg.hop = false,
                "--hop" => cfg.hop = true,
                "--break" => cfg.brk = true,
                "--no-view" => cfg.view = false,
                "--ai" => cfg.ai = true,
                "--no-ai" => cfg.ai = false,
                "-h" | "--help" => {
                    print_help();
                    std::process::exit(0);
                }
                _ => {}
            }
            i += 1;
        }
        cfg.save();
        cfg
    }

    pub fn save(&self) {
        let region_locked = match self.region {
            Some(r) if r.x >= 0 => Some(r),
            _ => self.locked_region,
        };
        let mut s = String::new();
        s.push_str(&format!("mode = {}\n", match self.mode {
            Mode::Grow => "grow",
            Mode::ToPoint => "point",
        }));
        s.push_str(&format!("target = {} {}\n", self.target.0, self.target.1));
        s.push_str(&format!("radius = {}\n", self.radius));
        s.push_str(&format!("swim = {}\n", self.swim as u8));
        s.push_str(&format!("hop = {}\n", self.hop as u8));
        s.push_str(&format!("brk = {}\n", self.brk as u8));
        s.push_str(&format!("view = {}\n", self.view as u8));
        s.push_str(&format!("ai = {}\n", self.ai as u8));
        s.push_str(&format!("sens = {:.3}\n", self.mouse_sens));
        s.push_str(&format!("side = {:.1}\n", self.side_bias));
        if let Some(r) = region_locked {
            s.push_str(&format!("locked = {} {} {} {}\n", r.x, r.y, r.w, r.h));
        }
        if let Some((b, g, rr)) = self.water_color {
            s.push_str(&format!("water = {},{},{}\n", b, g, rr));
        }
        let _ = std::fs::write(ini_path(), s);
    }

    pub fn set_water_color(&mut self, c: (u8, u8, u8)) {
        self.water_color = Some(c);
        self.save();
    }
    pub fn set_locked_region(&mut self, r: Region) {
        self.locked_region = Some(r);
        self.save();
    }
    pub fn clear_locked_region(&mut self) {
        self.locked_region = None;
        self.save();
    }
}

fn parse_region(s: &str) -> Option<Region> {
    let v: Vec<i32> = s.split_whitespace().filter_map(|p| p.parse().ok()).collect();
    if v.len() == 4 {
        Some(Region { x: v[0], y: v[1], w: v[2], h: v[3] })
    } else {
        None
    }
}

fn read_ini(path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if let Ok(t) = std::fs::read_to_string(path) {
        for line in t.lines() {
            if let Some((k, v)) = line.split_once('=') {
                m.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    m
}

pub fn print_help() {
    println!(r#"КУБ БОТ v5 - Куб Онлайн (Яндекс Игры). Только Windows 10/11.

Запуск: cube_bot.exe [опции]
  --target X Z      бежать к точке X Z (режим К ТОЧКЕ)
  --grow            режим РОСТ (X и Z растут)
  --radius N        радиус прибытия (по умолчанию 3)
  --region X Y W H  область поиска строки "X: Y: Z:"
  --auto-region     вернуть автоопределение области
  --recalibrate     откалибровать камеру заново
  --swim/--no-swim  плавание (зажим пробела в воде) вкл/выкл
  --hop/--no-hop    авто-прыжок на ступеньках вкл/выкл
  --break           ломать блок (ЛКМ), если застрял надолго
  --no-view         не писать файлы карты (view.html)
  --ai/--no-ai      ИИ-помощник (Hugging Face / Gemini)

Клавиши: F8 старт/стоп, F9 стоп, F4 режим, F10 калибровка камеры,
  F2 тест клавиш, Shift+F2 проверка чтения, Insert привязка области,
  Delete запомнить цвет воды (стоя в воде).
Команды консоли: go X Z | grow | radius N | status | view | token ... | model ... | ai on/off | ai test | ai gemini
Настройки: %APPDATA%\cube_bot\rust.ini"#);
}
