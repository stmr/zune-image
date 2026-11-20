//! Multi-threaded progressive decoding must be bit-identical to single-threaded decoding.
#![cfg(feature = "std")]

use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

fn decode(data: &[u8], cs: ColorSpace, threads: u8) -> Vec<u8> {
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(cs)
        .set_num_threads(threads);
    JpegDecoder::new_with_options(ZCursor::new(data), options)
        .decode()
        .unwrap()
}

#[test]
fn progressive_threads_match_single_thread() {
    let base = "../../test-images/jpeg/benchmarks/";
    for name in [
        "speed_bench_prog.jpg",
        "speed_bench_prog_h_sampling.jpg",
        "speed_bench_prog_v_sampling.jpg",
        "speed_bench_prog_hv_sampling.jpg",
        "../2029.jpg",
        "../down_sampled_grayscale_prog.jpg"
    ] {
        let data = std::fs::read(format!("{base}{name}")).unwrap();
        for cs in [ColorSpace::RGB, ColorSpace::Luma, ColorSpace::RGBA] {
            let reference = decode(&data, cs, 1);
            for threads in [2, 3, 8] {
                assert!(
                    reference == decode(&data, cs, threads),
                    "{name} {cs:?} differs with {threads} threads"
                );
            }
        }
    }
}

fn decode_result(data: &[u8], cs: ColorSpace, threads: u8) -> Result<Vec<u8>, String> {
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(cs)
        .set_num_threads(threads);
    JpegDecoder::new_with_options(ZCursor::new(data), options)
        .decode()
        .map_err(|e| format!("{e:?}"))
}

const BASELINE: [&str; 5] = [
    "speed_bench.jpg",
    "speed_bench_horizontal_subsampling.jpg",
    "speed_bench_vertical_subsampling.jpg",
    "speed_bench_hv_subsampling.jpg",
    "../2029.jpg"
];

#[test]
fn baseline_threads_match_single_thread() {
    let base = "../../test-images/jpeg/benchmarks/";
    for name in BASELINE {
        let data = std::fs::read(format!("{base}{name}")).unwrap();
        for cs in [
            ColorSpace::RGB,
            ColorSpace::Luma,
            ColorSpace::RGBA,
            ColorSpace::BGR
        ] {
            let reference = decode(&data, cs, 1);
            for threads in [2, 3, 8] {
                assert!(
                    reference == decode(&data, cs, threads),
                    "{name} {cs:?} differs with {threads} threads"
                );
            }
        }
    }
}

/// Truncated and corrupted baseline streams give the same result (output or error) on every
/// thread count.
#[test]
fn baseline_damaged_threads_match_single_thread() {
    let base = "../../test-images/jpeg/benchmarks/";
    for name in &BASELINE[..4] {
        let data = std::fs::read(format!("{base}{name}")).unwrap();
        let mut cases = Vec::new();
        for k in 1..8 {
            cases.push(data[..data.len() * k / 8].to_vec());
        }
        for k in 1..8_usize {
            let mut damaged = data.clone();
            for j in 0..4 {
                let at = (data.len() * k / 8 + j * 7919) % data.len();
                if at > 700 {
                    damaged[at] ^= 0x5a + j as u8;
                }
            }
            cases.push(damaged);
        }
        for (n, case) in cases.iter().enumerate() {
            let reference = decode_result(case, ColorSpace::RGB, 1);
            for threads in [2, 4, 8] {
                assert!(
                    reference == decode_result(case, ColorSpace::RGB, threads),
                    "{name} case {n} differs with {threads} threads"
                );
            }
        }
    }
}

/// Found by fuzzing (minimized): a progressive image the single-threaded finish rejects at its
/// first MCU row ("Invalid component dimensions") must be rejected the same way with threads,
/// not panic in a band that starts mid-image. The size is raised to 1024x1040 so the image is
/// large enough to be split across threads.
#[test]
fn invalid_component_dimensions_rejected_with_threads() {
    #[rustfmt::skip]
    const DATA: [u8; 173] = [
    0xff, 0xd8, 0xff, 0xdb, 0x00, 0x43, 0x00, 0x28, 0x1c, 0x1e, 0x23, 0x1e, 0x19, 0x28, 0x23, 0x21,
    0x23, 0x2d, 0x2b, 0x28, 0x30, 0x3c, 0x64, 0x41, 0x3c, 0x37, 0x37, 0x3c, 0x7b, 0x58, 0x5d, 0x49,
    0x64, 0x91, 0x80, 0x99, 0x96, 0x8f, 0x80, 0x8c, 0x8a, 0xa0, 0xb4, 0xe6, 0xc3, 0xa0, 0xaa, 0xda,
    0xad, 0x8a, 0x8c, 0xc8, 0xff, 0xcb, 0xda, 0xee, 0xf5, 0xff, 0xff, 0xff, 0x9b, 0xc1, 0xff, 0xff,
    0xff, 0xfa, 0xff, 0xe6, 0xfd, 0xff, 0xf8, 0xff, 0xdb, 0x00, 0x43, 0x01, 0x2b, 0x2d, 0x2d, 0x3c,
    0x35, 0x3c, 0x76, 0x41, 0x41, 0x76, 0xf8, 0xa5, 0x8c, 0xa5, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8,
    0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8,
    0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0x0c, 0x03, 0x01, 0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x00,
    0x10, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xff, 0xc2, 0x00, 0x11,
    0x08, 0x04, 0x10, 0x04, 0x00, 0x03, 0x01, 0x22, 0x00, 0x02, 0x14, 0x01, 0x03, 0x12, 0x00, 0xff,
    0xda, 0x00, 0x0c, 0x03, 0x01, 0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x00, 0x10
    ];
    for cs in [ColorSpace::RGB, ColorSpace::RGBA, ColorSpace::Luma, ColorSpace::YCbCr] {
        let decode = |threads: u8| {
            let options = DecoderOptions::default()
                .jpeg_set_out_colorspace(cs)
                .set_num_threads(threads);
            JpegDecoder::new_with_options(ZCursor::new(&DATA[..]), options)
                .decode()
                .map_err(|e| format!("{e:?}"))
        };
        let reference = decode(1);
        assert!(reference.is_err(), "{cs:?}");
        for threads in [2, 4, 8] {
            assert_eq!(reference, decode(threads), "{cs:?} {threads} threads");
        }
    }
}
