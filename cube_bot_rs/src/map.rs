// Карта мира в клетках 1x1 (как у Baritone, но строится самим ботом).
// - посещённые клетки запоминаются с высотой Y (значит проходимы);
// - при упоре полоса клеток впереди помечается стеной;
// - отметки "стена" забываются через 4 минуты;
// - карта сохраняется на диск (map.txt) раз в минуту и при остановке.
#![allow(dead_code)]
use std::collections::HashMap;

use crate::reader::Pos;

#[derive(Clone, Copy, PartialEq)]
pub enum Cell {
    Unknown,
    Open { y: f64 }, // стояли здесь - проходимо
    Wall { until: u64 }, // стена до момента (секунды uptime)
}

pub struct WorldMap {
    cells: HashMap<(i32, i32), Cell>,
}

impl WorldMap {
    pub fn new() -> Self {
        WorldMap { cells: HashMap::new() }
    }

    pub fn get(&self, gx: i32, gz: i32) -> Cell {
        self.cells.get(&(gx, gz)).copied().unwrap_or(Cell::Unknown)
    }

    pub fn mark_open(&mut self, p: &Pos, now: u64) {
        let key = cell_of(p);
        // не перетираем свежие стены посещением (маловероятно, но бережно)
        if let Some(Cell::Wall { until }) = self.cells.get(&key) {
            if *until > now {
                return;
            }
        }
        self.cells.insert(key, Cell::Open { y: p.y });
    }

    /// Упор: отметить полосу шириной `width` (нечётная) на `depth` клеток впереди
    /// вдоль направления heading (в градусах, 0 = +X? нет: см. coords ниже).
    pub fn mark_wall(&mut self, p: &Pos, dir_x: f64, dir_z: f64, width: i32, depth: i32, now: u64) {
        // перпендикуляр
        let px = -dir_z;
        let pz = dir_x;
        let half = width / 2;
        for d in 1..=depth {
            for s in -half..=half {
                let wx = p.x + dir_x * d as f64 + px * s as f64;
                let wz = p.z + dir_z * d as f64 + pz * s as f64;
                let key = (wx.floor() as i32, wz.floor() as i32);
                self.cells.insert(key, Cell::Wall { until: now + 240 });
            }
        }
    }

    pub fn forget_walls(&mut self) {
        self.cells.retain(|_, c| !matches!(c, Cell::Wall { .. }));
    }

    pub fn expire(&mut self, now: u64) {
        self.cells.retain(|_, c| match c {
            Cell::Wall { until } => *until > now,
            _ => true,
        });
    }

    /// Проходимость перехода a->b с учётом высоты: вверх >1 блока нельзя, вниз >4 нельзя.
    pub fn passable(&self, from: (i32, i32), to: (i32, i32), now: u64) -> bool {
        match self.get(to.0, to.1) {
            Cell::Wall { until } if until > now => false,
            Cell::Unknown => true,
            _ => {
                if let (Cell::Open { y: ya }, Cell::Open { y: yb }) =
                    (self.get(from.0, from.1), self.get(to.0, to.1))
                {
                    let dy = yb - ya;
                    if dy > 1.0 || dy < -4.0 {
                        return false;
                    }
                }
                true
            }
        }
    }

    pub fn cost_extra(&self, c: (i32, i32), now: u64) -> f64 {
        match self.get(c.0, c.1) {
            Cell::Open { .. } => -0.05, // пройденное чуть дешевле
            Cell::Wall { .. } => 6.0,   // штраф около стен (просачивание)
            Cell::Unknown => 0.0,
        }
    }

    pub fn save(&self, path: &str) {
        let mut s = String::with_capacity(self.cells.len() * 16);
        for ((gx, gz), c) in &self.cells {
            match c {
                Cell::Open { y } => s.push_str(&format!("O {} {} {:.1}\n", gx, gz, y)),
                Cell::Wall { .. } => s.push_str(&format!("W {} {}\n", gx, gz)),
                Cell::Unknown => {}
            }
        }
        let _ = std::fs::write(path, s);
    }

    pub fn load(&mut self, path: &str, now: u64) -> usize {
        let mut n = 0;
        if let Ok(t) = std::fs::read_to_string(path) {
            for line in t.lines() {
                let mut it = line.split_whitespace();
                match it.next() {
                    Some("O") => {
                        if let (Some(x), Some(z), Some(y)) = (
                            it.next().and_then(|v| v.parse::<i32>().ok()),
                            it.next().and_then(|v| v.parse::<i32>().ok()),
                            it.next().and_then(|v| v.parse::<f64>().ok()),
                        ) {
                            self.cells.insert((x, z), Cell::Open { y });
                            n += 1;
                        }
                    }
                    Some("W") => {
                        if let (Some(x), Some(z)) = (
                            it.next().and_then(|v| v.parse::<i32>().ok()),
                            it.next().and_then(|v| v.parse::<i32>().ok()),
                        ) {
                            self.cells.insert((x, z), Cell::Wall { until: now + 240 });
                            n += 1;
                        }
                    }
                    _ => {}
                }
            }
        }
        n
    }

    pub fn walls(&self, now: u64) -> Vec<(i32, i32)> {
        self.cells
            .iter()
            .filter(|(_, c)| matches!(c, Cell::Wall { until } if *until > now))
            .map(|(k, _)| *k)
            .collect()
    }

    pub fn opens(&self) -> Vec<(i32, i32, f64)> {
        self.cells
            .iter()
            .filter_map(|(k, c)| match c {
                Cell::Open { y } => Some((k.0, k.1, *y)),
                _ => None,
            })
            .collect()
    }
}

pub fn cell_of(p: &Pos) -> (i32, i32) {
    (p.x.floor() as i32, p.z.floor() as i32)
}
