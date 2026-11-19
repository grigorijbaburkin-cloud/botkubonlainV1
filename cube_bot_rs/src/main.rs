// КУБ БОТ (Rust) - бот по координатам для игры "Куб Онлайн" (Яндекс Игры).
// Работает только на Windows 10/11. Собери: build.bat (cargo build --release).
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod win;
#[cfg(windows)]
mod input;
#[cfg(windows)]
mod reader;
#[cfg(windows)]
mod config;
#[cfg(windows)]
mod map;
#[cfg(windows)]
mod path;
#[cfg(windows)]
mod view;
#[cfg(windows)]
mod ai;
#[cfg(windows)]
mod bot;

#[cfg(windows)]
fn main() {
    // Консольное окно:AllocConsole даёт лог, если запустили двойным кликом.
    win::attach_or_alloc_console();
    let cfg = config::Config::load_from_args();
    bot::run(cfg);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("КУБ БОТ работает только на Windows 10/11.");
    eprintln!("Собери его на Windows: cube_bot_rs> build.bat");
    std::process::exit(1);
}
