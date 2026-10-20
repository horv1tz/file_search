//! Собственный OCR-движок: PaddleOCR PP-OCRv5 (детектор DBNet + CTC-распознаватель) на чистом Rust.
//!
//! Единственная зависимость: `rten` (+ `rten-tensor`; чистый Rust, MIT/Apache-2.0). Без C/C++, без DLL.
//! Вход: RGBA-пиксели страницы. Выход: текст, строки через '\n', порядок чтения
//! сверху вниз / слева направо, колонки — одна за другой.
//!
//! `Engine: Send + Sync` — один экземпляр можно разделять между потоками (`Arc<Engine>`);
//! вызовы `recognize` из разных потоков независимы.

use rten::{Model, RunOptions, ThreadPool};
use rten_tensor::prelude::*;
use rten_tensor::{NdTensor, Tensor};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Ошибка движка (реализует `std::error::Error`, поэтому `?` работает с `anyhow`).
#[derive(Debug, Clone)]
pub struct OcrError(pub String);

impl std::fmt::Display for OcrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for OcrError {}
impl From<String> for OcrError {
    fn from(s: String) -> Self {
        OcrError(s)
    }
}
impl From<&str> for OcrError {
    fn from(s: &str) -> Self {
        OcrError(s.to_string())
    }
}

pub type Result<T> = std::result::Result<T, OcrError>;

fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(OcrError(msg.into()))
}

/// Настройки (значения по умолчанию подобраны на тестовом наборе).
#[derive(Clone, Debug)]
pub struct Options {
    /// Длинная сторона входа детектора (px). Больше — точнее на мелком тексте, но медленнее.
    pub det_long_side: u32,
    /// Порог бинаризации карты вероятностей детектора.
    pub det_thresh: f32,
    /// Минимальная средняя вероятность в компоненте (отсев мусора).
    pub box_thresh: f32,
    /// Коэффициент расширения рамок (unclip).
    pub unclip: f32,
    /// Минимальная средняя уверенность распознавателя для строки (отсев мусора на картинках).
    pub min_line_conf: f32,
    /// Число потоков внутри одного вызова `recognize` (пул rten + параллельное распознавание строк).
    /// 0 — взять min(2, ядра). Параллельные вызовы `recognize` из разных потоков делят этот же пул.
    pub threads: usize,
    /// Исправлять путаницу латиница/кириллица и регистра внутри слов (полезно для поиска).
    pub fix_script: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            det_long_side: 1280,
            det_thresh: 0.3,
            box_thresh: 0.6,
            unclip: 1.5,
            min_line_conf: 0.5,
            threads: 0,
            fix_script: true,
        }
    }
}

pub struct Engine {
    det: Model,
    rec: Model,
    dict: Vec<String>,
    opts: Options,
    /// Пул потоков для внутренних вычислений модели (None — общий пул rten).
    pool: Option<Arc<ThreadPool>>,
}

/// Высота входа распознавателя (зашита в модель PP-OCRv5).
const REC_H: usize = 48;
const MAX_REC_W: usize = 4000;

impl Engine {
    pub fn load(det: &[u8], rec: &[u8], dict: &[u8]) -> Result<Engine> {
        Self::load_with(det, rec, dict, Options::default())
    }

    pub fn load_with(det: &[u8], rec: &[u8], dict: &[u8], opts: Options) -> Result<Engine> {
        let det = Model::load(det.to_vec()).map_err(|e| format!("детектор: {e}"))?;
        let rec = Model::load(rec.to_vec()).map_err(|e| format!("распознаватель: {e}"))?;
        let text = std::str::from_utf8(dict).map_err(|e| format!("словарь не UTF-8: {e}"))?;
        let mut dict: Vec<String> = text.split('\n').map(|l| l.trim_end_matches('\r').to_string()).collect();
        if dict.last().is_some_and(|l| l.is_empty()) {
            dict.pop();
        }
        if dict.is_empty() {
            return err("пустой словарь");
        }
        let mut opts = opts;
        if opts.threads == 0 {
            opts.threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(2);
        }
        let pool = Some(Arc::new(ThreadPool::with_num_threads(opts.threads)));
        let e = Engine { det, rec, dict, opts, pool };
        // проверка совместимости модели и словаря
        let (shape, _) = e.run(&e.rec, vec![0.0; 3 * REC_H * 160], [1, 3, REC_H, 160])?;
        if shape.len() != 3 {
            return err(format!("неожиданная форма выхода распознавателя: {shape:?}"));
        }
        let classes = shape[2];
        // blank + словарь (+ пробел в конце)
        if classes != e.dict.len() + 1 && classes != e.dict.len() + 2 {
            return err(format!("словарь ({}) не соответствует модели ({} классов)", e.dict.len(), classes));
        }
        Ok(e)
    }

    fn run(&self, m: &Model, data: Vec<f32>, shape: [usize; 4]) -> Result<(Vec<usize>, Vec<f32>)> {
        let x = NdTensor::<f32, 4>::from_data(shape, data);
        let opts = RunOptions::default().with_thread_pool(self.pool.clone());
        let out = m.run_one(x.view().into(), Some(opts)).map_err(|e| format!("инференс: {e}"))?;
        let t: Tensor<f32> = out.try_into().map_err(|_| "неожиданный тип выхода")?;
        Ok((t.shape().to_vec(), t.to_vec()))
    }

    /// Распознать страницу. `rgba` — width*height*4 байт.
    pub fn recognize(&self, rgba: &[u8], width: u32, height: u32) -> Result<String> {
        let (w, h) = (width as usize, height as usize);
        if w == 0 || h == 0 || rgba.len() < w * h * 4 {
            return err("неверный размер изображения");
        }
        if w < 16 || h < 16 {
            return Ok(String::new());
        }
        let img = Image { rgba, w, h };
        let boxes = self.detect(&img)?;
        if boxes.is_empty() {
            return Ok(String::new());
        }
        let texts = self.recognize_boxes(&img, &boxes)?;
        let mut lines: Vec<Line> = Vec::new();
        for (b, t) in boxes.iter().zip(texts) {
            if let Some((text, conf)) = t {
                // одиночные цифры/знаки с невысокой уверенностью — типичный мусор на картинках
                let short_junk =
                    text.trim().chars().count() <= 2 && !text.chars().any(|c| c.is_alphabetic()) && conf < 0.7;
                if conf >= self.opts.min_line_conf && !short_junk && !text.trim().is_empty() {
                    let text = if self.opts.fix_script { fix_text(&text) } else { text };
                    lines.push(Line { b: *b, text });
                }
            }
        }
        Ok(assemble(lines))
    }

    // ---------------------------------------------------------------- детектор
    fn detect(&self, img: &Image) -> Result<Vec<RBox>> {
        let long = img.w.max(img.h) as f32;
        let target = self.opts.det_long_side as f32;
        // уменьшаем большие страницы, мелкие (до ~900 px) увеличиваем не более чем вдвое
        let mut scale = target / long;
        if scale > 1.0 {
            scale = scale.min(2.0);
        }
        let dw = (((img.w as f32 * scale) / 32.0).round().max(1.0) as usize) * 32;
        let dh = (((img.h as f32 * scale) / 32.0).round().max(1.0) as usize) * 32;
        let sx = dw as f32 / img.w as f32;
        let sy = dh as f32 / img.h as f32;
        let data = resize_to_chw(img, dw, dh, [0.485, 0.456, 0.406], [0.229, 0.224, 0.225], true);
        let (shape, prob) = self.run(&self.det, data, [1, 3, dh, dw])?;
        if shape.len() < 2 {
            return err("неожиданный выход детектора");
        }
        let (oh, ow) = (shape[shape.len() - 2], shape[shape.len() - 1]);
        // карта может быть чуть другого размера, чем вход
        let (ax, ay) = (ow as f32 / img.w as f32, oh as f32 / img.h as f32);
        let _ = (sx, sy);
        Ok(boxes_from_prob(&prob, ow, oh, ax, ay, &self.opts))
    }

    // ---------------------------------------------------------- распознавание
    fn recognize_boxes(&self, img: &Image, boxes: &[RBox]) -> Result<Vec<Option<(String, f32)>>> {
        // длинные строки сначала — лучше балансировка по потокам
        let mut order: Vec<usize> = (0..boxes.len()).collect();
        order.sort_by(|&a, &b| boxes[b].w.partial_cmp(&boxes[a].w).unwrap_or(std::cmp::Ordering::Equal));
        let next = AtomicUsize::new(0);
        let results: Mutex<Vec<Option<(String, f32)>>> = Mutex::new(vec![None; boxes.len()]);
        let first_err: Mutex<Option<String>> = Mutex::new(None);
        let work = || {
            loop {
                let k = next.fetch_add(1, Ordering::Relaxed);
                if k >= order.len() {
                    break;
                }
                let idx = order[k];
                match self.recognize_one(img, &boxes[idx]) {
                    Ok(r) => results.lock().unwrap_or_else(|e| e.into_inner())[idx] = r,
                    Err(e) => {
                        *first_err.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.to_string());
                        break;
                    }
                }
            }
        };
        let n = self.opts.threads.max(1).min(boxes.len());
        if n <= 1 {
            work();
        } else {
            std::thread::scope(|s| {
                for _ in 0..n {
                    s.spawn(work);
                }
            });
        }
        if let Some(e) = first_err.into_inner().unwrap_or_else(|e| e.into_inner()) {
            return err(e);
        }
        Ok(results.into_inner().unwrap_or_else(|e| e.into_inner()))
    }

    fn recognize_one(&self, img: &Image, b: &RBox) -> Result<Option<(String, f32)>> {
        // ширина полоски при высоте rec_height
        let rh = REC_H;
        let rh0 = rh;
        let ratio = (b.w / b.h.max(1.0)).max(0.2);
        let mut cw = (rh0 as f32 * ratio).ceil() as usize;
        cw = cw.max(16).div_ceil(8) * 8;
        let max_w = MAX_REC_W;
        if cw > max_w {
            cw = max_w; // очень длинную строку сжимаем по горизонтали
        }
        let bucket = cw;
        let strip = crop_strip(img, b, cw, rh);
        let mut data = vec![0f32; 3 * rh * bucket];
        for c in 0..3 {
            for y in 0..rh {
                let dst = &mut data[(c * rh + y) * bucket..(c * rh + y) * bucket + cw];
                let src = &strip[(c * rh + y) * cw..(c * rh + y) * cw + cw];
                for (d, s) in dst.iter_mut().zip(src) {
                    *d = (*s / 255.0 - 0.5) / 0.5;
                }
            }
        }
        let (sh, flat) = self.run(&self.rec, data, [1, 3, rh, bucket])?;
        let (t, c) = (sh[1], sh[2]);
        let valid_t = t;
        Ok(Some(self.ctc_decode(&flat, valid_t, c)))
    }

    fn ctc_decode(&self, logits: &[f32], t: usize, c: usize) -> (String, f32) {
        let mut text = String::new();
        let mut prev = 0usize;
        let (mut sum, mut n) = (0f32, 0usize);
        for i in 0..t {
            let row = &logits[i * c..(i + 1) * c];
            let (mut best, mut bv) = (0usize, f32::MIN);
            for (k, &v) in row.iter().enumerate() {
                if v > bv {
                    bv = v;
                    best = k;
                }
            }
            if best != 0 && best != prev {
                // выход модели — уже softmax; на случай логитов нормируем
                let p = if (0.0..=1.0).contains(&bv) { bv } else { softmax_at(row, best) };
                if best - 1 < self.dict.len() {
                    text.push_str(&self.dict[best - 1]);
                } else {
                    text.push(' ');
                }
                sum += p;
                n += 1;
            }
            prev = best;
        }
        let conf = if n == 0 { 0.0 } else { sum / n as f32 };
        (text, conf)
    }
}

fn softmax_at(row: &[f32], k: usize) -> f32 {
    let m = row.iter().cloned().fold(f32::MIN, f32::max);
    let s: f32 = row.iter().map(|v| (v - m).exp()).sum();
    (row[k] - m).exp() / s
}

// ------------------------------------------------------------------ изображение
struct Image<'a> {
    rgba: &'a [u8],
    w: usize,
    h: usize,
}

impl Image<'_> {
    #[inline]
    fn px(&self, x: usize, y: usize) -> [f32; 3] {
        let i = (y * self.w + x) * 4;
        let a = self.rgba[i + 3] as f32 / 255.0;
        // прозрачное считаем белым
        let f = |v: u8| v as f32 * a + 255.0 * (1.0 - a);
        // порядок BGR — так модели PaddleOCR обучались (OpenCV)
        [f(self.rgba[i + 2]), f(self.rgba[i + 1]), f(self.rgba[i])]
    }
    #[inline]
    fn sample(&self, x: f32, y: f32) -> [f32; 3] {
        let x = x.clamp(0.0, (self.w - 1) as f32);
        let y = y.clamp(0.0, (self.h - 1) as f32);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let (p00, p10, p01, p11) = (self.px(x0, y0), self.px(x1, y0), self.px(x0, y1), self.px(x1, y1));
        let mut o = [0f32; 3];
        for c in 0..3 {
            o[c] = (p00[c] * (1.0 - fx) + p10[c] * fx) * (1.0 - fy) + (p01[c] * (1.0 - fx) + p11[c] * fx) * fy;
        }
        o
    }
}

/// Сепарабельное уменьшение/увеличение с антиалиасингом (треугольное ядро), результат — CHW, нормализованный.
fn resize_to_chw(img: &Image, dw: usize, dh: usize, mean: [f32; 3], std: [f32; 3], _rgb: bool) -> Vec<f32> {
    let weights = |src: usize, dst: usize| -> Vec<(usize, Vec<f32>)> {
        let scale = src as f32 / dst as f32;
        let support = scale.max(1.0);
        (0..dst)
            .map(|i| {
                let center = (i as f32 + 0.5) * scale;
                let lo = ((center - support).floor().max(0.0)) as usize;
                let hi = ((center + support).ceil() as usize).min(src);
                let mut ws: Vec<f32> =
                    (lo..hi).map(|j| (1.0 - ((j as f32 + 0.5 - center) / support).abs()).max(0.0)).collect();
                let s: f32 = ws.iter().sum();
                if s > 0.0 {
                    ws.iter_mut().for_each(|w| *w /= s);
                } else {
                    ws = vec![1.0];
                }
                (lo, ws)
            })
            .collect()
    };
    let wx = weights(img.w, dw);
    let wy = weights(img.h, dh);
    // горизонтальный проход для нужных строк с кэшем
    let mut tmp = vec![0f32; img.h * dw * 3];
    // считаем только те строки исходника, которые нужны
    let mut need = vec![false; img.h];
    for (lo, ws) in &wy {
        for k in 0..ws.len() {
            need[lo + k] = true;
        }
    }
    for (y, _) in need.iter().enumerate().filter(|(_, n)| **n) {
        for (x, (lo, ws)) in wx.iter().enumerate() {
            let mut acc = [0f32; 3];
            for (k, w) in ws.iter().enumerate() {
                let p = img.px(lo + k, y);
                acc[0] += p[0] * w;
                acc[1] += p[1] * w;
                acc[2] += p[2] * w;
            }
            let o = (y * dw + x) * 3;
            tmp[o] = acc[0];
            tmp[o + 1] = acc[1];
            tmp[o + 2] = acc[2];
        }
    }
    let mut out = vec![0f32; 3 * dh * dw];
    for (y, (lo, ws)) in wy.iter().enumerate() {
        for x in 0..dw {
            let mut acc = [0f32; 3];
            for (k, w) in ws.iter().enumerate() {
                let o = ((lo + k) * dw + x) * 3;
                acc[0] += tmp[o] * w;
                acc[1] += tmp[o + 1] * w;
                acc[2] += tmp[o + 2] * w;
            }
            for c in 0..3 {
                out[(c * dh + y) * dw + x] = (acc[c] / 255.0 - mean[c]) / std[c];
            }
        }
    }
    out
}

// ------------------------------------------------------------------ геометрия
#[derive(Clone, Copy, Debug)]
struct RBox {
    cx: f32,
    cy: f32,
    /// длина вдоль направления текста / высота
    w: f32,
    h: f32,
    /// размеры до расширения (по самому тексту) — для порядка чтения
    tw: f32,
    th: f32,
    /// угол направления текста (рад), в (-pi/4, pi/4]
    ang: f32,
}

/// Из карты вероятностей — повёрнутые рамки строк в координатах исходного изображения.
fn boxes_from_prob(prob: &[f32], ow: usize, oh: usize, ax: f32, ay: f32, o: &Options) -> Vec<RBox> {
    let mask: Vec<bool> = prob.iter().map(|&p| p > o.det_thresh).collect();
    let mut seen = vec![false; ow * oh];
    let mut out = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for start in 0..ow * oh {
        if !mask[start] || seen[start] {
            continue;
        }
        // компонента связности (8 соседей)
        let mut count = 0usize;
        let mut psum = 0f32;
        // для выпуклой оболочки хватает крайних точек строк
        let mut rows: HashMap<usize, (usize, usize)> = HashMap::new();
        seen[start] = true;
        stack.push(start);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % ow, i / ow);
            count += 1;
            psum += prob[i];
            let e = rows.entry(y).or_insert((x, x));
            e.0 = e.0.min(x);
            e.1 = e.1.max(x);
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= ow as i32 || ny >= oh as i32 {
                        continue;
                    }
                    let ni = ny as usize * ow + nx as usize;
                    if mask[ni] && !seen[ni] {
                        seen[ni] = true;
                        stack.push(ni);
                    }
                }
            }
        }
        if count < 12 || psum / (count as f32) < o.box_thresh {
            continue;
        }
        let mut pts: Vec<(f32, f32)> = Vec::with_capacity(rows.len() * 2);
        for (y, (x0, x1)) in rows {
            pts.push((x0 as f32, y as f32));
            pts.push((x1 as f32 + 1.0, y as f32 + 1.0));
            pts.push((x0 as f32, y as f32 + 1.0));
            pts.push((x1 as f32 + 1.0, y as f32));
        }
        let hull = convex_hull(pts);
        let Some((cx, cy, mut w, mut h, mut ang)) = min_area_rect(&hull) else { continue };
        // направление текста — ближайшее к горизонтали
        if h > w {
            std::mem::swap(&mut w, &mut h);
            ang += std::f32::consts::FRAC_PI_2;
        }
        while ang > std::f32::consts::FRAC_PI_4 {
            ang -= std::f32::consts::PI / 2.0;
            std::mem::swap(&mut w, &mut h);
        }
        while ang <= -std::f32::consts::FRAC_PI_4 {
            ang += std::f32::consts::PI / 2.0;
            std::mem::swap(&mut w, &mut h);
        }
        if h.min(w) < 3.0 {
            continue;
        }
        // unclip как в DB: d = площадь * ratio / периметр
        let d = w * h * o.unclip / (2.0 * (w + h));
        let (w2, h2) = (w + 2.0 * d, h + 2.0 * d);
        // в исходные координаты (ax, ay почти равны; берём среднее для размеров)
        let s = 2.0 / (ax + ay);
        out.push(RBox { cx: cx / ax, cy: cy / ay, w: w2 * s, h: h2 * s, tw: w * s, th: h * s, ang });
    }
    out
}

fn convex_hull(mut p: Vec<(f32, f32)>) -> Vec<(f32, f32)> {
    p.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    p.dedup();
    if p.len() < 3 {
        return p;
    }
    let cross = |o: (f32, f32), a: (f32, f32), b: (f32, f32)| (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0);
    let mut h: Vec<(f32, f32)> = Vec::new();
    for &q in &p {
        while h.len() >= 2 && cross(h[h.len() - 2], h[h.len() - 1], q) <= 0.0 {
            h.pop();
        }
        h.push(q);
    }
    let lower = h.len() + 1;
    for &q in p.iter().rev().skip(1) {
        while h.len() >= lower && cross(h[h.len() - 2], h[h.len() - 1], q) <= 0.0 {
            h.pop();
        }
        h.push(q);
    }
    h.pop();
    h
}

/// Минимальный по площади описанный прямоугольник: (cx, cy, w, h, угол первой стороны).
fn min_area_rect(hull: &[(f32, f32)]) -> Option<(f32, f32, f32, f32, f32)> {
    if hull.len() < 3 {
        return None;
    }
    let mut best: Option<(f32, f32, f32, f32, f32, f32)> = None;
    for i in 0..hull.len() {
        let a = hull[i];
        let b = hull[(i + 1) % hull.len()];
        let ang = (b.1 - a.1).atan2(b.0 - a.0);
        let (c, s) = (ang.cos(), ang.sin());
        let (mut x0, mut x1, mut y0, mut y1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for p in hull {
            let u = p.0 * c + p.1 * s;
            let v = -p.0 * s + p.1 * c;
            x0 = x0.min(u);
            x1 = x1.max(u);
            y0 = y0.min(v);
            y1 = y1.max(v);
        }
        let area = (x1 - x0) * (y1 - y0);
        if best.is_none_or(|b| area < b.0) {
            let (mu, mv) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
            let (cx, cy) = (mu * c - mv * s, mu * s + mv * c);
            best = Some((area, cx, cy, x1 - x0, y1 - y0, ang));
        }
    }
    best.map(|b| (b.1, b.2, b.3, b.4, b.5))
}

/// Вырезать повёрнутую рамку в полоску cw×ch, формат CHW (0..255).
fn crop_strip(img: &Image, b: &RBox, cw: usize, ch: usize) -> Vec<f32> {
    let (c, s) = (b.ang.cos(), b.ang.sin());
    let mut out = vec![0f32; 3 * ch * cw];
    // при сильном уменьшении усредняем 2x2 подвыборки
    let ss = if b.h > ch as f32 * 1.7 { 2 } else { 1 };
    for y in 0..ch {
        for x in 0..cw {
            let mut acc = [0f32; 3];
            for sy in 0..ss {
                for sx in 0..ss {
                    let u = ((x as f32 + (sx as f32 + 0.5) / ss as f32) / cw as f32 - 0.5) * b.w;
                    let v = ((y as f32 + (sy as f32 + 0.5) / ss as f32) / ch as f32 - 0.5) * b.h;
                    let px = b.cx + u * c - v * s - 0.5;
                    let py = b.cy + u * s + v * c - 0.5;
                    let p = img.sample(px, py);
                    acc[0] += p[0];
                    acc[1] += p[1];
                    acc[2] += p[2];
                }
            }
            let n = (ss * ss) as f32;
            for k in 0..3 {
                out[(k * ch + y) * cw + x] = acc[k] / n;
            }
        }
    }
    out
}

// ------------------------------------------------------------ порядок чтения
struct Line {
    b: RBox,
    text: String,
}

/// Прямоугольник в «выправленных» координатах (поворот страницы убран).
#[derive(Clone)]
struct Item {
    x0: f32,
    x1: f32,
    y0: f32,
    y1: f32,
    text: String,
}

fn assemble(lines: Vec<Line>) -> String {
    if lines.is_empty() {
        return String::new();
    }
    // угол страницы — медиана углов длинных строк
    let mut angs: Vec<f32> = lines.iter().filter(|l| l.b.tw > 3.0 * l.b.th).map(|l| l.b.ang).collect();
    angs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let theta = if angs.is_empty() { 0.0 } else { angs[angs.len() / 2] };
    let (c, s) = ((-theta).cos(), (-theta).sin());
    let items: Vec<Item> = lines
        .into_iter()
        .map(|l| {
            let x = l.b.cx * c - l.b.cy * s;
            let y = l.b.cx * s + l.b.cy * c;
            // высоту берём как есть, а длину — вдоль строки; собственный наклон учтён поворотом
            Item {
                x0: x - l.b.tw / 2.0,
                x1: x + l.b.tw / 2.0,
                y0: y - l.b.th / 2.0,
                y1: y + l.b.th / 2.0,
                text: l.text,
            }
        })
        .collect();
    let mut hs: Vec<f32> = items.iter().map(|i| i.y1 - i.y0).collect();
    hs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let med_h = hs[hs.len() / 2].max(1.0);
    let mut out = Vec::new();
    reading_order(items, med_h, &mut out);
    out.join("\n")
}

fn fcmp(a: &f32, b: &f32) -> std::cmp::Ordering {
    a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
}

fn reading_order(items: Vec<Item>, med_h: f32, out: &mut Vec<String>) {
    if items.is_empty() {
        return;
    }
    let cuts = if items.len() >= 6 { find_gutters(&items, med_h) } else { Vec::new() };
    if cuts.is_empty() {
        return rows_in_order(items, med_h, out);
    }
    // «пересекающие» рамки (заголовки на всю ширину) разделяют текст на блоки по вертикали
    let mut items = items;
    items.sort_by(|a, b| fcmp(&a.y0, &b.y0));
    let mut seg: Vec<Item> = Vec::new();
    let flush = |seg: &mut Vec<Item>, out: &mut Vec<String>| {
        if seg.is_empty() {
            return;
        }
        let mut cols: Vec<Vec<Item>> = vec![Vec::new(); cuts.len() + 1];
        for it in seg.drain(..) {
            let cx = (it.x0 + it.x1) / 2.0;
            let k = cuts.iter().filter(|&&c| cx > c).count();
            cols[k].push(it);
        }
        for col in cols {
            // внутри колонки жёлобов уже нет — читаем по строкам
            rows_in_order(col, med_h, out);
        }
    };
    // строки-перекрытия, лежащие на одной высоте с обычными, сворачиваем: блок = всё между перекрывающими рамками
    let mut spanners: Vec<Item> = Vec::new();
    for it in items {
        let spans = cuts.iter().any(|&c| it.x0 < c && it.x1 > c);
        if spans {
            flush(&mut seg, out);
            spanners.push(it.clone());
            // объединяем подряд идущие перекрытия на одной строке
            rows_in_order(vec![it], med_h, out);
        } else {
            seg.push(it);
        }
    }
    flush(&mut seg, out);
    let _ = spanners;
}

/// Строки по y, внутри строки слева направо (ячейки таблицы склеиваются пробелом).
fn rows_in_order(mut items: Vec<Item>, med_h: f32, out: &mut Vec<String>) {
    items.sort_by(|a, b| fcmp(&((a.y0 + a.y1) / 2.0), &((b.y0 + b.y1) / 2.0)));
    let mut rows: Vec<(f32, Vec<Item>)> = Vec::new();
    for it in items {
        let cy = (it.y0 + it.y1) / 2.0;
        match rows.last_mut() {
            Some((rc, r)) if (cy - *rc).abs() < 0.5 * med_h => {
                r.push(it);
                *rc = r.iter().map(|i| (i.y0 + i.y1) / 2.0).sum::<f32>() / r.len() as f32;
            }
            _ => rows.push((cy, vec![it])),
        }
    }
    for (_, mut r) in rows {
        r.sort_by(|a, b| fcmp(&a.x0, &b.x0));
        out.push(r.iter().map(|i| i.text.as_str()).collect::<Vec<_>>().join(" "));
    }
}

/// Позиции вертикальных жёлобов (колонки текста). Пустой вектор — колонок нет.
/// Жёлоб: полоса шире 1.2 высоты строки, которую пересекает не более 10% рамок (заголовки),
/// причём получающихся колонок не больше 3, и в каждой не меньше 3 длинных строк
/// (таблицы с короткими ячейками колонками не считаются).
fn find_gutters(items: &[Item], med_h: f32) -> Vec<f32> {
    let mut xs: Vec<f32> = items.iter().flat_map(|i| [i.x0, i.x1]).collect();
    xs.sort_by(fcmp);
    xs.dedup();
    let total = xs[xs.len() - 1] - xs[0];
    if total <= 0.0 {
        return Vec::new();
    }
    let allowed = ((items.len() as f32) * 0.1).floor().max(1.0) as usize;
    let mut cuts: Vec<f32> = Vec::new();
    let mut run: Option<(f32, f32)> = None;
    let close = |run: &mut Option<(f32, f32)>, cuts: &mut Vec<f32>| {
        if let Some((a, b)) = run.take()
            && b - a >= 1.2 * med_h
        {
            cuts.push((a + b) / 2.0);
        }
    };
    for w in xs.windows(2) {
        let (a, b) = (w[0], w[1]);
        let cover = items.iter().filter(|i| i.x0 <= a && i.x1 >= b).count();
        if cover <= allowed {
            run = Some(match run {
                Some((ra, _)) => (ra, b),
                None => (a, b),
            });
        } else {
            close(&mut run, &mut cuts);
        }
    }
    close(&mut run, &mut cuts);
    if cuts.is_empty() || cuts.len() > 2 {
        return Vec::new();
    }
    let mut longs = vec![0usize; cuts.len() + 1];
    for it in items {
        if it.x0 < cuts[0] && it.x1 > cuts[0] {
            continue;
        }
        if it.x1 - it.x0 >= 0.28 * total {
            let cx = (it.x0 + it.x1) / 2.0;
            longs[cuts.iter().filter(|&&c| cx > c).count()] += 1;
        }
    }
    if longs.iter().any(|&n| n < 3) {
        return Vec::new();
    }
    cuts
}

// ------------------------------------------------------ постобработка текста
/// Пары «похожих» букв: (латиница, кириллица).
const HOMO: [(char, char); 34] = [
    ('a', 'а'),
    ('c', 'с'),
    ('e', 'е'),
    ('o', 'о'),
    ('p', 'р'),
    ('x', 'х'),
    ('y', 'у'),
    ('k', 'к'),
    ('r', 'г'),
    ('n', 'п'),
    ('t', 'т'),
    ('m', 'м'),
    ('h', 'н'),
    ('i', 'і'),
    ('l', 'ӏ'),
    ('b', 'ь'),
    ('u', 'и'),
    ('A', 'А'),
    ('B', 'В'),
    ('C', 'С'),
    ('E', 'Е'),
    ('H', 'Н'),
    ('K', 'К'),
    ('M', 'М'),
    ('O', 'О'),
    ('P', 'Р'),
    ('T', 'Т'),
    ('X', 'Х'),
    ('Y', 'У'),
    ('I', 'І'),
    ('l', 'Ӏ'),
    ('N', 'П'),
    ('R', 'Г'),
    ('U', 'И'),
];

const SHORT_WORDS: [&str; 34] = [
    "в", "с", "к", "у", "о", "а", "и", "я", "на", "не", "по", "их", "от", "до", "за", "из", "со", "во", "ко", "об",
    "он", "но", "да", "ни", "же", "бы", "ли", "то", "та", "те", "ее", "им", "ей", "ем",
];

/// Возможные кириллические «прототипы» латинской буквы (для коротких слов).
fn short_alts(c: char) -> Option<&'static [char]> {
    Some(match c.to_ascii_lowercase() {
        'a' => &['а'],
        'c' => &['с'],
        'e' => &['е'],
        'o' => &['о'],
        'p' => &['р'],
        'x' => &['х'],
        'y' => &['у'],
        'k' => &['к'],
        'r' => &['г'],
        'n' => &['н', 'п'],
        't' => &['т'],
        'm' => &['м'],
        'h' => &['н'],
        'i' => &['и'],
        'u' => &['и'],
        'b' => &['в', 'ь'],
        _ => return None,
    })
}

fn short_word_candidate(tok: &str) -> Option<String> {
    let mut cands: Vec<String> = vec![String::new()];
    for c in tok.chars() {
        if !c.is_ascii_alphabetic() {
            for w in cands.iter_mut() {
                w.push(c);
            }
            continue;
        }
        let alts = short_alts(c)?;
        cands = cands.iter().flat_map(|w| alts.iter().map(move |a| format!("{w}{a}"))).collect();
    }
    let upper_first = tok.chars().next().is_some_and(|c| c.is_ascii_uppercase());
    cands
        .into_iter()
        .find(|w| SHORT_WORDS.contains(&w.chars().filter(|c| c.is_alphabetic()).collect::<String>().as_str()))
        .map(|w| {
            if upper_first {
                let mut it = w.chars();
                it.next().map(|f| f.to_uppercase().chain(it).collect()).unwrap_or(w)
            } else {
                w
            }
        })
}

fn is_cyr(c: char) -> bool {
    ('\u{0400}'..='\u{04FF}').contains(&c)
}
fn is_lat(c: char) -> bool {
    c.is_ascii_alphabetic()
}
fn to_lat(c: char) -> Option<char> {
    HOMO.iter().find(|h| h.1 == c).map(|h| h.0)
}
fn to_cyr(c: char) -> Option<char> {
    // в русский текст не вносим «палочку» и украинскую «і»
    HOMO.iter().find(|h| h.0 == c && h.1 != 'ӏ' && h.1 != 'Ӏ' && h.1 != 'і' && h.1 != 'І').map(|h| h.1)
}

/// Слово из смеси кириллицы и латиницы приводим к одному алфавиту; «b»->«ь», «u»->«и» (нестрогие) — только когда иначе не получается.
fn fix_token(tok: &str, line_is_cyr: bool) -> String {
    let letters: Vec<char> = tok.chars().filter(|c| is_cyr(*c) || is_lat(*c)).collect();
    if letters.is_empty() {
        return tok.to_string();
    }
    // адреса и ссылки — всегда латиница
    let addr = tok.contains('@') || tok.contains("://") || tok.starts_with("www.");
    let n_cyr = letters.iter().filter(|c| is_cyr(**c)).count();
    let n_lat = letters.len() - n_cyr;
    let mut out = tok.to_string();
    if addr && n_cyr > 0 && letters.iter().filter(|c| is_cyr(**c)).all(|c| to_lat(*c).is_some()) {
        out = tok.chars().map(|c| if is_cyr(c) { to_lat(c).unwrap_or(c) } else { c }).collect();
    } else if n_cyr > 0 && n_lat > 0 {
        let can_lat = letters.iter().filter(|c| is_cyr(**c)).all(|c| to_lat(*c).is_some());
        let can_cyr = letters.iter().filter(|c| is_lat(**c)).all(|c| to_cyr(*c).is_some());
        // латиница, если все кириллические буквы похожи на латинские и (их мало или латиницы больше)
        let to_latin = match (can_lat, can_cyr) {
            (true, false) => true,
            (false, true) => false,
            (true, true) => n_lat >= n_cyr,
            (false, false) => return tok.to_string(),
        };
        out = tok
            .chars()
            .map(|c| {
                if to_latin && is_cyr(c) {
                    to_lat(c).unwrap_or(c)
                } else if !to_latin && is_lat(c) {
                    to_cyr(c).unwrap_or(c)
                } else {
                    c
                }
            })
            .collect();
    } else if n_lat > 0
        && n_cyr == 0
        && line_is_cyr
        && letters.len() <= 3
        && tok.chars().all(|c| !c.is_alphabetic() || short_alts(c).is_some())
    {
        // «na», «ne», «B» в русской строке — только если получается настоящее короткое русское слово
        if let Some(w) = short_word_candidate(tok) {
            out = w;
        }
    }
    out
}

fn fix_text(line: &str) -> String {
    let cyr = line.chars().filter(|c| is_cyr(*c)).count();
    let lat = line.chars().filter(|c| is_lat(*c)).count();
    let line_is_cyr = cyr > lat;
    line.split(' ').map(|t| fix_token(t, line_is_cyr)).collect::<Vec<_>>().join(" ")
}

// Проверка на этапе компиляции: Engine можно разделять между потоками.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Engine>();
};

#[cfg(test)]
mod tests {
    use super::*;

    fn line(cx: f32, cy: f32, w: f32, text: &str) -> Line {
        Line { b: RBox { cx, cy, w, h: 40.0, tw: w, th: 20.0, ang: 0.0 }, text: text.to_string() }
    }

    #[test]
    fn fix_script_mixed_and_short() {
        assert_eq!(fix_text("Сonta"), "Conta"); // кириллическая «С» внутри латинского слова
        assert_eq!(fix_text("hr@tесhпорагk.ехаmрӏе"), "hr@technopark.example");
        assert_eq!(fix_text("мы идем na работу"), "мы идем на работу");
        assert_eq!(fix_text("модель HP для"), "модель HP для"); // латиница не трогается
        assert_eq!(fix_text("Invoice number"), "Invoice number");
    }

    #[test]
    fn rect_of_rotated_box() {
        // прямоугольник 100x20, повёрнутый на 10 градусов
        let (c, s) = (10f32.to_radians().cos(), 10f32.to_radians().sin());
        let mut pts = Vec::new();
        for (x, y) in [(-50.0, -10.0), (50.0, -10.0), (50.0, 10.0), (-50.0, 10.0)] {
            pts.push((200.0 + x * c - y * s, 100.0 + x * s + y * c));
        }
        let hull = convex_hull(pts);
        let (cx, cy, w, h, _) = min_area_rect(&hull).unwrap();
        assert!((cx - 200.0).abs() < 0.1 && (cy - 100.0).abs() < 0.1);
        assert!((w.max(h) - 100.0).abs() < 0.1 && (w.min(h) - 20.0).abs() < 0.1);
    }

    #[test]
    fn reading_order_columns_and_table() {
        // заголовок, затем две колонки по 3 длинные строки: сначала вся левая, потом правая
        let mut v = vec![line(500.0, 50.0, 400.0, "Заголовок")];
        for i in 0..3 {
            v.push(line(250.0, 150.0 + 60.0 * i as f32, 420.0, &format!("L{i}")));
            v.push(line(750.0, 150.0 + 60.0 * i as f32, 420.0, &format!("R{i}")));
        }
        assert_eq!(assemble(v), "Заголовок\nL0\nL1\nL2\nR0\nR1\nR2");
        // таблица: короткие ячейки — читаем по строкам слева направо
        let mut t = Vec::new();
        for r in 0..4 {
            for c in 0..5 {
                t.push(line(100.0 + 200.0 * c as f32, 100.0 + 50.0 * r as f32, 80.0, &format!("{r}{c}")));
            }
        }
        assert!(assemble(t).starts_with("00 01 02 03 04\n10 11 12 13 14"));
    }

    #[test]
    fn empty_input() {
        assert_eq!(assemble(Vec::new()), "");
    }
}
