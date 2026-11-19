// Поиск пути A* по клеткам карты (в духе Baritone).
// Неизвестное - проходимо, стены - нет, перепад высоты >1 вверх / >4 вниз - нет.
#![allow(dead_code)]
use std::collections::{BinaryHeap, HashMap};

use crate::map::WorldMap;

const MAX_NODES: usize = 20_000;

#[derive(PartialEq)]
struct Node {
    c: (i32, i32),
    f: f64,
}
impl Ord for Node {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        o.f.total_cmp(&self.f)
    }
}
impl PartialOrd for Node {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Eq for Node {}

fn heur(a: (i32, i32), b: (i32, i32)) -> f64 {
    let dx = (a.0 - b.0).abs() as f64;
    let dz = (a.1 - b.1).abs() as f64;
    (dx * dx + dz * dz).sqrt()
}

/// A* от start до goal. Ограничение по числу узлов (ночь, большие расстояния).
pub fn astar(map: &WorldMap, start: (i32, i32), goal: (i32, i32), now: u64) -> Vec<(i32, i32)> {
    if start == goal {
        return vec![start];
    }
    let mut g: HashMap<(i32, i32), f64> = HashMap::new();
    let mut came: HashMap<(i32, i32), (i32, i32)> = HashMap::new();
    let mut open = BinaryHeap::new();
    g.insert(start, 0.0);
    open.push(Node { c: start, f: heur(start, goal) });
    let mut visited = 0usize;

    while let Some(cur) = open.pop() {
        if cur.c == goal {
            let mut path = vec![goal];
            let mut k = goal;
            while let Some(&p) = came.get(&k) {
                path.push(p);
                k = p;
            }
            path.reverse();
            return path;
        }
        let cg = match g.get(&cur.c) {
            Some(v) => *v,
            None => continue,
        };
        if cg > cur.f + 1e-9 && false {
            continue; // ленивое удаление устаревших записей не критично
        }
        visited += 1;
        if visited > MAX_NODES {
            break;
        }
        for (dx, dz) in [
            (1, 0), (-1, 0), (0, 1), (0, -1),
            (1, 1), (1, -1), (-1, 1), (-1, -1),
        ] {
            let n = (cur.c.0 + dx, cur.c.1 + dz);
            if !map.passable(cur.c, n, now) {
                continue;
            }
            // диагонали только если оба соседних ортогональных проходимы
            if dx != 0 && dz != 0 {
                if !map.passable(cur.c, (cur.c.0 + dx, cur.c.1), now)
                    || !map.passable((cur.c.0 + dx, cur.c.1), n, now)
                {
                    continue;
                }
            }
            let step = if dx != 0 && dz != 0 { 1.414 } else { 1.0 };
            let ng = cg + step + map.cost_extra(n, now);
            if ng < *g.get(&n).unwrap_or(&f64::INFINITY) {
                g.insert(n, ng);
                came.insert(n, cur.c);
                open.push(Node { c: n, f: ng + heur(n, goal) });
            }
        }
    }
    // Путь не найден: вернём самую близкую к цели посещённую проходимую клетку как промежуточную.
    let mut best: Option<((i32, i32), f64)> = None;
    for (&c, &gg) in &g {
        let d = heur(c, goal) + gg * 0.01;
        if best.map(|(_, bd)| d < bd).unwrap_or(true) {
            best = Some((c, d));
        }
    }
    if let Some((b, _)) = best {
        if b != start {
            // восстановим путь до b
            let mut path = vec![b];
            let mut k = b;
            while let Some(&p) = came.get(&k) {
                path.push(p);
                k = p;
            }
            path.reverse();
            return path;
        }
    }
    Vec::new()
}

/// Самая дальняя точка пути (до `max_cells` вперёд), до которой прямая от бота
/// не пересекает стен - в неё бот рулит.
pub fn steer_target(
    map: &WorldMap,
    bot: (i32, i32),
    path: &[(i32, i32)],
    max_cells: usize,
    now: u64,
) -> Option<(i32, i32)> {
    let mut res: Option<(i32, i32)> = None;
    for (i, &p) in path.iter().enumerate().skip(1).take(max_cells) {
        if line_blocked(map, bot, p, now) {
            // если первый же сегмент перекрыт - рулим на него всё равно вдоль пути
            if res.is_none() && i == 1 {
                res = Some(p);
            }
            break;
        }
        res = Some(p);
    }
    res.or_else(|| path.get(1).copied())
}

fn line_blocked(map: &WorldMap, a: (i32, i32), b: (i32, i32), now: u64) -> bool {
    let steps = ((b.0 - a.0).abs()).max((b.1 - a.1).abs()) * 2 + 1;
    for s in 1..steps {
        let t = s as f64 / steps as f64;
        let x = (a.0 as f64 + (b.0 - a.0) as f64 * t).round() as i32;
        let z = (a.1 as f64 + (b.1 - a.1) as f64 * t).round() as i32;
        if matches!(map.get(x, z), crate::map::Cell::Wall { until } if until > now) {
            return true;
        }
    }
    false
}
