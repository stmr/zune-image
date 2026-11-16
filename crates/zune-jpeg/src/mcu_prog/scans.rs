/*
 * Copyright (c) 2023.
 *
 * This software is free software;
 *
 * You can redistribute it or modify it under terms of the MIT, Apache License or Zlib license
 */

//! Opt-in concurrent entropy decoding of independent progressive scans.
//!
//! Only used when `DecoderOptions::jpeg_set_threads(n)` has `n > 1`.
//!
//! The scan sequence is first walked without decoding: each scan's entropy data is skipped by
//! looking for its terminating marker, and the scan header, DHT and DRI segments between
//! scans are parsed on the decoder as usual. Scans are then split into chains that touch
//! disjoint coefficients and decoded concurrently, each chain in file order:
//!
//! - one chain for every DC scan (they only touch coefficient 0 of every block),
//! - one chain per component for its single-component AC scans (coefficients 1..64).
//!
//! The result is only used if every scan decodes cleanly and ends exactly where the walk
//! predicted. Otherwise the decoder state is restored and the usual sequential path decodes
//! the image, so malformed input behaves exactly as it does with one thread.

use alloc::vec;
use alloc::vec::Vec;
use std::sync::Mutex;

use zune_core::bytestream::{ZByteReaderTrait, ZCursor};

use super::get_marker;
use crate::bitstream::BitStream;
use crate::components::Components;
use crate::decoder::{EntropyTables, ImageInfo, JpegDecoder, MAX_COMPONENTS};
use crate::headers::parse_sos;
use crate::marker::Marker;

/// One scan: where its entropy data lives and the decoder state it is decoded with.
struct Job {
    start:            usize,
    /// Offset just after the marker that terminates the scan.
    end:              usize,
    marker:           Marker,
    spec_start:       u8,
    spec_end:         u8,
    succ_high:        u8,
    succ_low:         u8,
    num_scans:        u8,
    z_order:          [usize; MAX_COMPONENTS],
    /// (DC, AC) huffman table ids of every component.
    huff:             Vec<(usize, usize)>,
    restart_interval: usize,
    tables:           EntropyTables
}

/// Decoder state a worker needs to decode scans, shared by every worker.
struct Proto {
    info:             ImageInfo,
    components:       Vec<Components>,
    options:          zune_core::options::DecoderOptions,
    h_max:            usize,
    v_max:            usize,
    mcu_x:            usize,
    mcu_y:            usize,
    is_interleaved:   bool,
    input_colorspace: zune_core::colorspace::ColorSpace
}

/// A chain of scans decoded in order into its own buffers.
struct Task {
    /// `None` for the DC chain, otherwise the component whose AC scans this decodes.
    component: Option<usize>,
    jobs:      Vec<usize>,
    cost:      usize,
    buffers:   [Vec<i16>; MAX_COMPONENTS]
}

struct Done {
    component: Option<usize>,
    buffers:   [Vec<i16>; MAX_COMPONENTS],
    dc_seen:   [bool; MAX_COMPONENTS],
    /// Last good row of every job in `Task::jobs`, `None` if the chain failed.
    rows:      Option<Vec<usize>>,
    jobs:      Vec<usize>
}

/// Find the marker ending the scan data that starts at `from`; returns the offset just
/// after it. Escaped `FF00` and restart markers belong to the scan, unless `skip_rst` is unset.
fn find_marker(data: &[u8], from: usize, skip_rst: bool) -> Option<(usize, Marker)> {
    let mut i = from;
    while i < data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while data.get(j) == Some(&0xFF) {
            j += 1;
        }
        let r = *data.get(j)?;
        if r == 0 || (skip_rst && (0xD0..=0xD7).contains(&r)) {
            i = j + 1;
            continue;
        }
        return Some((j + 1, Marker::from_u8(r)?));
    }
    None
}

struct Saved {
    base:             usize,
    spec_start:       u8,
    spec_end:         u8,
    succ_high:        u8,
    succ_low:         u8,
    num_scans:        u8,
    scan_subsampled:  bool,
    z_order:          [usize; MAX_COMPONENTS],
    huff:             Vec<(usize, usize)>,
    restart_interval: usize,
    todo:             usize,
    tables:           EntropyTables
}

impl<T: ZByteReaderTrait> JpegDecoder<T> {
    fn huff_ids(&self) -> Vec<(usize, usize)> {
        self.components
            .iter()
            .map(|c| (c.dc_huff_table, c.ac_huff_table))
            .collect()
    }

    fn restore_scan_state(&mut self, s: Saved) {
        self.spec_start = s.spec_start;
        self.spec_end = s.spec_end;
        self.succ_high = s.succ_high;
        self.succ_low = s.succ_low;
        self.num_scans = s.num_scans;
        self.scan_subsampled = s.scan_subsampled;
        self.z_order = s.z_order;
        for (c, (dc, ac)) in self.components.iter_mut().zip(s.huff) {
            c.dc_huff_table = dc;
            c.ac_huff_table = ac;
        }
        self.restart_interval = s.restart_interval;
        self.todo = s.todo;
        self.entropy_tables = s.tables;
        let _ = self.stream.set_position(s.base);
    }

    /// Record the scan whose header was just parsed, its data starting at `start`.
    fn plan_job(&self, data: &[u8], start: usize) -> Option<Job> {
        let (end, marker) = find_marker(data, start, true)?;
        Some(Job {
            start,
            end,
            marker,
            spec_start: self.spec_start,
            spec_end: self.spec_end,
            succ_high: self.succ_high,
            succ_low: self.succ_low,
            num_scans: self.num_scans,
            z_order: self.z_order,
            huff: self.huff_ids(),
            restart_interval: self.restart_interval,
            tables: self.entropy_tables.clone()
        })
    }

    /// Walk the scans from the one whose header is parsed, without decoding any of them.
    fn plan_scans(&mut self, base: usize, data: &[u8]) -> Option<Vec<Job>> {
        let max_scans = self.options.jpeg_get_max_scans();
        let mut jobs: Vec<Job> = Vec::new();
        let mut start = 0;
        loop {
            // Only single-component AC scans and DC scans are independent chains.
            if (self.spec_start != 0 && self.num_scans != 1)
                || (self.num_scans == 1 && self.z_order[0] >= self.components.len())
            {
                return None;
            }
            let job = self.plan_job(data, start)?;
            let mut marker = job.marker;
            self.stream.set_position(base + job.end).ok()?;
            jobs.push(job);
            if jobs.len() > max_scans {
                return None;
            }
            loop {
                match marker {
                    Marker::EOI => return Some(jobs),
                    Marker::SOS => {
                        parse_sos(self).ok()?;
                        start = usize::try_from(self.stream.position().ok()?).ok()? - base;
                        break;
                    }
                    Marker::DHT | Marker::DRI => {
                        self.parse_marker_inner(marker).ok()?;
                        let at = usize::try_from(self.stream.position().ok()?).ok()? - base;
                        let (end, m) = find_marker(data, at, false)?;
                        self.stream.set_position(base + end).ok()?;
                        marker = m;
                    }
                    // Anything else between scans (metadata, DQT, DNL, restarts ...) has
                    // effects on the decoder that are not worth replicating here.
                    _ => return None
                }
            }
        }
    }

    /// Decode all scans from the current one to EOI concurrently.
    ///
    /// Returns `true` when `block` holds the coefficients of every scan and the decoder is in
    /// the state the sequential loop would leave it in after the last scan. Returns `false`
    /// with the decoder untouched (apart from zeroed `block`) when the image is not eligible
    /// or any scan did not decode cleanly: the caller then decodes sequentially.
    #[allow(clippy::too_many_lines)]
    // Kept out of line: when inlined it made the sequential caller ~4x larger, and
    // threads == 1 progressive decodes measured ~3% slower on x86_64 Windows.
    #[inline(never)]
    pub(super) fn decode_progressive_scans_parallel<B: BitStream>(
        &mut self, block: &mut [Vec<i16>; MAX_COMPONENTS], require_real_dc: bool
    ) -> bool {
        let Ok(base) = self.stream.position() else {
            return false;
        };
        let Ok(base) = usize::try_from(base) else {
            return false;
        };
        let mut data = Vec::new();
        let read = self.stream.read_all(&mut data);
        if read.is_err() || self.stream.set_position(base).is_err() {
            return false;
        }

        let saved = Saved {
            base,
            spec_start: self.spec_start,
            spec_end: self.spec_end,
            succ_high: self.succ_high,
            succ_low: self.succ_low,
            num_scans: self.num_scans,
            scan_subsampled: self.scan_subsampled,
            z_order: self.z_order,
            huff: self.huff_ids(),
            restart_interval: self.restart_interval,
            todo: self.todo,
            tables: self.entropy_tables.clone()
        };

        let Some(jobs) = self.plan_scans(base, &data) else {
            self.restore_scan_state(saved);
            return false;
        };

        let proto = Proto {
            info:             self.info.clone(),
            components:       self.components.clone(),
            options:          self.options,
            h_max:            self.h_max,
            v_max:            self.v_max,
            mcu_x:            self.mcu_x,
            mcu_y:            self.mcu_y,
            is_interleaved:   self.is_interleaved,
            input_colorspace: self.input_colorspace
        };

        // Build the chains. DC scans write coefficient 0 of every component into scratch
        // buffers, AC chains write the real buffers of their component.
        let ncomp = self.components.len();
        let mut tasks: Vec<Task> = Vec::new();
        for (idx, job) in jobs.iter().enumerate() {
            let component = if job.spec_start == 0 { None } else { Some(job.z_order[0]) };
            let cost = job.end - job.start;
            if let Some(t) = tasks.iter_mut().find(|t| t.component == component) {
                t.jobs.push(idx);
                t.cost += cost;
            } else {
                tasks.push(Task {
                    component,
                    jobs: vec![idx],
                    cost,
                    buffers: core::array::from_fn(|_| Vec::new())
                });
            }
        }
        for t in &mut tasks {
            match t.component {
                Some(c) => t.buffers[c] = core::mem::take(&mut block[c]),
                None => {
                    for (buffer, b) in t.buffers.iter_mut().zip(block.iter()).take(ncomp) {
                        *buffer = vec![0; b.len()];
                    }
                }
            }
        }
        // Heaviest chains are popped first.
        tasks.sort_by_key(|t| t.cost);

        let workers = usize::from(self.options.jpeg_get_threads()).min(tasks.len());
        let queue = Mutex::new(tasks);
        let done: Mutex<Vec<Done>> = Mutex::new(Vec::new());
        let work = || {
            while let Some(task) = queue.lock().unwrap().pop() {
                let result = run_task::<B>(&data, &proto, &jobs, task, require_real_dc);
                done.lock().unwrap().push(result);
            }
        };
        std::thread::scope(|s| {
            for _ in 1..workers {
                s.spawn(work);
            }
            work();
        });

        let mut done = done.into_inner().unwrap();
        let ok = done.iter().all(|d| d.rows.is_some());
        let mut rows = vec![0; jobs.len()];
        let mut dc_seen = [false; MAX_COMPONENTS];
        let mut dc_buffers: Option<[Vec<i16>; MAX_COMPONENTS]> = None;
        for d in &mut done {
            if let Some(c) = d.component {
                block[c] = core::mem::take(&mut d.buffers[c]);
            } else {
                dc_seen = d.dc_seen;
                dc_buffers = Some(core::mem::take(&mut d.buffers));
            }
            if let Some(r) = &d.rows {
                for (&job, &row) in d.jobs.iter().zip(r) {
                    rows[job] = row;
                }
            }
        }

        if !ok {
            for b in block.iter_mut() {
                b.fill(0);
            }
            self.restore_scan_state(saved);
            return false;
        }

        if let Some(dc) = dc_buffers {
            for (target, source) in block.iter_mut().zip(&dc) {
                for (t, s) in target.iter_mut().step_by(64).zip(source.iter().step_by(64)) {
                    *t = *s;
                }
            }
        }
        for (c, seen) in dc_seen.iter().enumerate().take(ncomp) {
            self.components[c].progressive_dc_seen = *seen;
        }
        for (job, &row) in jobs.iter().zip(&rows) {
            self.spec_start = job.spec_start;
            self.spec_end = job.spec_end;
            self.succ_low = job.succ_low;
            self.num_scans = job.num_scans;
            self.z_order = job.z_order;
            self.progressive_scan_last_good_row = row;
            self.progressive_completed_scans += 1;
            self.accept_progressive_precision();
        }
        true
    }
}

/// Decode a chain of scans; any scan that is not clean fails the chain.
fn run_task<B: BitStream>(
    data: &[u8], proto: &Proto, jobs: &[Job], mut task: Task, require_real_dc: bool
) -> Done {
    let mut d = JpegDecoder::new_with_options(ZCursor::new(data), proto.options);
    d.info.clone_from(&proto.info);
    d.components.clone_from(&proto.components);
    d.h_max = proto.h_max;
    d.v_max = proto.v_max;
    d.mcu_x = proto.mcu_x;
    d.mcu_y = proto.mcu_y;
    d.is_interleaved = proto.is_interleaved;
    d.input_colorspace = proto.input_colorspace;
    d.is_progressive = true;

    let mut rows = Vec::with_capacity(task.jobs.len());
    let mut failed = false;
    for &idx in &task.jobs {
        let job = &jobs[idx];
        d.spec_start = job.spec_start;
        d.spec_end = job.spec_end;
        d.succ_high = job.succ_high;
        d.succ_low = job.succ_low;
        d.num_scans = job.num_scans;
        d.z_order = job.z_order;
        for (c, &(dc, ac)) in d.components.iter_mut().zip(&job.huff) {
            c.dc_huff_table = dc;
            c.ac_huff_table = ac;
        }
        d.restart_interval = job.restart_interval;
        d.entropy_tables = job.tables.clone();
        d.progressive_scan_last_good_row = 0;
        let mut stream = B::new_progressive(job.succ_low, job.spec_start, job.spec_end);
        stream.set_strict(d.options.strict_mode());
        let clean = d.stream.set_position(job.start).is_ok()
            && d.parse_entropy_coded_data(&mut stream, &mut task.buffers, None, require_real_dc)
                .is_ok()
            && stream.overread_by() == 0
            && matches!(get_marker(&mut d.stream, &mut stream), Ok(m) if m == job.marker)
            && d.stream.position().is_ok_and(|p| p == job.end as u64);
        if !clean {
            failed = true;
            break;
        }
        rows.push(d.progressive_scan_last_good_row);
    }
    let dc_seen =
        core::array::from_fn(|c| d.components.get(c).is_some_and(|c| c.progressive_dc_seen));
    Done {
        component: task.component,
        buffers: task.buffers,
        dc_seen,
        rows: if failed { None } else { Some(rows) },
        jobs: task.jobs
    }
}
