//! Generates assets/icon-1024.png (run: cargo run --example icon). Pure `image` crate, no assets needed.
use image::{Rgba, RgbaImage};

const N: u32 = 1024;

fn in_rect(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> bool {
    x >= x0 && x <= x1 && y >= y0 && y <= y1
}

fn in_tri(p: (f32, f32), a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let s = |p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)| (p1.0 - p3.0) * (p2.1 - p3.1) - (p2.0 - p3.0) * (p1.1 - p3.1);
    let (d1, d2, d3) = (s(p, a, b), s(p, b, c), s(p, c, a));
    !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
}

fn glyph(x: f32, y: f32) -> bool {
    // down arrow
    in_rect(x, y, -55.0, -270.0, 55.0, 40.0)
        || in_tri((x, y), (-210.0, 0.0), (210.0, 0.0), (0.0, 215.0))
        // tray
        || in_rect(x, y, -290.0, 150.0, -220.0, 290.0)
        || in_rect(x, y, 220.0, 150.0, 290.0, 290.0)
        || in_rect(x, y, -290.0, 235.0, 290.0, 305.0)
}

fn main() {
    let mut img = RgbaImage::new(N, N);
    let half = 412.0f32; // macOS icon grid: 824px squircle inside 1024
    let ss = 3;
    for py in 0..N {
        for px in 0..N {
            let (mut bg, mut fg) = (0.0f32, 0.0f32);
            for sy in 0..ss {
                for sx in 0..ss {
                    let x = px as f32 + (sx as f32 + 0.5) / ss as f32 - N as f32 / 2.0;
                    let y = py as f32 + (sy as f32 + 0.5) / ss as f32 - N as f32 / 2.0;
                    if (x / half).abs().powf(5.0) + (y / half).abs().powf(5.0) <= 1.0 {
                        bg += 1.0;
                        if glyph(x, y) {
                            fg += 1.0;
                        }
                    }
                }
            }
            let k = (ss * ss) as f32;
            let (a, f) = (bg / k, fg / k);
            let t = ((px + py) as f32 / (2.0 * N as f32)).clamp(0.0, 1.0);
            let lerp = |a: f32, b: f32| a + (b - a) * t;
            let base = [lerp(99.0, 236.0), lerp(102.0, 72.0), lerp(241.0, 153.0)]; // indigo -> pink
            let mut c = [0u8; 4];
            for i in 0..3 {
                let v = if bg > 0.0 { base[i] * (1.0 - f / (bg / k).max(1e-6)).max(0.0) + 255.0 * (f / a.max(1e-6)).min(1.0) } else { 0.0 };
                c[i] = v.clamp(0.0, 255.0) as u8;
            }
            c[3] = (a * 255.0) as u8;
            img.put_pixel(px, py, Rgba(c));
        }
    }
    img.save("assets/icon-1024.png").unwrap();
}
