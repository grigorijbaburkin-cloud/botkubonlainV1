// КУБ БОТ v5 - главный цикл: чтение координат, курс камеры (как в Baritone),
// A*-путь, обход препятствий вдоль стен, плавание (зажатый пробел в воде), ночной режим.
#![allow(dead_code)]
use std::io::Write;
use std::time::Instant;

use crate::ai::Ai;
use crate::config::{app_dir, Config, Mode, Region};
use crate::input as inp;
use crate::map::{cell_of, WorldMap};
use crate::path::{astar, steer_target};
use crate::reader::{self, Pos};
use crate::view::ViewWriter;
use crate::win;

const TICK_MS: u64 = 20; // ~50 Гц основной цикл

#[derive(Clone, Copy, PartialEq)]
enum St {
    Run,      // бежим по плану A*
    Avoid,    // ОБХОД: идём вдоль стены
    Straight, // периодическая попытка побежать прямо на цель
    Sidestep, // нет прогресса - уходим в сторону
    AiTurn,   // ждём совет ИИ / бежим по нему
}

pub struct Bot {
    pub cfg: Config,
    pub running: bool,
    calib_done: bool,
    // позиция и курс
    pub pos: Pos,
    have_pos: bool,
    prev_pos: Pos,
    heading: f64, // градусы: 0 = +X, 90 = +Z (на карте X вправо, Z вниз)
    mouse_acc: i64, // сумма пикселей мыши, отправленных ботом (для самокалибровки sens)
    // чтение
    region: Option<Region>,
    auto_region: bool,
    last_capture_rect: Option<(i32, i32, i32, i32)>,
    read_ms: f64,
    ok_streak: usize,
    last_good: Instant,
    ad_since: Option<Instant>,
    pending: Option<(Pos, Instant)>,
    // вода
    water_hits: usize,
    water_miss_ms: u32,
    in_water: bool,
    swimming: bool,
    // движение / застревание
    speed: f64,
    stuck_since: Option<Instant>,
    wall_bumps: u32,
    last_bump: Instant,
    avoid_side: f64,
    // состояния
    st: St,
    st_until: f64,
    avoid_enter: f64,
    last_hug_t: f64,
    last_straight_try: f64,
    straight_start: (f64, f64),
    // цель / путь
    goal_cell: (i32, i32),
    path: Vec<(i32, i32)>,
    steer: Option<(i32, i32)>,
    last_astar: Instant,
    path_dirty: bool,
    no_turn_until: f64,
    // прогресс
    best_dist: f64,
    last_progress: Instant,
    stall_attempts: u32,
    // карта/файлы
    map: WorldMap,
    trail: Vec<(f64, f64)>,
    last_save_t: f64,
    last_view_t: f64,
    // ИИ
    ai: Ai,
    ai_state: u8,
    ai_wait: Instant,
    ai_advice: Option<f64>,
    // прочее
    hop_until: f64,
    next_hop: f64,
    brk_at: f64,
}

fn now_f() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn clock_hms() -> String {
    let s = now_f() as u64 % 86400;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

fn deg_to_dir(deg: f64) -> (f64, f64) {
    let r = deg.to_radians();
    (r.cos(), r.sin())
}

fn norm180(a: f64) -> f64 {
    let mut a = a % 360.0;
    if a > 180.0 {
        a -= 360.0;
    }
    if a < -180.0 {
        a += 360.0;
    }
    a
}

impl Bot {
    fn new(cfg: Config) -> Bot {
        let mut b = Bot {
            cfg,
            running: false,
            calib_done: false,
            pos: Pos::default(),
            have_pos: false,
            prev_pos: Pos::default(),
            heading: 0.0,
            mouse_acc: 0,
            region: None,
            auto_region: true,
            last_capture_rect: None,
            read_ms: 0.0,
            ok_streak: 0,
            last_good: Instant::now(),
            ad_since: None,
            pending: None,
            water_hits: 0,
            water_miss_ms: 0,
            in_water: false,
            swimming: false,
            speed: 0.0,
            stuck_since: None,
            wall_bumps: 0,
            last_bump: Instant::now(),
            avoid_side: 1.0,
            st: St::Run,
            st_until: 0.0,
            avoid_enter: 0.0,
            last_hug_t: 0.0,
            last_straight_try: 0.0,
            straight_start: (0.0, 0.0),
            goal_cell: (0, 0),
            path: Vec::new(),
            steer: None,
            last_astar: Instant::now(),
            path_dirty: true,
            no_turn_until: 0.0,
            best_dist: f64::MAX,
            last_progress: Instant::now(),
            stall_attempts: 0,
            map: WorldMap::new(),
            trail: Vec::new(),
            last_save_t: 0.0,
            last_view_t: 0.0,
            ai: Ai::new(false),
            ai_state: 0,
            ai_wait: Instant::now(),
            ai_advice: None,
            hop_until: 0.0,
            next_hop: 0.0,
            brk_at: 0.0,
        };
        b.avoid_side = if b.cfg.side_bias < 0.0 { -1.0 } else { 1.0 };
        if let Some(r) = b.cfg.region.take() {
            if r.x >= 0 {
                b.region = Some(r);
                b.auto_region = false;
            } else {
                // --auto-region: сброс сохранённой области
                b.region = None;
                b.auto_region = true;
                b.cfg.clear_locked_region();
            }
        } else if let Some(r) = b.cfg.locked_region {
            b.region = Some(r);
            b.auto_region = false;
        }
        if b.cfg.recalibrate {
            b.cfg.mouse_sens = 3.5;
            b.calib_done = false;
        }
        b.ai.enabled = b.cfg.ai;
        let n = b.map.load(&format!("{}\\map.txt", app_dir()), now_f() as u64);
        if n > 0 {
            println!("Карта: загружено {} клеток с прошлого раза.", n);
        }
        b
    }

    pub fn log(&mut self, s: &str) {
        let line = format!("[{}] {}", clock_hms(), s);
        println!("{}", line);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(format!("{}\\log.txt", app_dir()))
        {
            let _ = writeln!(f, "{}", line);
        }
    }

    // ---------- чтение координат ----------

    fn capture_region(&mut self) -> Option<(usize, usize, Vec<u8>)> {
        let screen = win::grab_screen()?;
        let (x, y, w, h): (i32, i32, i32, i32) = if let Some(r) = self.region {
            (r.x, r.y, r.w, r.h)
        } else {
            // авто: левый нижний угол экрана (строка "X: Y: Z:" в HUD игры)
            let w = (screen.width / 3).clamp(320, 720);
            let h = 60;
            (0, screen.height - h - 4, w, h)
        };
        let x = x.max(0).min(screen.width.saturating_sub(1));
        let y = y.max(0).min(screen.height.saturating_sub(1));
        let w = w.min(screen.width - x).max(1);
        let h = h.min(screen.height - y).max(1);
        self.last_capture_rect = Some((x, y, w, h));
        let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
        for row in 0..h as usize {
            let src = ((y as usize + row) * screen.width as usize + x as usize) * 4;
            let dst = row * w as usize * 4;
            let len = w as usize * 4;
            if src + len <= screen.pixels.len() {
                out[dst..dst + len].copy_from_slice(&screen.pixels[src..src + len]);
            }
        }
        Some((w as usize, h as usize, out))
    }

    /// Снимок текущей области для диагностики (Shift+F2 при неудаче).
    fn capture_dbg(&mut self) -> Option<(usize, usize, Vec<u8>)> {
        self.capture_region()
    }

    /// Одно чтение координат (~1 мс) со всеми фильтрами.
    pub fn read(&mut self) -> Option<Pos> {
        let t = Instant::now();
        let (w, h, px) = self.capture_region()?;
        let mut p = reader::read_pos(&px, w, h);
        if p.is_none() && !self.auto_region {
            // последний шанс: поиск по всему экрану
            if let Some(full) = win::grab_screen() {
                p = reader::read_pos(&full.pixels, full.width as usize, full.height as usize);
            }
        }
        self.read_ms = t.elapsed().as_secs_f64() * 1000.0;
        match p {
            Some(np) => {
                self.last_good = Instant::now();
                if !self.have_pos {
                    self.accept_pos(np);
                    return Some(self.pos);
                }
                let d = self.pos.dist_xz(&np);
                if d <= 6.0 {
                    self.accept_pos(np); // обычное обновление
                } else if d < 25.0 {
                    // рывок >6 блоков за тик - подозрительно, ждём подтверждения 0.25 с
                    if self.confirm_pending(np) {
                        self.accept_pos(np);
                    }
                } else if self.confirm_pending(np) {
                    // резкая смена позиции (респавн/телепорт): принимаем после 0.25 с
                    self.log("Резкая смена позиции (респавн?) - заново определяю курс.");
                    self.accept_pos(np);
                    if self.running {
                        self.prestart_measure(); // перекалибровка курса с нового места
                    }
                }
                Some(self.pos)
            }
            None => {
                self.ok_streak = 0;
                self.pending = None;
                if self.running && self.last_good.elapsed().as_secs() > 5 {
                    // реклама/меню: строки координат нет на экране
                    let start = *self.ad_since.get_or_insert_with(Instant::now);
                    if start.elapsed().as_secs() > 30 * 60 {
                        self.log("Экран без координат (реклама/меню) дольше 30 минут - стоп.");
                        self.stop(true);
                    }
                }
                None
            }
        }
    }

    fn confirm_pending(&mut self, np: Pos) -> bool {
        match self.pending {
            Some((p, t)) if p.dist_xz(&np) < 1.0 => {
                if t.elapsed().as_secs_f64() >= 0.25 {
                    self.pending = None;
                    true
                } else {
                    false
                }
            }
            _ => {
                self.pending = Some((np, Instant::now()));
                false
            }
        }
    }

    fn accept_pos(&mut self, np: Pos) {
        if !self.have_pos {
            self.prev_pos = np;
            self.pos = np;
            self.have_pos = true;
            self.ok_streak = 1;
            return;
        }
        self.prev_pos = self.pos;
        self.pos = np;
        self.ok_streak += 1;
        self.ad_since = None;
        // область запоминается только после 40 удачных чтений подряд
        if self.auto_region && self.ok_streak == 40 {
            if let Some((x, y, w, h)) = self.last_capture_rect {
                let r = Region { x, y, w, h };
                self.region = Some(r);
                self.auto_region = false;
                self.cfg.set_locked_region(r);
                if let Some((rw, rh, px)) = self.capture_region() {
                    win::save_bmp(&format!("{}\\region.bmp", app_dir()), rw as i32, rh as i32, &px);
                }
            }
        }
    }

    // ---------- вода ----------

    /// Экран залит водой? Центр кадра сине-голубой или совпадает с цветом,
    /// запомненным Delete-ом.
    fn detect_water(&self) -> bool {
        let screen = match win::grab_screen() {
            Some(s) => s,
            None => return false,
        };
        let cx = screen.width / 2;
        let cy = screen.height / 2;
        let (b, g, r) = screen.rgb(cx, cy);
        let blueish = b as i32 > r as i32 + 25 && b > 70;
        if let Some((wb, wg, wr)) = self.cfg.water_color {
            let close = (b as i32 - wb as i32).abs() < 40
                && (g as i32 - wg as i32).abs() < 40
                && (r as i32 - wr as i32).abs() < 40;
            return close || blueish;
        }
        blueish
    }

    /// Гистерезис: вода = 3 подряд положительных проверки; выход через ~1 с без воды.
    pub fn update_water(&mut self, dt: f64) {
        if !self.cfg.swim {
            self.in_water = false;
            return;
        }
        if self.detect_water() {
            self.water_hits += 1;
            self.water_miss_ms = 0;
        } else {
            self.water_hits = 0;
            self.water_miss_ms += (dt * 1000.0) as u32;
        }
        let was = self.in_water;
        if self.water_hits >= 3 {
            self.in_water = true;
        } else if self.in_water && self.water_miss_ms > 1000 {
            self.in_water = false;
        }
        if self.in_water != was && self.running {
            println!(
                "{}",
                if self.in_water { "ВОДА: зажимаю пробел" } else { "выплыл: пробел отпущен" }
            );
        }
    }

    // ---------- курс и повороты ----------

    /// Поворот камеры на deg градусов (вправо +). Учитывается в self.heading.
    pub fn turn(&mut self, deg: f64) {
        let px = (deg * self.cfg.mouse_sens).round() as i32;
        if px != 0 {
            inp::mouse_move(px);
            self.mouse_acc += px as i64;
            self.heading = norm180(self.heading + deg);
        }
    }

    /// Самокалибровка курса по фактическому движению (как учёт Baritone):
    /// если бежим быстро и направление движения стабильно - подправляем heading
    /// и чувствительность мыши; в воде координаты меняются медленно - не трогаем.
    fn correct_heading(&mut self) {
        let dx = self.pos.x - self.prev_pos.x;
        let dz = self.pos.z - self.prev_pos.z;
        let d = (dx * dx + dz * dz).sqrt();
        if !self.running || self.in_water || d < 0.35 || self.speed < 2.0 {
            return;
        }
        let moved = dz.atan2(dx).to_degrees();
        // ожидаемое направление движения = курс (бежим вперёд вдоль камеры)
        let err = norm180(moved - self.heading);
        if err.abs() > 80.0 {
            return; // упор/снос/прыжок - не корректный сигнал
        }
        // экспоненциальное сглаживание дрейфа курса
        self.heading = norm180(self.heading + err * 0.25);
        // уточнение чувствительности мыши: сколько пикселей реально дали этот поворот
        let mx = std::mem::take(&mut self.mouse_acc);
        if mx.abs() > 40 {
            // движение изменилось на moved относительно прежнего heading; грубая,
            // но полезная оценка sens в связке с F10
            let est = (mx as f64 / err.abs().max(1.0)).clamp(1.5, 12.0);
            self.cfg.mouse_sens = self.cfg.mouse_sens * 0.9 + est * 0.1;
        }
    }

    /// Плавно довернуть к желаемому курсу (до 25° за тик).
    fn steer_to(&mut self, want: f64) {
        if now_f() < self.no_turn_until {
            return;
        }
        let err = norm180(want - self.heading);
        let step = err.clamp(-25.0, 25.0);
        if step.abs() > 1.0 {
            self.turn(step);
        }
    }

    // ---------- план движения ----------

    fn target_dir(&self) -> (f64, f64, f64) {
        match self.cfg.mode {
            Mode::ToPoint => {
                let dx = self.cfg.target.0 - self.pos.x;
                let dz = self.cfg.target.1 - self.pos.z;
                let d = (dx * dx + dz * dz).sqrt();
                if d < 1e-6 {
                    (1.0, 0.0, 0.0)
                } else {
                    (dx / d, dz / d, d)
                }
            }
            Mode::Grow => {
                // РОСТ: диагональ, где X и Z растут
                let d = 2.0f64.sqrt();
                (1.0 / d, 1.0 / d, d)
            }
        }
    }

    pub fn tick_plan(&mut self, dt: f64) {
        let t = now_f();
        let (tdx, tdz, tdist) = self.target_dir();

        // прибытие (К ТОЧКЕ): пикает и останавливается
        if self.cfg.mode == Mode::ToPoint && tdist <= self.cfg.radius {
            self.log(&format!("ПРИБЫЛ: X={:.0} Z={:.0}", self.pos.x, self.pos.z));
            self.stop(false);
            return;
        }

        // прогресс: в К ТОЧКЕ - расстояние уменьшается; в РОСТ - X и Z растут
        match self.cfg.mode {
            Mode::ToPoint => {
                if tdist < self.best_dist - 0.3 {
                    self.best_dist = tdist;
                    self.last_progress = Instant::now();
                    self.stall_attempts = 0;
                }
            }
            Mode::Grow => {
                let gx = self.pos.x - self.prev_pos.x;
                let gz = self.pos.z - self.prev_pos.z;
                if gx + gz > 0.05 {
                    self.last_progress = Instant::now();
                    self.stall_attempts = 0;
                }
            }
        }

        // детектор упора: W зажата, а скорости нет (не в воде!)
        let pushing = inp::is_down(inp::VK_W);
        if pushing && self.speed < 0.8 && !self.in_water {
            let s = self.stuck_since.get_or_insert_with(Instant::now);
            if s.elapsed().as_secs_f64() > 0.7 {
                self.on_wall_bump(t);
                self.stuck_since = None;
            }
        } else {
            self.stuck_since = None;
        }

        // нет прогресса 60 с (в режиме обхода - 150 с)
        let limit = if self.st == St::Avoid { 150.0 } else { 60.0 };
        if self.last_progress.elapsed().as_secs_f64() > limit {
            self.on_stall(t);
        }

        match self.st {
            St::Run => self.plan_run(t, tdx, tdz),
            St::Avoid => self.plan_avoid(t, tdx, tdz),
            St::Straight => self.plan_straight(t, tdx, tdz),
            St::Sidestep => {
                if t >= self.st_until {
                    self.st = St::Run;
                }
            }
            St::AiTurn => self.ai_step(t),
        }

        // авто-прыжок: короткие нажатия пробела берут ступеньки в 1 блок;
        // в воде прыжок не нужен (пробел и так удерживается плаванием)
        if self.cfg.hop && !self.in_water && self.speed > 0.3 && t > self.next_hop {
            self.next_hop = t + 1.6;
            self.hop_until = t + 0.3;
        }
        let _ = dt;
    }

    fn plan_run(&mut self, t: f64, tdx: f64, tdz: f64) {
        // A* перестраиваем раз в 1.5 с и сразу при новой стене
        if self.last_astar.elapsed().as_secs_f64() > 1.5 || self.path_dirty {
            self.path_dirty = false;
            self.goal_cell = if self.cfg.mode == Mode::ToPoint {
                (self.cfg.target.0.floor() as i32, self.cfg.target.1.floor() as i32)
            } else {
                let far = 300.0;
                (
                    (self.pos.x + tdx * far).floor() as i32,
                    (self.pos.z + tdz * far).floor() as i32,
                )
            };
            self.path = astar(&self.map, cell_of(&self.pos), self.goal_cell, t as u64);
            self.last_astar = Instant::now();
        }
        self.steer = steer_target(&self.map, cell_of(&self.pos), &self.path, 12, t as u64);
        let want = if let Some((sx, sz)) = self.steer {
            let dx = sx as f64 + 0.5 - self.pos.x;
            let dz = sz as f64 + 0.5 - self.pos.z;
            dz.atan2(dx).to_degrees()
        } else {
            tdz.atan2(tdx).to_degrees()
        };
        self.steer_to(want);
    }

    fn on_wall_bump(&mut self, t: f64) {
        self.wall_bumps += 1;
        self.last_bump = Instant::now();
        // отметить стену: полоса шире при повторах: 7, 11, 15, 19 блоков
        let width = 7 + 4 * (self.wall_bumps - 1).min(3) as i32;
        let (hx, hz) = deg_to_dir(self.heading);
        self.map.mark_wall(&self.pos, hx, hz, width, 2, t as u64);
        self.path_dirty = true;
        if self.st != St::Avoid && self.st != St::Straight {
            self.st = St::Avoid;
            self.avoid_enter = t;
            self.last_straight_try = t;
            self.log(&format!(
                "ОБХОД: упор №{}, держу стену {}",
                self.wall_bumps,
                if self.avoid_side > 0.0 { "справа" } else { "слева" }
            ));
            // развернуться вдоль стены
            self.turn(90.0 * self.avoid_side);
            self.no_turn_until = t + 0.6;
        } else if self.st == St::Avoid && t - self.avoid_enter > 10.0 && self.ai.enabled {
            self.start_ai(t);
        }
        self.hop_now(t);
    }

    fn plan_avoid(&mut self, t: f64, tdx: f64, tdz: f64) {
        // держимся стены: свежий упор - отворачиваем от неё (при повторах сильнее)
        if self.last_bump.elapsed().as_secs_f64() < 0.9 {
            let k = 1.0 + 0.35 * self.wall_bumps.min(6) as f64;
            self.turn(-self.avoid_side * 35.0 * k);
        } else if t - self.last_hug_t > 0.7 {
            // бежит свободно - раз в 0.7 с чуть прижимаемся к стене
            self.turn(self.avoid_side * 8.0);
            self.last_hug_t = t;
        }
        // раз в 2.5 с пробуем 0.8 с побежать прямо на цель
        if t - self.last_straight_try > 2.5 {
            self.st = St::Straight;
            self.st_until = t + 0.8;
            self.straight_start = (self.pos.x, self.pos.z);
            self.last_straight_try = t;
            self.no_turn_until = 0.0;
            let want = tdz.atan2(tdx).to_degrees();
            self.steer_to(want);
        }
    }

    fn plan_straight(&mut self, t: f64, tdx: f64, tdz: f64) {
        let want = tdz.atan2(tdx).to_degrees();
        self.steer_to(want);
        if t >= self.st_until {
            let d = ((self.pos.x - self.straight_start.0).powi(2)
                + (self.pos.z - self.straight_start.1).powi(2))
                .sqrt();
            if d >= 2.0 {
                // прошёл >=2 блока прямо - обход закончен
                self.st = St::Run;
                self.wall_bumps = 0;
                self.cfg.side_bias = self.avoid_side;
                self.cfg.save();
                self.no_turn_until = t + 2.5; // не доворачивать сразу - вдруг та же стена
                self.log("Прямой путь свободен - обход закончен.");
            } else {
                self.st = St::Avoid; // вернулись к стене, идём дальше
            }
        }
    }

    fn on_stall(&mut self, t: f64) {
        self.stall_attempts += 1;
        self.last_progress = Instant::now();
        // углы ухода растут с попытками: 100..180°, паузы длиннее
        let angle = (100.0 + 15.0 * self.stall_attempts.min(6) as f64).min(180.0);
        let dur = (6.0 + self.stall_attempts.min(4) as f64).min(10.0);
        self.log(&format!(
            "Нет прогресса - уход в сторону {:.0}° (попытка {}).",
            angle * self.avoid_side,
            self.stall_attempts
        ));
        if self.stall_attempts == 2 {
            self.map.forget_walls();
            self.path_dirty = true;
            self.log("Забываю стены (возможно, были ошибочные).");
        }
        if self.stall_attempts >= 3 && self.ai.enabled {
            self.start_ai(t);
            return;
        }
        self.st = St::Sidestep;
        self.st_until = t + dur;
        self.turn(angle * self.avoid_side);
    }

    fn hop_now(&mut self, t: f64) {
        if self.cfg.hop {
            self.hop_until = t + 0.35;
        }
    }

    // ---------- ИИ-помощник ----------

    fn start_ai(&mut self, _t: f64) {
        self.st = St::AiTurn;
        self.ai_state = 0;
        self.ai_wait = Instant::now();
        self.ai_advice = None;
        self.log("Обход долгий - зову ИИ-помощника (стою, жду совет)...");
    }

    fn ai_step(&mut self, t: f64) {
        match self.ai_state {
            0 => {
                // встали, смотрим на цель, снимаем экран
                let (tdx, tdz, _) = self.target_dir();
                self.steer_to(tdz.atan2(tdx).to_degrees());
                if self.ai_wait.elapsed().as_secs_f64() > 1.2 {
                    self.snap_game_png();
                    let b64 = self.read_img_b64();
                    self.ai_advice = self.ai.ask(t, &b64);
                    self.ai_wait = Instant::now();
                    self.ai_state = 1;
                }
            }
            1 => {
                if let Some(d) = self.ai_advice.take() {
                    self.log(&format!("ИИ совет: TURN={:.0}°", d));
                    self.turn(d);
                } else {
                    self.log("ИИ молчит - продолжаю обычный обход.");
                }
                self.ai_state = 2;
                self.ai_wait = Instant::now();
            }
            2 => {
                // бежим по совету 6 с, затем возвращаемся к обычной логике
                if self.ai_wait.elapsed().as_secs_f64() > 6.0 {
                    self.ai_state = 0;
                    self.st = St::Avoid;
                    self.avoid_enter = t;
                }
            }
            _ => self.ai_state = 0,
        }
    }

    fn snap_game_png(&mut self) {
        // снимок игры (без рекламы справа: берём левые 2/3 ширины)
        if let Some(s) = win::grab_screen() {
            let w = s.width * 2 / 3;
            let h = s.height;
            let mut px = vec![0u8; (w as usize) * (h as usize) * 4];
            for y in 0..h as usize {
                let src = (y * s.width as usize) * 4;
                let dst = y * w as usize * 4;
                px[dst..dst + w as usize * 4]
                    .copy_from_slice(&s.pixels[src..src + w as usize * 4]);
            }
            let bmp = format!("{}\\ai_last.bmp", app_dir());
            win::save_bmp(&bmp, w, h, &px);
            // BMP -> PNG средствами .NET (PowerShell)
            let png = format!("{}\\ai_last.png", app_dir());
            let ps = format!(
                "Add-Type -AssemblyName System.Drawing; $b=New-Object System.Drawing.Bitmap('{}'); $b.Save('{}',[System.Drawing.Imaging.ImageFormat]::Png); $b.Dispose()",
                bmp, png
            );
            let _ = std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", &ps])
                .output();
        }
    }

    fn read_img_b64(&self) -> String {
        let png = format!("{}\\ai_last.png", app_dir());
        let data = std::fs::read(&png)
            .or_else(|_| std::fs::read(format!("{}\\ai_last.bmp", app_dir())))
            .unwrap_or_default();
        b64(&data)
    }

    // ---------- клавиши каждый тик ----------

    pub fn apply_keys(&mut self, t: f64) {
        // W зажата всё время бега
        hold(inp::VK_W, true);
        // ПРОБЕЛ: в воде удерживается постоянно (плавание), иначе - короткие прыжки
        let want_space = (self.cfg.swim && self.in_water) || t < self.hop_until;
        if want_space != self.swimming {
            self.swimming = want_space;
            if want_space {
                inp::key_down(inp::VK_SPACE);
            } else {
                inp::key_up(inp::VK_SPACE);
            }
        }
        // застрял в воде: W и пробел НЕ отпускаем, сворачиваем в сторону (не пятимся)
        if self.in_water && self.speed < 0.4 && self.last_bump.elapsed().as_secs_f64() > 2.0 {
            self.last_bump = Instant::now();
            self.turn(45.0 * self.avoid_side);
        }
        // ломать блок ЛКМ при долгом застревании (--break)
        if self.cfg.brk && self.wall_bumps >= 4 && t - self.brk_at > 4.0 {
            self.brk_at = t;
            inp::click_left();
        }
    }

    // ---------- старт/стоп ----------

    pub fn start(&mut self) {
        if self.running {
            return;
        }
        if !self.have_pos {
            println!("НЕ ЧИТАЮ координаты - старт невозможен. Нажми Shift+F2 для проверки.");
            return;
        }
        if !self.calib_done {
            println!("Первый запуск: калибрую камеру автоматически...");
            self.calibrate();
        }
        self.running = true;
        self.st = St::Run;
        self.best_dist = f64::MAX;
        self.last_progress = Instant::now();
        self.path_dirty = true;
        win::keep_awake(true);
        win::beep(true); // высокий = старт
        self.log(&format!(
            "СТАРТ: режим {}, цель ({}, {})",
            if self.cfg.mode == Mode::Grow { "РОСТ" } else { "К ТОЧКЕ" },
            self.cfg.target.0,
            self.cfg.target.1
        ));
        // предпробег ~1.5 с: узнаём, куда смотрим, и сами разворачиваемся на цель
        self.prestart_measure();
    }

    fn prestart_measure(&mut self) {
        let t_end = now_f() + 1.5;
        let p0 = self.pos;
        inp::key_down(inp::VK_W);
        while now_f() < t_end {
            self.read();
            std::thread::sleep(std::time::Duration::from_millis(TICK_MS));
        }
        let dx = self.pos.x - p0.x;
        let dz = self.pos.z - p0.z;
        let d = (dx * dx + dz * dz).sqrt();
        if d > 0.5 {
            self.heading = dz.atan2(dx).to_degrees();
            println!("Курс определён: {:.0}° (пробежал {:.1} кл.)", self.heading, d);
        } else {
            println!("Предпробег: движение не замечено - использую текущий курс.");
        }
        let (tdx, tdz, _) = self.target_dir();
        let err = norm180(tdz.atan2(tdx).to_degrees() - self.heading);
        println!("Разворот на цель: {:+.0}°", err);
        self.turn(err);
    }

    pub fn stop(&mut self, emergency: bool) {
        if !self.running {
            return;
        }
        self.running = false;
        self.swimming = false;
        inp::release_all();
        win::keep_awake(false);
        win::beep(false); // низкий = стоп
        self.map.save(&format!("{}\\map.txt", app_dir()));
        self.log(if emergency { "АВАРИЙНЫЙ СТОП" } else { "СТОП" });
    }

    fn calibrate(&mut self) {
        // sens мыши: бег вперёд, фиксированный поворот, замер направления движения.
        println!("Калибровка камеры: нужно ~20 свободных блоков впереди.");
        self.prestart_measure();
        let p0 = self.pos;
        inp::mouse_move(315); // ~90° при типовом sens
        std::thread::sleep(std::time::Duration::from_millis(250));
        self.read();
        let dx = self.pos.x - p0.x;
        let dz = self.pos.z - p0.z;
        let d = (dx * dx + dz * dz).sqrt();
        if d > 1.5 {
            let moved = dz.atan2(dx).to_degrees();
            let diff = norm180(moved - self.heading);
            if diff.abs() > 20.0 {
                let est = (315.0 / diff.abs()).clamp(1.0, 15.0);
                self.cfg.mouse_sens = est;
                println!("Чувствительность мыши ≈ {:.2} пикс/град", est);
            }
        }
        self.turn(-90.0);
        self.calib_done = true;
        self.cfg.save();
        println!("Калибровка завершена.");
    }

    // ---------- статус ----------

    pub fn status_line(&self) -> String {
        format!(
            "{} {} X:{:.0} Y:{:.1} Z:{:.0} | курс {:.0}° ск {:.1}кл/с {}{}путь {} кл. | {:.*}мс",
            if self.running { "►БЕГУ" } else { "■стоп" },
            if self.cfg.mode == Mode::Grow { "РОСТ" } else { "К ТОЧКЕ" },
            self.pos.x,
            self.pos.y,
            self.pos.z,
            self.heading,
            self.speed,
            if self.in_water { "🌊вода " } else { "" },
            match self.st {
                St::Avoid => "ОБХОД ",
                St::Straight => "ПРЯМО? ",
                St::Sidestep => "В СТОРОНУ ",
                St::AiTurn => "ИИ ",
                St::Run => "",
            },
            self.path.len(),
            1,
            self.read_ms
        )
    }

    pub fn write_view(&mut self, view: &ViewWriter, t: f64) {
        if !self.cfg.view || !self.have_pos || t - self.last_view_t < 0.3 {
            return;
        }
        self.last_view_t = t;
        let status = self.status_line();
        view.write_state(
            &self.pos,
            self.heading,
            self.in_water,
            &self.trail,
            &self.map.opens(),
            &self.map.walls(t as u64),
            &self.path,
            self.steer,
            self.cfg.target,
            &status,
        );
    }
}

fn hold(vk: u32, down: bool) {
    if inp::is_down(vk) != down {
        if down {
            inp::key_down(vk);
        } else {
            inp::key_up(vk);
        }
    }
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len() * 4 / 3 + 4);
    for ch in data.chunks(3) {
        let b0 = ch[0] as u32;
        let b1 = *ch.get(1).unwrap_or(&0) as u32;
        let b2 = *ch.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if ch.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if ch.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

// ---------- главный цикл ----------

pub fn run(cfg: Config) {
    win::console_title("КУБ БОТ");
    win::hide_console_from_captures();
    inp::install_hotkey_hook();
    println!(
        "КУБ БОТ v5 (Rust). F8 старт/стоп, F9 стоп, F4 режим, F10 калибровка,\n\
         F2 тест клавиш, Shift+F2 проверка чтения, Insert привязка области,\n\
         Delete цвет воды. Команды: help. Настройки: %APPDATA%\\cube_bot"
    );
    let mut bot = Bot::new(cfg);
    let view = ViewWriter::new(app_dir(), bot.cfg.view);
    let mut last = Instant::now();

    loop {
        inp::pump_messages();
        let dt = last.elapsed().as_secs_f64().min(0.2);
        last = Instant::now();
        let t = now_f();

        // ---- hotkeys ----
        if inp::take(&inp::HOT_F9) {
            bot.stop(true);
        }
        if inp::take(&inp::HOT_F8) {
            if bot.running {
                bot.stop(false);
            } else {
                bot.start();
            }
        }
        if inp::take(&inp::HOT_F4) {
            bot.cfg.mode = match bot.cfg.mode {
                Mode::Grow => Mode::ToPoint,
                Mode::ToPoint => Mode::Grow,
            };
            bot.cfg.save();
            bot.path_dirty = true;
            println!(
                "Режим: {}",
                if bot.cfg.mode == Mode::Grow { "РОСТ" } else { "К ТОЧКЕ" }
            );
        }
        if inp::take(&inp::HOT_F10) {
            bot.calibrate();
        }
        if inp::take(&inp::HOT_SF2) {
            test_read(&mut bot);
        }
        if inp::take(&inp::HOT_F2) {
            test_keys(&mut bot);
        }
        if inp::take(&inp::HOT_DELETE) {
            if let Some(s) = win::grab_screen() {
                let c = s.rgb(s.width / 2, s.height / 2);
                bot.cfg.set_water_color(c);
                println!("Запомнил цвет воды: RGB({}, {}, {})", c.2, c.1, c.0);
            }
        }
        if inp::take(&inp::HOT_INSERT) {
            // ручная привязка: Esc в игре -> навести курсор на строку "X: Y: Z:" -> Insert
            let (cx, cy) = win::cursor_pos();
            let r = Region {
                x: (cx - 160).max(0),
                y: (cy - 14).max(0),
                w: 460,
                h: 34,
            };
            bot.region = Some(r);
            bot.auto_region = false;
            bot.cfg.set_locked_region(r);
            println!("Привязал область: {} {} {} {} (кликни обратно в игру)", r.x, r.y, r.w, r.h);
        }

        poll_console(&mut bot, &view);

        // ---- чтение и физика ----
        let had = bot.read().is_some();
        bot.speed = if had && dt > 0.005 {
            let d = bot.pos.dist_xz(&bot.prev_pos);
            (d / dt).min(20.0)
        } else {
            bot.speed * 0.9
        };
        bot.update_water(dt);

        if bot.running {
            bot.correct_heading();
            bot.tick_plan(dt);
            bot.apply_keys(t);
            bot.map.mark_open(&bot.pos, t as u64);
            bot.trail.push((bot.pos.x, bot.pos.z));
            if bot.trail.len() > 4000 {
                bot.trail.drain(0..2000);
            }
            bot.map.expire(t as u64);
            if t - bot.last_save_t > 60.0 {
                bot.last_save_t = t;
                bot.map.save(&format!("{}\\map.txt", app_dir()));
            }
        } else {
            bot.speed = 0.0;
        }

        // ---- статус и карта ----
        win::console_title(&bot.status_line());
        bot.write_view(&view, t);

        std::thread::sleep(std::time::Duration::from_millis(TICK_MS));
    }
}

// ---------- диагностики ----------

fn test_read(bot: &mut Bot) {
    let t = Instant::now();
    match bot.read() {
        Some(p) => println!(
            "Читаю: X={:.0} Y={:.0} Z={:.0} ({:.1} мс)",
            p.x,
            p.y,
            p.z,
            t.elapsed().as_secs_f64() * 1000.0
        ),
        None => {
            println!("НЕ ЧИТАЮ координаты. Вот что видит бот (# - светлый пиксель):");
            if let Some((w, h, px)) = bot.capture_dbg() {
                println!("{}", reader::preview(&px, w, h));
                win::save_bmp(&format!("{}\\region.bmp", app_dir()), w as i32, h as i32, &px);
            }
            if let Some(s) = win::grab_screen() {
                // левый нижний угол крупным планом
                let sw = (s.width / 3).min(720).max(1);
                let sh = 120.min(s.height);
                let mut px = vec![0u8; (sw as usize) * (sh as usize) * 4];
                for y in 0..sh as usize {
                    let src = (((s.height - sh) as usize + y) * s.width as usize) * 4;
                    let dst = y * sw as usize * 4;
                    if src + sw as usize * 4 <= s.pixels.len() {
                        px[dst..dst + sw as usize * 4]
                            .copy_from_slice(&s.pixels[src..src + sw as usize * 4]);
                    }
                }
                win::save_bmp(&format!("{}\\hud_corner.png", app_dir()), sw, sh, &px);
                // весь экран 2x уменьшенно
                let (dw, dh) = (s.width / 2, s.height / 2);
                let mut small = vec![0u8; (dw as usize) * (dh as usize) * 4];
                for y in 0..dh as usize {
                    for x in 0..dw as usize {
                        let si = ((y * 2) * s.width as usize + x * 2) * 4;
                        let di = (y * dw as usize + x) * 4;
                        if si + 4 <= s.pixels.len() {
                            small[di..di + 4].copy_from_slice(&s.pixels[si..si + 4]);
                        }
                    }
                }
                win::save_bmp(&format!("{}\\screen_small.png", app_dir()), dw, dh, &small);
                println!("Снимки: %APPDATA%\\cube_bot\\screen_small.png, hud_corner.png, region.bmp");
            }
        }
    }
}

fn test_keys(bot: &mut Bot) {
    println!("ТЕСТ клавиш: бегу вперёд 2 с...");
    inp::key_down(inp::VK_W);
    std::thread::sleep(std::time::Duration::from_millis(2000));
    inp::key_up(inp::VK_W);
    println!("Поворачиваю камеру вправо на 60°...");
    bot.turn(60.0);
}

fn poll_console(bot: &mut Bot, view: &ViewWriter) {
    use std::io::BufRead;
    use std::sync::mpsc;
    static RX: std::sync::OnceLock<mpsc::Receiver<String>> = std::sync::OnceLock::new();
    let rx = RX.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                if let Ok(l) = line {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
            }
        });
        rx
    });
    while let Ok(line) = rx.try_recv() {
        let parts: Vec<&str> = line.trim().split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        match parts[0] {
            "go" if parts.len() >= 3 => {
                if let (Ok(x), Ok(z)) = (parts[1].parse::<f64>(), parts[2].parse::<f64>()) {
                    bot.cfg.target = (x, z);
                    bot.cfg.mode = Mode::ToPoint;
                    bot.cfg.save();
                    bot.path_dirty = true;
                    bot.best_dist = f64::MAX;
                    println!("Новая цель: X={} Z={} (режим К ТОЧКЕ)", x, z);
                }
            }
            "grow" => {
                bot.cfg.mode = Mode::Grow;
                bot.cfg.save();
                bot.path_dirty = true;
                println!("Режим РОСТ (X и Z растут)");
            }
            "radius" if parts.len() >= 2 => {
                if let Ok(r) = parts[1].parse() {
                    bot.cfg.radius = r;
                    bot.cfg.save();
                    println!("Радиус прибытия: {}", r);
                }
            }
            "status" => println!("{}", bot.status_line()),
            "view" => {
                view.write_html();
                win::open_in_browser(&format!("{}\\view.html", app_dir()));
                println!("Открыта карта. Кликни ОБРАТНО в игру, чтобы клавиши шли туда!");
            }
            "token" if parts.len() >= 2 => {
                let _ = std::fs::write(format!("{}\\hf_token.txt", app_dir()), parts[1]);
                println!("Ключ сохранён в %APPDATA%\\cube_bot\\hf_token.txt (никому его не показывай).");
            }
            "model" if parts.len() >= 2 => {
                bot.ai.set_model(parts[1]);
                println!("Модель: {}", parts[1]);
            }
            "url" if parts.len() >= 2 => {
                bot.ai.set_url(parts[1]);
                println!("Адрес ИИ: обновлён");
            }
            "ai" if parts.len() >= 2 => match parts[1] {
                "on" => {
                    bot.ai.enabled = true;
                    bot.cfg.ai = true;
                    bot.cfg.save();
                    println!("ИИ-помощник включён");
                }
                "off" => {
                    bot.ai.enabled = false;
                    bot.cfg.ai = false;
                    bot.cfg.save();
                    println!("ИИ-помощник выключен");
                }
                "gemini" => bot.ai.use_gemini(),
                "test" => {
                    println!("Через 3 с снимаю экран и отправляю запрос...");
                    std::thread::sleep(std::time::Duration::from_millis(3000));
                    bot.snap_game_png();
                    let b64 = bot.read_img_b64();
                    crate::ai::test_now(&b64);
                }
                _ => println!("ai on/off/test/gemini"),
            },
            "help" => crate::config::print_help(),
            _ => println!(
                "Команды: go X Z | grow | radius N | status | view | token КЛЮЧ | model ИМЯ | \
                 ai on/off/test/gemini | url АДРЕС | help"
            ),
        }
    }
}
