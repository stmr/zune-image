//! Pipelined baseline decoding (`DecoderOptions::num_threads() > 1`, `std` only).
//!
//! The calling thread keeps doing entropy decoding and the IDCT (the sequential loop in
//! `mcu.rs`), but instead of up-sampling and colour converting every MCU row itself it
//! snapshots the row's samples and hands bands of rows to `n - 1` scoped worker threads.
//! Each worker owns a copy of the components, so the up-sampler's carried state (the last
//! rows of the previous MCU row) is private to it; with more than one worker a band starts
//! with the previous row as a "priming" row whose output is discarded.
//!
//! Workers write disjoint bands of the output buffer. The first error (in submission order)
//! is reported in place of whatever the calling thread returns, matching the sequential order
//! in which a post-processing error of an earlier row precedes anything that happens later.

#[cfg(feature = "std")]
pub(crate) use real::{PipeScope, Pipeline};
#[cfg(not(feature = "std"))]
pub(crate) use stub::{PipeScope, Pipeline};

use crate::components::Components;
use crate::errors::DecodeErrors;

#[cfg(feature = "std")]
mod real {
    use alloc::sync::Arc;
    use alloc::vec;
    use alloc::vec::Vec;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender};
    use std::thread::{Scope, ScopedJoinHandle};

    use zune_core::colorspace::ColorSpace;

    use super::{Components, DecodeErrors};
    use crate::components::SampleRatios;
    use crate::mcu::{post_process_rows, PostCtx};

    pub(crate) type PipeScope<'s, 'e> = Scope<'s, 'e>;

    /// Everything post-processing needs besides the samples.
    #[derive(Clone, Copy)]
    struct Geometry {
        ctx:          PostCtx,
        vertical:     bool,
        stride:       usize,
        width:        usize,
        padded_width: usize
    }

    impl Geometry {
        /// Output lines written when MCU row `i` is post-processed, see `post_process_rows`.
        fn lines(&self, i: usize, mcu_height: usize) -> usize {
            let ctx = &self.ctx;
            if ctx.is_interleaved && ctx.out_colorspace != ColorSpace::Luma {
                let cv = ctx.coeff * ctx.v_max;
                if self.vertical {
                    usize::from(i > 0) * cv
                        + (8 - usize::from(i != mcu_height.saturating_sub(1))) * cv
                } else {
                    8 * cv
                }
            } else if let SampleRatios::Generic(_, v) = ctx.sample_ratio {
                8 * v * ctx.coeff
            } else {
                8 * ctx.coeff
            }
        }
    }

    /// Snapshots of consecutive MCU rows: `data[row][component]` is the component's `raw_coeff`.
    #[derive(Default)]
    struct Rows {
        data:  Vec<Vec<Vec<i16>>>,
        /// (MCU row, MCU height passed to post-processing, submission sequence number)
        ids:   Vec<(usize, usize, usize)>,
        n:     usize,
        /// `data[0]` is the previous band's last row, only run to recover the carried state.
        prime: bool
    }

    impl Rows {
        fn push(&mut self, comps: &[Components], id: (usize, usize, usize)) {
            if self.data.len() <= self.n {
                self.data.push(vec![Vec::new(); comps.len()]);
                self.ids.push(id);
            }
            for (snap, comp) in self.data[self.n].iter_mut().zip(comps) {
                snap.clear();
                snap.extend_from_slice(&comp.raw_coeff);
            }
            self.ids[self.n] = id;
            self.n += 1;
        }
    }

    struct Job<'s> {
        rows: Rows,
        out:  &'s mut [u8]
    }

    type WorkerError = Option<(usize, DecodeErrors)>;

    fn work(
        jobs: Receiver<Job<'_>>, free: &Sender<Rows>, mut comps: Vec<Components>, g: Geometry,
        scratch_len: usize, failed: &AtomicBool
    ) -> WorkerError {
        let mut scratch = vec![0_i16; scratch_len];
        let mut sink: Vec<u8> = Vec::new();
        let mut error: WorkerError = None;
        for Job { mut rows, out } in jobs {
            // after a failure keep draining so the producer never blocks on a full queue
            if error.is_none() {
                let mut written = 0;
                for r in 0..rows.n {
                    let (i, mcu_height, seq) = rows.ids[r];
                    for (comp, snap) in comps.iter_mut().zip(rows.data[r].iter_mut()) {
                        core::mem::swap(&mut comp.raw_coeff, snap);
                    }
                    let result = if rows.prime && r == 0 {
                        sink.clear();
                        sink.resize(g.lines(i, mcu_height) * g.stride, 0);
                        post_process_rows(
                            &g.ctx,
                            &mut comps,
                            &mut sink,
                            g.stride,
                            i,
                            mcu_height,
                            g.width,
                            g.padded_width,
                            &mut 0,
                            &mut scratch
                        )
                    } else {
                        post_process_rows(
                            &g.ctx,
                            &mut comps,
                            out,
                            g.stride,
                            i,
                            mcu_height,
                            g.width,
                            g.padded_width,
                            &mut written,
                            &mut scratch
                        )
                    };
                    if let Err(e) = result {
                        error = Some((seq, e));
                        failed.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            }
            let _ = free.send(rows);
        }
        error
    }

    pub(crate) struct Pipeline<'s, 'x> {
        g:         Geometry,
        failed:    Arc<AtomicBool>,
        txs:       Vec<SyncSender<Job<'s>>>,
        handles:   Vec<ScopedJoinHandle<'s, WorkerError>>,
        next:      usize,
        band_rows: usize,
        free:      Receiver<Rows>,
        cur:       Rows,
        seq:       usize,
        /// output not yet handed to a worker
        rest:      &'s mut [u8],
        /// bytes of `rest` claimed by rows of `cur`
        out_len:   usize,
        slot:      &'x mut Option<DecodeErrors>
    }

    impl<'s, 'x> Pipeline<'s, 'x> {
        #[allow(clippy::too_many_arguments)]
        pub(crate) fn new<'e>(
            scope: &'s Scope<'s, 'e>, threads: usize, comps: &[Components], ctx: PostCtx,
            stride: usize, width: usize, padded_width: usize, scratch_len: usize,
            mcu_height: usize, slot: &'x mut Option<DecodeErrors>
        ) -> Option<Self> {
            let vertical = comps
                .iter()
                .any(|c| c.sample_ratio == SampleRatios::HV || c.sample_ratio == SampleRatios::V);
            let g = Geometry {
                ctx,
                vertical,
                stride,
                width,
                padded_width
            };
            let workers = (threads - 1).min(mcu_height).max(1);
            // single worker: strictly sequential, one row per job; several: bands, so the
            // priming row is amortised
            let band_rows = if workers == 1 { 1 } else { (mcu_height / (workers * 3)).clamp(2, 8) };
            let failed = Arc::new(AtomicBool::new(false));
            let (free_tx, free) = channel();
            let mut txs = Vec::with_capacity(workers);
            let mut handles = Vec::with_capacity(workers);
            for _ in 0..workers {
                let (tx, rx) = sync_channel(2);
                let worker_comps = comps.to_vec();
                let free_tx = free_tx.clone();
                let failed = failed.clone();
                let handle = std::thread::Builder::new()
                    .spawn_scoped(scope, move || {
                        work(rx, &free_tx, worker_comps, g, scratch_len, &failed)
                    })
                    .ok()?;
                txs.push(tx);
                handles.push(handle);
            }
            Some(Self {
                g,
                failed,
                txs,
                handles,
                next: 0,
                band_rows,
                free,
                cur: Rows::default(),
                seq: 0,
                rest: &mut [],
                out_len: 0,
                slot
            })
        }

        /// Hand over the output buffer, must be called before the first `submit`.
        pub(crate) fn attach(&mut self, pixels: &'s mut [u8]) {
            self.rest = pixels;
        }

        fn primes(&self) -> bool {
            self.txs.len() > 1
        }

        /// Queue post-processing of the MCU row currently in `comps`, returns the number of
        /// output bytes it produces.
        pub(crate) fn submit(
            &mut self, comps: &[Components], i: usize, mcu_height: usize
        ) -> Result<usize, DecodeErrors> {
            self.cur.push(comps, (i, mcu_height, self.seq));
            self.seq += 1;
            let avail = self.rest.len() - self.out_len;
            let len = (self.g.lines(i, mcu_height) * self.g.stride)
                .min(avail / self.g.stride * self.g.stride);
            self.out_len += len;
            if self.cur.n == self.band_rows + usize::from(self.cur.prime) {
                self.send();
            }
            if self.failed.load(Ordering::Relaxed) {
                // replaced by the worker's own error in `Drop`
                return Err(DecodeErrors::FormatStatic("jpeg post-processing failed"));
            }
            Ok(len)
        }

        fn send(&mut self) {
            let mut next = self.free.try_recv().unwrap_or_default();
            next.n = 0;
            next.prime = false;
            let n = self.cur.n;
            if self.primes() {
                if next.data.is_empty() {
                    next.data.push(Vec::new());
                    next.ids.push((0, 0, 0));
                }
                next.data[0].clone_from(&self.cur.data[n - 1]);
                next.ids[0] = self.cur.ids[n - 1];
                next.n = 1;
                next.prime = true;
            }
            let rows = core::mem::replace(&mut self.cur, next);
            let (out, tail) = core::mem::take(&mut self.rest).split_at_mut(self.out_len);
            self.rest = tail;
            self.out_len = 0;
            let worker = self.next;
            self.next = (self.next + 1) % self.txs.len();
            if self.txs[worker].send(Job { rows, out }).is_err() {
                self.failed.store(true, Ordering::Relaxed);
            }
        }

        fn flush(&mut self) {
            if self.cur.n > usize::from(self.cur.prime) {
                self.send();
            }
        }

        /// Fill every output byte not produced by a submitted row with 128.
        pub(crate) fn fill_neutral(&mut self) {
            self.flush();
            self.rest.fill(128);
        }
    }

    impl Drop for Pipeline<'_, '_> {
        fn drop(&mut self) {
            self.flush();
            self.txs.clear();
            let mut first: WorkerError = None;
            for handle in self.handles.drain(..) {
                match handle.join() {
                    Ok(Some((seq, e))) => {
                        if first.as_ref().map_or(true, |(s, _)| seq < *s) {
                            first = Some((seq, e));
                        }
                    }
                    Ok(None) => {}
                    Err(payload) => {
                        if !std::thread::panicking() {
                            std::panic::resume_unwind(payload);
                        }
                    }
                }
            }
            if let Some((_, e)) = first {
                *self.slot = Some(e);
            }
        }
    }
}

#[cfg(not(feature = "std"))]
mod stub {
    use core::convert::Infallible;
    use core::marker::PhantomData;

    use super::{Components, DecodeErrors};
    use crate::mcu::PostCtx;

    pub(crate) struct PipeScope<'s, 'e>(PhantomData<(&'s (), &'e ())>);

    /// Uninhabited: no thread support without `std`.
    pub(crate) struct Pipeline<'s, 'x>(Infallible, PhantomData<(&'s (), &'x ())>);

    impl<'s, 'x> Pipeline<'s, 'x> {
        #[allow(clippy::too_many_arguments)]
        pub(crate) fn new<'e>(
            _: &'s PipeScope<'s, 'e>, _: usize, _: &[Components], _: PostCtx, _: usize, _: usize,
            _: usize, _: usize, _: usize, _: &'x mut Option<DecodeErrors>
        ) -> Option<Self> {
            None
        }

        pub(crate) fn attach(&mut self, _: &'s mut [u8]) {
            match self.0 {}
        }

        pub(crate) fn submit(
            &mut self, _: &[Components], _: usize, _: usize
        ) -> Result<usize, DecodeErrors> {
            match self.0 {}
        }

        pub(crate) fn fill_neutral(&mut self) {
            match self.0 {}
        }
    }
}
