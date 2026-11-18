// ИИ-помощник: снимок игры -> vision-модель (Hugging Face или Google Gemini,
// OpenAI-совместимый /chat/completions) -> совет "TURN=угол".
// Запросы идут через curl.exe (есть в Windows 10/11), чтобы не тащить TLS-стек.
#![allow(dead_code)]
use std::io::Write;
use std::process::{Command, Stdio};

use crate::config::app_dir;

pub struct Ai {
    pub enabled: bool,
    pub url: String,
    pub model: String,
    last_call: f64,
}

const HF_URL: &str = "https://router.huggingface.co/v1/chat/completions";
const GEMINI_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions";
const DEFAULT_MODEL: &str = "Qwen/Qwen2.5-VL-72B-Instruct";

impl Ai {
    pub fn new(enabled: bool) -> Ai {
        Ai { enabled, url: HF_URL.into(), model: DEFAULT_MODEL.into(), last_call: -999.0 }
    }

    pub fn use_gemini(&mut self) {
        self.url = GEMINI_URL.into();
        self.model = "gemini-2.0-flash".into();
        println!("ИИ: Google Gemini (OpenAI-совместимый адрес). Ключ - тот же token ...");
    }
    pub fn set_url(&mut self, u: &str) {
        self.url = u.to_string();
    }
    pub fn set_model(&mut self, m: &str) {
        self.model = m.to_string();
    }

    fn token() -> Option<String> {
        if let Ok(t) = std::env::var("HF_TOKEN") {
            if !t.is_empty() {
                return Some(t);
            }
        }
        std::fs::read_to_string(format!("{}\\hf_token.txt", app_dir()))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Не чаще раза в 30 с. png_b64 - снимок игры в base64. Возвращает угол поворота в градусах.
    pub fn ask(&mut self, now: f64, png_b64: &str) -> Option<f64> {
        if !self.enabled || now - self.last_call < 30.0 {
            return None;
        }
        let token = Self::token()?;
        self.last_call = now;
        let body = format!(
            "{{\"model\":\"{}\",\"max_tokens\":60,\"messages\":[{{\"role\":\"user\",\"content\":[\
{{\"type\":\"text\",\"text\":\"Это скриншот от первого лица из кубической игры. Мне нужно дойти до цели прямо передо мной, но впереди препятствие. Ответь ОДНОЙ строкой вида TURN=<число>, где число - на сколько градусов повернуть камеру вправо (отрицательное - влево), чтобы найти обход. Углы: 0..180 вправо, 0..-180 влево.\"}},\
{{\"type\":\"image_url\",\"image_url\":{{\"url\":\"data:image/png;base64,{}\"}}}}]}}]}}",
            self.model, png_b64
        );
        let out = curl_post(&self.url, &token, &body)?;
        // вытащить TURN=число из ответа модели
        let re_start = out.find("TURN")?;
        let bytes: Vec<u8> = out.as_bytes()[re_start..].iter().copied().collect();
        let txt = String::from_utf8_lossy(&bytes);
        let after = txt.get(4..).unwrap_or("");
        let mut num = String::new();
        for c in after.chars() {
            if c == '-' || c == '+' || c.is_ascii_digit() || c == '.' {
                num.push(c);
            } else if !num.is_empty() {
                break;
            }
        }
        let deg: f64 = num.trim_start_matches('+').parse().ok()?;
        Some(deg.clamp(-180.0, 180.0))
    }
}

fn curl_post(url: &str, token: &str, body: &str) -> Option<String> {
    let tmp = format!("{}\\ai_body.json", app_dir());
    std::fs::write(&tmp, body).ok()?;
    let mut cmd = Command::new("curl.exe");
    cmd.args([
        "-s",
        "--max-time",
        "25",
        "-X",
        "POST",
        url,
        "-H",
        &format!("Authorization: Bearer {}", token),
        "-H",
        "Content-Type: application/json",
        "--data-binary",
        &format!("@{}", tmp),
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    let out = cmd.output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Проверка связи: `ai test`
pub fn test_now(png_b64: &str) {
    let mut ai = Ai::new(true);
    ai.last_call = -999.0;
    match ai.ask(0.0, png_b64) {
        Some(d) => println!("ИИ ответил: TURN={:.0}", d),
        None => println!("ИИ: нет ответа (проверь token / интернет / лимиты кредитов). Снимок: %APPDATA%\\cube_bot\\ai_last.png"),
    }
}

#[allow(dead_code)]
fn _keep(w: &mut dyn Write) {
    let _ = w.write_all(b"");
}
