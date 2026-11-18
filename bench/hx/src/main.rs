// usage: hx bench <file> <unsafe 0|1> <cs> <warm> <n> [threads]  -> prints n per-decode times (ms), space separated
// usage: hx dec <files...>  (writes .zune.ppm like harness; opts from env HX_THREADS)
use std::fs;
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;
fn cs_of(s: &str) -> ColorSpace {
    match s { "luma" => ColorSpace::Luma, "rgba" => ColorSpace::RGBA, "bgr" => ColorSpace::BGR, "bgra" => ColorSpace::BGRA, _ => ColorSpace::RGB }
}
fn opts(cs: ColorSpace, uns: bool) -> DecoderOptions {
    DecoderOptions::default().jpeg_set_out_colorspace(cs).set_use_unsafe(uns).jpeg_set_threads(std::env::var("HX_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(1))
}
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args[0] == "bench" {
        let data = fs::read(&args[1]).unwrap();
        let uns = args[2] == "1";
        let cs = cs_of(&args[3]);
        let warm: usize = args[4].parse().unwrap();
        let n: usize = args[5].parse().unwrap();
        let mut out = String::new();
        for i in 0..warm + n {
            let t = std::time::Instant::now();
            let mut d = JpegDecoder::new_with_options(ZCursor::new(&data), opts(cs, uns));
            let p = d.decode().unwrap();
            std::hint::black_box(&p);
            let e = t.elapsed().as_secs_f64() * 1000.0;
            if i >= warm { out.push_str(&format!("{e:.4} ")); }
        }
        println!("{out}");
        return;
    }
    for f in &args[1..] {
        let data = fs::read(f).unwrap();
        let r = std::panic::catch_unwind(|| {
            let mut d = JpegDecoder::new_with_options(ZCursor::new(&data), opts(ColorSpace::RGB, true));
            let p = d.decode();
            let i = d.info();
            (p, i)
        });
        match r {
            Ok((Ok(p), Some(i))) => {
                let mut o = format!("P6\n{} {}\n255\n", i.width, i.height).into_bytes();
                o.extend_from_slice(&p);
                fs::write(format!("{f}.zune.ppm"), o).unwrap();
            }
            Ok((Err(e), _)) => fs::write(format!("{f}.zune.err"), format!("{e:?}")).unwrap(),
            Ok(_) => fs::write(format!("{f}.zune.err"), "noinfo").unwrap(),
            Err(_) => fs::write(format!("{f}.zune.err"), "PANIC").unwrap(),
        }
    }
}
