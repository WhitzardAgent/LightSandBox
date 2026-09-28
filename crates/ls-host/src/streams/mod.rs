//! T12 — bounded output spool, per-stream cursors, backpressure, retention
//! gaps (plans/tasks/T12.md; docs/v5/06_HTTP.md §输出与重放;
//! docs/v5/08_FAILURES.md §日志与背压).
//!
//! Laws honored here (card review focus + failure spec):
//!
//! * BOUNDED ALWAYS: per-stream memory cap spills to a per-stream disk
//!   file; a GLOBAL memory budget is shared by every spool on the node
//!   (slow consumers cannot inflate node RSS). FD usage is accounted the
//!   same way. Nothing here can grow unboundedly.
//! * NO SILENT LOSS: every byte range that leaves the retention window —
//!   by disk-cap rotation (Retention) or by terminate-policy truncation
//!   (Overflow) — is recorded in the spool's loss log. A read for a lost
//!   range returns a StreamGap with the available interval and reason;
//!   output is never truncated-and-claimed-complete (08_FAILURES §5).
//! * WRITERS NEVER BLOCK ON READERS: consumption is pull-based
//!   (`read(cursor, max)`). A dead subscriber costs nothing; a guest
//!   writer only ever contends with the spool's own bounded io.
//! * CURSOR = LOGICAL BYTE OFFSET of the producer's stream (gaps count;
//!   offsets never rewind). The upstream LINE cursor
//!   (`EXECD-COMMANDS-TAIL-CURSOR`) is a different unit, converted
//!   explicitly at the wire layer (ls-api) from the newline index here
//!   (card: "明确转换"; review focus: tail cursor ≠ byte cursor).
//! * DIGEST INDEX, NOT PER-EVENT HASHES: retained bytes are summarized as
//!   ~64KiB chunk digests (08_FAILURES §5); the index is bounded by the
//!   retention window. The newline index is likewise capped (MAX_NEWLINES)
//!   with overflow evicted "before the floor" — older line-cursor lookups
//!   degrade to an honest stale answer, never a guess.

use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::images::sha256::Sha256;
use std::sync::{Arc, Mutex};



// ------------------------------------------------------------------ bounds

/// Per-stream in-memory retention target before spilling to disk.
pub const DEFAULT_MEM_CAP: usize = 256 * 1024;
/// Per-stream disk spool cap; rotation past it is a recorded Retention loss.
pub const DEFAULT_DISK_CAP: u64 = 64 * 1024 * 1024;
/// Node-wide in-memory output budget shared by ALL spools.
pub const DEFAULT_GLOBAL_MEM_CAP: usize = 8 * 1024 * 1024;
/// Node-wide open spool-file FD budget.
pub const DEFAULT_FD_CAP: usize = 256;
/// Chunk digest segment target (08_FAILURES: segmented summaries).
pub const CHUNK_TARGET: usize = 64 * 1024;
/// Bounded newline index: the last N line-start offsets stay resolvable;
/// older line-cursor lookups answer Stale (same honesty class as gaps).
pub const MAX_NEWLINES: usize = 4096;

// ------------------------------------------------------------------- ids

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamId {
    Stdout,
    Stderr,
    /// PTY merges both into one stream (06_HTTP); declared, not yet fed.
    Pty,
}

impl StreamId {
    pub fn as_str(self) -> &'static str {
        match self {
            StreamId::Stdout => "stdout",
            StreamId::Stderr => "stderr",
            StreamId::Pty => "pty",
        }
    }

    pub fn parse(s: &str) -> Option<StreamId> {
        match s {
            "stdout" => Some(StreamId::Stdout),
            "stderr" => Some(StreamId::Stderr),
            "pty" => Some(StreamId::Pty),
            _ => None,
        }
    }
}

// ------------------------------------------------------------------ config

/// What the spool does when the DISK cap would be exceeded by a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskOverflow {
    /// Rotate the oldest disk prefix out (recorded as a Retention loss);
    /// the producer keeps running. Default.
    Rotate,
    /// Truncate: stop accepting (recorded as an Overflow loss from the
    /// truncation point); the caller is expected to terminate the producer.
    TerminateRequested,
}

#[derive(Debug, Clone)]
pub struct SpoolConfig {
    pub mem_cap: usize,
    pub disk_cap: u64,
    pub overflow: DiskOverflow,
}

impl Default for SpoolConfig {
    fn default() -> Self {
        SpoolConfig {
            mem_cap: DEFAULT_MEM_CAP,
            disk_cap: DEFAULT_DISK_CAP,
            overflow: DiskOverflow::Rotate,
        }
    }
}

// ------------------------------------------------------------------ losses

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LossKind {
    /// Rotated out under the disk cap or explicit GC (retention policy).
    Retention,
    /// Dropped because the terminate policy truncated the stream.
    Overflow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LossRecord {
    /// Logical stream interval [start, end) that is gone, in producer
    /// offsets. Losses are recorded coalesced, never overlapping.
    pub start: u64,
    pub end: u64,
    pub kind: LossKind,
}

// ------------------------------------------------------------------ digests

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkEntry {
    pub index: u64,
    pub first_offset: u64,
    pub len: u32,
    pub sha256: String,
}

// ------------------------------------------------------------------- reads

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataSlice {
    pub offset: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadBatch {
    /// Ordered slices covering [first.offset, next_cursor); `complete` is
    /// true iff the writer is closed and next_cursor == write_pos.
    Data {
        slices: Vec<DataSlice>,
        next_cursor: u64,
        complete: bool,
    },
    /// Requested range left the retention window: nothing served, but the
    /// honest available interval + loss kind is returned; the consumer
    /// resumes from `available_from` (never a re-execution).
    Gap {
        requested: u64,
        available_from: u64,
        available_to: u64,
        kind: LossKind,
    },
}

// ------------------------------------------------------------------ writes

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteReport {
    /// Logical offset at which this write's first byte was offered.
    pub first_offset: u64,
    /// Bytes accepted into retention (== input len unless truncated).
    pub accepted: usize,
    /// Bytes of this call that landed on the disk file.
    pub spilled: usize,
    /// True when the terminate policy fired on this write.
    pub terminate_requested: bool,
}

// ------------------------------------------------------------------- spool

/// Append-only, gap-aware output journal for ONE (execution, stream).
pub struct StreamSpool {
    stream: StreamId,
    cfg: SpoolConfig,
    /// total producer-offered bytes (gaps included) == next read cursor.
    write_pos: u64,
    closed: bool,
    terminated: bool,
    /// in-memory tail: bytes [mem_floor, write_pos), dense.
    mem: VecDeque<u8>,
    mem_floor: u64,
    /// optional disk segment covering [disk_floor, mem_floor), dense.
    disk: Option<DiskSeg>,
    losses: Vec<LossRecord>,
    /// offsets just AFTER each '\n', within the resolvable window.
    newlines: VecDeque<u64>,
    lines_before_floor: u64,
    chunks: VecDeque<ChunkEntry>,
    pending_chunk: Vec<u8>,
    pending_chunk_first: u64,
    /// registry-owned shared accounting
    global_mem: Arc<AtomicUsize>,
    global_mem_cap: usize,
    local_mem: usize,
    fd_counter: Arc<AtomicUsize>,
    fd_cap: usize,
    /// set by the registry at open(): directory + exec label for spill files
    spool_dir: PathBuf,
    exec: String,
}

/// Disk segment physical model: the file holds a dead (rotated-out) prefix
/// of `dead` bytes at its head until an amortized compaction rewrites the
/// live tail at offset 0. Logical byte k sits at file offset
/// `dead + (k - floor)`.
struct DiskSeg {
    file: File,
    path: PathBuf,
    floor: u64,
    len: u64,
    dead: u64,
}

impl StreamSpool {
    pub fn stream(&self) -> StreamId {
        self.stream
    }
    /// Next cursor a consumer would read (== producer-offered bytes).
    pub fn write_pos(&self) -> u64 {
        self.write_pos
    }
    pub fn is_closed(&self) -> bool {
        self.closed
    }
    pub fn is_terminated(&self) -> bool {
        self.terminated
    }
    pub fn losses(&self) -> &[LossRecord] {
        &self.losses
    }
    pub fn chunk_index(&self) -> &VecDeque<ChunkEntry> {
        &self.chunks
    }
    /// Newline offsets inside the resolvable window (position AFTER '\n').
    pub fn newline_offsets(&self) -> &VecDeque<u64> {
        &self.newlines
    }
    pub fn lines_before_floor(&self) -> u64 {
        self.lines_before_floor
    }
    pub fn local_mem_bytes(&self) -> usize {
        self.local_mem
    }
    /// Current on-disk spool length in logical bytes (0 before first spill).
    pub fn disk_len_bytes(&self) -> u64 {
        self.disk_len()
    }

    /// Oldest byte offset still served from retention.
    pub fn retention_floor(&self) -> u64 {
        match &self.disk {
            Some(d) => d.floor,
            None => self.mem_floor,
        }
    }

    fn global_add(&mut self, n: usize) {
        self.global_mem.fetch_add(n, Ordering::SeqCst);
        self.local_mem += n;
    }

    fn global_sub(&mut self, n: usize) {
        self.global_mem.fetch_sub(n, Ordering::SeqCst);
        self.local_mem = self.local_mem.saturating_sub(n);
    }

    /// Append producer bytes. Writers never wait on readers; the only work
    /// here is bounded io to the spill file. Writing after close() is an
    /// internal protocol error (final offsets are frozen), not a silent
    /// drop.
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<WriteReport> {
        if self.closed {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "spool write after close",
            ));
        }
        let first = self.write_pos;
        // The line index tracks PRODUCER truth, even across gaps.
        self.index_newlines(bytes);
        self.write_pos += bytes.len() as u64;

        if self.terminated {
            self.record_loss(first, self.write_pos, LossKind::Overflow);
            return Ok(WriteReport {
                first_offset: first,
                accepted: 0,
                spilled: 0,
                terminate_requested: true,
            });
        }

        // Keep the GLOBAL memory budget honest before admitting more:
        // spill our own cold prefix (writers never touch other spools).
        self.spill_to_fit(bytes.len())?;

        // Terminate policy: when even a full spill of current memory cannot
        // keep this write under the disk cap, truncate honestly.
        if self.cfg.overflow == DiskOverflow::TerminateRequested {
            let pending_disk =
                (self.mem.len() + bytes.len()).saturating_sub(self.cfg.mem_cap) as u64;
            if self.disk_len() + pending_disk > self.cfg.disk_cap {
                self.terminated = true;
                self.record_loss(first, self.write_pos, LossKind::Overflow);
                return Ok(WriteReport {
                    first_offset: first,
                    accepted: 0,
                    spilled: 0,
                    terminate_requested: true,
                });
            }
        }

        // Accept into memory, charge budgets, update chunk digests.
        let n = bytes.len();
        self.global_add(n);
        self.mem.extend(bytes.iter().copied());
        self.feed_chunks(first, bytes);

        // Local memory cap → spill the cold prefix.
        let mut spilled = 0usize;
        if self.mem.len() > self.cfg.mem_cap {
            spilled += self.spill_n(self.mem.len() - self.cfg.mem_cap)?;
        }
        // Disk cap (Rotate policy) → rotate the oldest prefix out.
        while self.disk_len() > self.cfg.disk_cap {
            self.rotate_disk_oldest()?;
        }
        Ok(WriteReport {
            first_offset: first,
            accepted: n,
            spilled,
            terminate_requested: false,
        })
    }

    /// Ordered read. Stale cursors are a semantic answer (Gap), io failures
    /// are the only Err.
    pub fn read(&mut self, cursor: u64, max_bytes: usize) -> io::Result<ReadBatch> {
        let floor = self.retention_floor();
        if cursor < floor {
            let kind = self
                .losses
                .iter()
                .rev()
                .find(|l| l.end > cursor)
                .map(|l| l.kind)
                .unwrap_or(LossKind::Retention);
            return Ok(ReadBatch::Gap {
                requested: cursor,
                available_from: floor,
                available_to: self.write_pos,
                kind,
            });
        }
        let mut slices = Vec::new();
        let mut pos = cursor;
        let mut budget = max_bytes;
        if let Some(d) = &mut self.disk {
            if pos < self.mem_floor && budget > 0 {
                let take = ((self.mem_floor - pos) as usize).min(budget);
                let mut buf = vec![0u8; take];
                d.file.seek(SeekFrom::Start(d.dead + (pos - d.floor)))?;
                d.file.read_exact(&mut buf)?;
                slices.push(DataSlice { offset: pos, bytes: buf });
                pos += take as u64;
                budget -= take;
            }
        }
        if pos >= self.mem_floor && budget > 0 && pos < self.write_pos {
            let start = (pos - self.mem_floor) as usize;
            let take = (self.mem.len() - start).min(budget);
            let chunk: Vec<u8> = self.mem.iter().skip(start).take(take).copied().collect();
            slices.push(DataSlice { offset: pos, bytes: chunk });
            pos += take as u64;
        }
        Ok(ReadBatch::Data {
            slices,
            next_cursor: pos,
            // Terminated streams READ as complete at write_pos too: the
            // loss is already visible in the gap/loss log, the consumer
            // must not hang waiting for bytes that will never come.
            complete: (self.closed || self.terminated) && pos >= self.write_pos,
        })
    }

    /// Writer finished naturally. Final offsets are frozen from here.
    pub fn close(&mut self) {
        self.seal_pending_chunk();
        self.closed = true;
    }

    /// Honest retention GC: drop everything below `new_floor` and record
    /// the loss as Retention. No-op inside the window.
    pub fn gc_to(&mut self, new_floor: u64) -> io::Result<()> {
        let floor = self.retention_floor();
        if new_floor <= floor {
            return Ok(());
        }
        let target = new_floor.min(self.write_pos);
        if target <= self.mem_floor {
            // Entire movement inside the disk segment (or exactly at its end).
            if target > floor {
                self.disk_rotate_to(target)?;
                self.record_loss(floor, target, LossKind::Retention);
                self.evict_chunk_entries_below(target);
                self.evict_newlines_below(target);
            }
            return Ok(());
        }
        // Movement crosses into memory: retire the disk segment first.
        if let Some(d) = self.disk.take() {
            self.close_disk(d);
        }
        let drop_n = ((target - self.mem_floor) as usize).min(self.mem.len());
        for _ in 0..drop_n {
            self.mem.pop_front();
        }
        self.global_sub(drop_n);
        self.mem_floor = target;
        self.record_loss(floor, target, LossKind::Retention);
        self.evict_chunk_entries_below(target);
        self.evict_newlines_below(target);
        Ok(())
    }

    // ------------------------------------------------------------ internals

    fn disk_len(&self) -> u64 {
        self.disk.as_ref().map(|d| d.len).unwrap_or(0)
    }

    fn close_disk(&mut self, d: DiskSeg) {
        drop(d.file);
        self.fd_counter.fetch_sub(1, Ordering::SeqCst);
        let _ = std::fs::remove_file(&d.path);
    }

    fn open_disk(&mut self) -> io::Result<()> {
        if self.disk.is_some() {
            return Ok(());
        }
        let cur = self.fd_counter.load(Ordering::SeqCst);
        if cur >= self.fd_cap {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("spool fd budget exhausted ({cur}/{})", self.fd_cap),
            ));
        }
        std::fs::create_dir_all(&self.spool_dir)?;
        let path = self
            .spool_dir
            .join(format!("{}.log", self.stream.as_str()));
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(true)
            .open(&path)?;
        self.fd_counter.fetch_add(1, Ordering::SeqCst);
        self.disk = Some(DiskSeg {
            file,
            path,
            floor: 0,
            len: 0,
            dead: 0,
        });
        Ok(())
    }

    fn spill_to_fit(&mut self, incoming: usize) -> io::Result<()> {
        let used = self.global_mem.load(Ordering::SeqCst);
        if used + incoming <= self.global_mem_cap {
            return Ok(());
        }
        let want = (used + incoming).saturating_sub(self.global_mem_cap);
        let n = want.min(self.mem.len());
        if n > 0 {
            self.spill_n(n)?;
        }
        Ok(())
    }

    /// Move the oldest `n` memory bytes to the disk file.
    fn spill_n(&mut self, n: usize) -> io::Result<usize> {
        if n == 0 {
            return Ok(0);
        }
        if self.disk.is_none() {
            self.open_disk()?;
        }
        let n = n.min(self.mem.len());
        let mut buf = Vec::with_capacity(n);
        for _ in 0..n {
            if let Some(b) = self.mem.pop_front() {
                buf.push(b);
            }
        }
        let d = self.disk.as_mut().expect("disk opened above");
        d.file.seek(SeekFrom::Start(d.dead + d.len))?;
        d.file.write_all(&buf)?;
        d.len += buf.len() as u64;
        self.mem_floor += buf.len() as u64;
        self.global_sub(buf.len());
        Ok(buf.len())
    }

    /// Rotate the OLDEST disk prefix so disk_len() returns under the cap;
    /// the drop is a recorded Retention loss.
    fn rotate_disk_oldest(&mut self) -> io::Result<()> {
        let overflow = self.disk_len() - self.cfg.disk_cap;
        if overflow == 0 {
            return Ok(());
        }
        let floor = self.retention_floor();
        let target = (floor + overflow).min(self.mem_floor);
        self.disk_rotate_to(target)?;
        self.record_loss(floor, target, LossKind::Retention);
        self.evict_chunk_entries_below(target);
        self.evict_newlines_below(target);
        Ok(())
    }

    /// Advance the disk segment's logical floor, compacting the file once
    /// the dead prefix outweighs the live tail (amortized O(live)).
    fn disk_rotate_to(&mut self, new_floor: u64) -> io::Result<()> {
        let d = self.disk.as_mut().expect("rotation requires disk");
        debug_assert!(new_floor >= d.floor && new_floor <= d.floor + d.len);
        let gone = new_floor - d.floor;
        if gone == 0 {
            return Ok(());
        }
        d.floor = new_floor;
        d.len -= gone;
        d.dead += gone;
        if d.dead > d.len && d.len > 0 {
            let mut buf = vec![0u8; d.len as usize];
            d.file.seek(SeekFrom::Start(d.dead))?;
            d.file.read_exact(&mut buf)?;
            d.file.seek(SeekFrom::Start(0))?;
            d.file.write_all(&buf)?;
            d.file.set_len(d.len)?;
            d.dead = 0;
        }
        Ok(())
    }

    fn record_loss(&mut self, start: u64, end: u64, kind: LossKind) {
        if end <= start {
            return;
        }
        if let Some(last) = self.losses.last_mut() {
            if last.kind == kind && last.end == start {
                last.end = end;
                return;
            }
        }
        self.losses.push(LossRecord { start, end, kind });
    }

    fn index_newlines(&mut self, bytes: &[u8]) {
        for (i, b) in bytes.iter().enumerate() {
            if *b == b'\n' {
                self.newlines.push_back(self.write_pos + i as u64 + 1);
            }
        }
        // bounded index: overflow leaves the window, honestly reusing the
        // "before floor" accounting.
        while self.newlines.len() > MAX_NEWLINES {
            self.newlines.pop_front();
            self.lines_before_floor += 1;
        }
    }

    fn evict_newlines_below(&mut self, floor: u64) {
        while let Some(&f) = self.newlines.front() {
            if f <= floor {
                self.newlines.pop_front();
                self.lines_before_floor += 1;
            } else {
                break;
            }
        }
    }

    fn feed_chunks(&mut self, _first: u64, bytes: &[u8]) {
        if self.pending_chunk.is_empty() {
            // write_pos was already advanced past this feed (see write()).
            self.pending_chunk_first = self.write_pos - bytes.len() as u64;
        }
        let mut rest = bytes;
        loop {
            let room = CHUNK_TARGET - self.pending_chunk.len();
            let take = room.min(rest.len());
            self.pending_chunk.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.pending_chunk.len() == CHUNK_TARGET {
                self.seal_pending_chunk();
            }
            if rest.is_empty() {
                break;
            }
        }
    }

    fn seal_pending_chunk(&mut self) {
        if self.pending_chunk.is_empty() {
            return;
        }
        let mut h = Sha256::new();
        h.update(&self.pending_chunk);
        let idx = self.chunks.back().map(|c| c.index + 1).unwrap_or(0);
        let first = self.pending_chunk_first;
        let len = self.pending_chunk.len() as u32;
        self.chunks.push_back(ChunkEntry {
            index: idx,
            first_offset: first,
            len,
            sha256: h.finalize_hex(),
        });
        self.pending_chunk.clear();
        self.pending_chunk_first = first + len as u64;
    }

    fn evict_chunk_entries_below(&mut self, floor: u64) {
        while let Some(c) = self.chunks.front() {
            if c.first_offset + c.len as u64 <= floor {
                self.chunks.pop_front();
            } else {
                break;
            }
        }
    }

    fn set_hints(&mut self, spool_dir: PathBuf, exec: String) {
        self.spool_dir = spool_dir;
        self.exec = exec;
    }
}

// -------------------------------------------------------------------- registry

/// Node-level owner: global memory/FD budgets + per-(exec,stream) spools.
pub struct StreamRegistry {
    dir: PathBuf,
    global_mem: Arc<AtomicUsize>,
    global_mem_cap: usize,
    fd_counter: Arc<AtomicUsize>,
    fd_cap: usize,
    spools: HashMap<(String, StreamId), Arc<Mutex<StreamSpool>>>,
}

impl StreamRegistry {
    pub fn new(dir: PathBuf) -> StreamRegistry {
        StreamRegistry {
            dir,
            global_mem: Arc::new(AtomicUsize::new(0)),
            global_mem_cap: DEFAULT_GLOBAL_MEM_CAP,
            fd_counter: Arc::new(AtomicUsize::new(0)),
            fd_cap: DEFAULT_FD_CAP,
            spools: HashMap::new(),
        }
    }

    pub fn with_caps(dir: PathBuf, global_mem_cap: usize, fd_cap: usize) -> StreamRegistry {
        let mut r = StreamRegistry::new(dir);
        r.global_mem_cap = global_mem_cap;
        r.fd_cap = fd_cap;
        r
    }

    pub fn global_mem_used(&self) -> usize {
        self.global_mem.load(Ordering::SeqCst)
    }
    pub fn fds_used(&self) -> usize {
        self.fd_counter.load(Ordering::SeqCst)
    }

    /// Open (or fetch) the spool for one (execution, stream).
    pub fn open(&mut self, exec: &str, stream: StreamId, cfg: SpoolConfig) -> Arc<Mutex<StreamSpool>> {
        let key = (exec.to_string(), stream);
        self.spools
            .entry(key)
            .or_insert_with(|| {
                let mut s = StreamSpool {
                    stream,
                    cfg,
                    write_pos: 0,
                    closed: false,
                    terminated: false,
                    mem: VecDeque::new(),
                    mem_floor: 0,
                    disk: None,
                    losses: Vec::new(),
                    newlines: VecDeque::new(),
                    lines_before_floor: 0,
                    chunks: VecDeque::new(),
                    pending_chunk: Vec::new(),
                    pending_chunk_first: 0,
                    global_mem: Arc::clone(&self.global_mem),
                    global_mem_cap: self.global_mem_cap,
                    local_mem: 0,
                    fd_counter: Arc::clone(&self.fd_counter),
                    fd_cap: self.fd_cap,
                    spool_dir: PathBuf::new(),
                    exec: String::new(),
                };
                s.set_hints(self.dir.join(sanitize(exec)), exec.to_string());
                Arc::new(Mutex::new(s))
            })
            .clone()
    }

    /// Post-exit GC: drop every spool of `exec`; disk files are removed.
    pub fn remove_exec(&mut self, exec: &str) {
        let keys: Vec<(String, StreamId)> = self
            .spools
            .keys()
            .filter(|(e, _)| e == exec)
            .cloned()
            .collect();
        for k in keys {
            if let Some(arc) = self.spools.remove(&k) {
                if let Ok(mut s) = arc.lock() {
                    if let Some(d) = s.disk.take() {
                        s.close_disk(d);
                    }
                }
            }
        }
    }

    pub fn live_spool_count(&self) -> usize {
        self.spools.len()
    }
}

fn sanitize(exec: &str) -> String {
    exec.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// --------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmpdir(tag: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!(
            "ls-streams-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn cfg(mem_cap: usize, disk_cap: u64, overflow: DiskOverflow) -> SpoolConfig {
        SpoolConfig {
            mem_cap,
            disk_cap,
            overflow,
        }
    }

    fn drain(spool: &Arc<Mutex<StreamSpool>>, mut cursor: u64) -> (Vec<u8>, u64) {
        let mut out = Vec::new();
        loop {
            let mut s = spool.lock().unwrap();
            match s.read(cursor, 64 * 1024).unwrap() {
                ReadBatch::Data {
                    slices,
                    next_cursor,
                    complete: _,
                } => {
                    let mut progressed = false;
                    for sl in slices {
                        assert_eq!(sl.offset, cursor, "slices must be dense from the cursor");
                        out.extend_from_slice(&sl.bytes);
                        cursor = sl.offset + sl.bytes.len() as u64;
                        progressed = true;
                    }
                    let _ = next_cursor;
                    if !progressed || cursor >= s.write_pos() {
                        break;
                    }
                }
                ReadBatch::Gap { .. } => panic!("unexpected gap in drain"),
            }
        }
        (out, cursor)
    }

    fn patterned(n: usize) -> Vec<u8> {
        (0..n).map(|i| ((i * 31 + i / 7) % 251) as u8).collect()
    }

    #[test]
    fn basic_write_read_roundtrip_mem_only() {
        let dir = tmpdir("basic");
        let mut reg = StreamRegistry::new(dir.clone());
        let sp = reg.open("e1", StreamId::Stdout, SpoolConfig::default());
        let payload = patterned(70_000);
        {
            let mut s = sp.lock().unwrap();
            let r = s.write(&payload).unwrap();
            assert_eq!(r.accepted, payload.len());
            assert_eq!(r.spilled, 0);
            assert_eq!(s.write_pos(), payload.len() as u64);
            assert_eq!(s.retention_floor(), 0);
        }
        let (got, next) = drain(&sp, 0);
        assert_eq!(got, payload);
        assert_eq!(next, payload.len() as u64);
        assert_eq!(reg.fds_used(), 0, "no spill => no fd");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_mid_cursor_and_max_bytes_window() {
        let dir = tmpdir("mid");
        let mut reg = StreamRegistry::new(dir.clone());
        let sp = reg.open("e1", StreamId::Stdout, SpoolConfig::default());
        let payload = patterned(10_000);
        sp.lock().unwrap().write(&payload).unwrap();
        let mut s = sp.lock().unwrap();
        match s.read(4000, 3000).unwrap() {
            ReadBatch::Data {
                slices,
                next_cursor,
                complete,
            } => {
                assert_eq!(slices.len(), 1);
                assert_eq!(slices[0].offset, 4000);
                assert_eq!(slices[0].bytes, payload[4000..7000]);
                assert_eq!(next_cursor, 7000);
                assert!(!complete, "writer still open");
            }
            _ => panic!("expected data"),
        }
        s.close();
        match s.read(10_000, 1024).unwrap() {
            ReadBatch::Data {
                slices,
                next_cursor,
                complete,
            } => {
                assert!(slices.is_empty());
                assert_eq!(next_cursor, 10_000);
                assert!(complete, "closed + cursor at end");
            }
            _ => panic!("expected data"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mem_cap_spills_to_disk_byte_exact() {
        let dir = tmpdir("spill");
        let mut reg = StreamRegistry::new(dir.clone());
        let small = cfg(4096, DEFAULT_DISK_CAP, DiskOverflow::Rotate);
        let sp = reg.open("e1", StreamId::Stdout, small);
        let payload = patterned(4096 * 3 + 111);
        {
            let mut s = sp.lock().unwrap();
            let r = s.write(&payload).unwrap();
            assert!(r.spilled > 0, "spill must be reported");
            assert!(s.local_mem_bytes() <= 4096, "local mem within cap");
            assert!(s.write_pos() >= payload.len() as u64);
        }
        assert_eq!(reg.fds_used(), 1, "spill opened exactly one fd");
        let (got, _) = drain(&sp, 0);
        assert_eq!(got, payload, "mem+disk reassembly must be byte-exact");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn global_budget_shared_across_spools() {
        let dir = tmpdir("global");
        let mut reg = StreamRegistry::with_caps(dir.clone(), 8192, 16);
        let c = cfg(64 * 1024, DEFAULT_DISK_CAP, DiskOverflow::Rotate);
        let a = reg.open("a", StreamId::Stdout, c.clone());
        let b = reg.open("b", StreamId::Stdout, c);
        let burst = patterned(6000);
        a.lock().unwrap().write(&burst).unwrap();
        // second spool's write forces (its own) pre-spill; global stays
        // within cap + one write's transient overshoot:
        b.lock().unwrap().write(&burst).unwrap();
        assert!(
            reg.global_mem_used() <= 8192 + burst.len(),
            "global budget honest: {} used",
            reg.global_mem_used()
        );
        let (ga, _) = drain(&a, 0);
        let (gb, _) = drain(&b, 0);
        assert_eq!(ga, burst);
        assert_eq!(gb, burst);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn disk_cap_rotation_records_loss_and_gaps_old_cursor() {
        let dir = tmpdir("rotate");
        let mut reg = StreamRegistry::new(dir.clone());
        let c = cfg(1024, 2048, DiskOverflow::Rotate);
        let sp = reg.open("e1", StreamId::Stdout, c);
        let payload = patterned(2048 * 2 + 500);
        sp.lock().unwrap().write(&payload).unwrap();
        let floor;
        {
            let s = sp.lock().unwrap();
            floor = s.retention_floor();
            assert!(floor > 0, "rotation must have dropped a prefix");
            assert!(
                s.losses().iter().any(|l| l.kind == LossKind::Retention),
                "rotation must be a recorded Retention loss"
            );
            assert!(s.write_pos() >= payload.len() as u64);
            assert!(
                s.disk_len_bytes() <= 2048,
                "disk stays under cap: {}",
                s.disk_len_bytes()
            );
        }
        // old cursor => honest gap, never fabricated bytes:
        {
            let mut s = sp.lock().unwrap();
            match s.read(0, 100).unwrap() {
                ReadBatch::Gap {
                    requested,
                    available_from,
                    available_to,
                    kind,
                } => {
                    assert_eq!(requested, 0);
                    assert_eq!(available_from, floor);
                    assert_eq!(available_to, payload.len() as u64);
                    assert_eq!(kind, LossKind::Retention);
                }
                _ => panic!("stale cursor must answer Gap"),
            }
        }
        let (got, _) = drain(&sp, floor);
        assert_eq!(got, payload[floor as usize..], "suffix byte-exact");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn terminate_policy_truncates_and_reports() {
        let dir = tmpdir("term");
        let mut reg = StreamRegistry::new(dir.clone());
        let c = cfg(1024, 2048, DiskOverflow::TerminateRequested);
        let sp = reg.open("e1", StreamId::Stdout, c);
        let burst = patterned(5000);
        let r1 = sp.lock().unwrap().write(&burst).unwrap();
        assert!(r1.terminate_requested, "burst must trip the policy");
        assert_eq!(r1.accepted, 0);
        let r2 = sp.lock().unwrap().write(&burst).unwrap();
        assert_eq!(r2.accepted, 0, "post-terminate writes rejected");
        assert!(r2.terminate_requested);
        let mut s = sp.lock().unwrap();
        assert!(s.is_terminated());
        assert_eq!(
            s.write_pos(),
            (burst.len() * 2) as u64,
            "cursors stay producer-absolute"
        );
        assert!(
            s.losses().iter().any(|l| l.kind == LossKind::Overflow),
            "truncation is an Overflow loss"
        );
        // complete-at-write_pos must not hang a consumer after truncation:
        let end = s.write_pos();
        match s.read(end, 1024).unwrap() {
            ReadBatch::Data { complete, .. } => assert!(complete),
            other => panic!("expected terminal data read, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_after_close_is_an_error_never_silent() {
        let dir = tmpdir("afterclose");
        let mut reg = StreamRegistry::new(dir.clone());
        let sp = reg.open("e1", StreamId::Stdout, SpoolConfig::default());
        {
            let mut s = sp.lock().unwrap();
            s.write(b"hello").unwrap();
            s.close();
            assert!(s.write(b"late").is_err());
            assert_eq!(s.write_pos(), 5, "final offset frozen");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chunk_digests_verify_and_evict_with_retention() {
        let dir = tmpdir("chunks");
        let mut reg = StreamRegistry::new(dir.clone());
        let c = cfg(1024, CHUNK_TARGET as u64 * 2, DiskOverflow::Rotate);
        let sp = reg.open("e1", StreamId::Stdout, c);
        let payload = patterned(CHUNK_TARGET * 3 + 17);
        {
            let mut s = sp.lock().unwrap();
            s.write(&payload).unwrap();
            s.close();
            // sealed chunks (rest sealed at close) each verify independently:
            for c in s.chunk_index() {
                let seg = &payload[c.first_offset as usize..(c.first_offset + c.len as u64) as usize];
                let mut h = Sha256::new();
                h.update(seg);
                assert_eq!(c.sha256, h.finalize_hex(), "chunk {} digest", c.index);
            }
            // rotation dropped some prefix; no retained chunk below floor:
            let floor = s.retention_floor();
            assert!(floor > 0);
            assert!(
                s.chunk_index()
                    .iter()
                    .all(|c| c.first_offset + c.len as u64 > floor),
                "index bounded by retention window"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn newline_index_bounded_and_offset_exact() {
        let dir = tmpdir("lines");
        let mut reg = StreamRegistry::new(dir.clone());
        let sp = reg.open("e1", StreamId::Stdout, SpoolConfig::default());
        {
            let mut s = sp.lock().unwrap();
            s.write(b"a\nbb\nccc\n").unwrap();
            let offs: Vec<u64> = s.newline_offsets().iter().copied().collect();
            assert_eq!(offs, vec![2, 5, 9]);
            assert_eq!(s.lines_before_floor() , 0);
            // cap eviction
            let mut big = Vec::new();
            for i in 0..(MAX_NEWLINES + 100) {
                big.extend_from_slice(format!("{i}\n").as_bytes());
            }
            s.write(&big).unwrap();
            assert_eq!(s.newline_offsets().len(), MAX_NEWLINES);
            assert_eq!(s.lines_before_floor(), 3 + 100);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explicit_gc_gaps_and_resume() {
        let dir = tmpdir("gc");
        let mut reg = StreamRegistry::new(dir.clone());
        let sp = reg.open("e1", StreamId::Stdout, cfg(4096, 1 << 20, DiskOverflow::Rotate));
        let payload = patterned(6000);
        {
            let mut s = sp.lock().unwrap();
            s.write(&payload).unwrap();
            s.gc_to(5000).unwrap();
            assert_eq!(s.retention_floor(), 5000);
            match s.read(100, 64).unwrap() {
                ReadBatch::Gap {
                    available_from, kind, ..
                } => {
                    assert_eq!(available_from, 5000);
                    assert_eq!(kind, LossKind::Retention);
                }
                _ => panic!("expected gap"),
            }
        }
        let (got, _) = drain(&sp, 5000);
        assert_eq!(got, payload[5000..]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writer_never_blocked_without_any_reader() {
        let dir = tmpdir("noblock");
        let mut reg = StreamRegistry::with_caps(dir.clone(), 1 << 20, 16);
        let sp = reg.open("e1", StreamId::Stdout, cfg(8192, 64 << 20, DiskOverflow::Rotate));
        let data = patterned(2 << 20);
        let t0 = std::time::Instant::now();
        {
            let mut s = sp.lock().unwrap();
            for chunk in data.chunks(8192) {
                s.write(chunk).unwrap();
            }
            assert_eq!(s.write_pos(), data.len() as u64);
            assert!(s.local_mem_bytes() <= 8192);
        }
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(30),
            "writer must not block on absent readers"
        );
        let floor = sp.lock().unwrap().retention_floor();
        let (got, _) = drain(&sp, floor);
        assert!(got.len() <= data.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dual_streams_keep_independent_order() {
        let dir = tmpdir("dual");
        let mut reg = StreamRegistry::new(dir.clone());
        let o = reg.open("e1", StreamId::Stdout, SpoolConfig::default());
        let e = reg.open("e1", StreamId::Stderr, SpoolConfig::default());
        let a = b"out-1\n".to_vec();
        let b = b"err-A\n".to_vec();
        {
            o.lock().unwrap().write(&a).unwrap();
            e.lock().unwrap().write(&b).unwrap();
            o.lock().unwrap().write(&a).unwrap();
            e.lock().unwrap().write(&b).unwrap();
        }
        assert_eq!(o.lock().unwrap().write_pos(), 12);
        assert_eq!(e.lock().unwrap().write_pos(), 12);
        let (go, _) = drain(&o, 0);
        let (ge, _) = drain(&e, 0);
        assert_eq!(go, b"out-1\nout-1\n");
        assert_eq!(ge, b"err-A\nerr-A\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn registry_remove_exec_reclaims_fds_and_files() {
        let dir = tmpdir("reclaim");
        let mut reg = StreamRegistry::new(dir.clone());
        let c = cfg(512, 1 << 20, DiskOverflow::Rotate);
        let sp = reg.open("e9", StreamId::Stdout, c.clone());
        reg.open("e9", StreamId::Stderr, c);
        sp.lock().unwrap().write(&patterned(4096)).unwrap();
        assert_eq!(reg.fds_used(), 1);
        let exec_dir = dir.join("e9");
        assert!(exec_dir.join("stdout.log").exists());
        reg.remove_exec("e9");
        assert_eq!(reg.fds_used(), 0);
        assert!(!exec_dir.join("stdout.log").exists());
        assert_eq!(reg.live_spool_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn multi_exec_isolation() {
        let dir = tmpdir("multiexec");
        let mut reg = StreamRegistry::new(dir.clone());
        let a = reg.open("a", StreamId::Stdout, SpoolConfig::default());
        let b = reg.open("b", StreamId::Stdout, SpoolConfig::default());
        a.lock().unwrap().write(b"AAAA").unwrap();
        b.lock().unwrap().write(b"bbbbbbbb").unwrap();
        assert_eq!(a.lock().unwrap().write_pos(), 4);
        assert_eq!(b.lock().unwrap().write_pos(), 8);
        let (ga, _) = drain(&a, 0);
        let (gb, _) = drain(&b, 0);
        assert_eq!(ga, b"AAAA");
        assert_eq!(gb, b"bbbbbbbb");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
