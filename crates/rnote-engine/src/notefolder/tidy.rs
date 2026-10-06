//! Taking out of a note folder what is no longer needed.

// Imports
use super::NoteFolder;
use super::record::{self, BatchName, ContentKind, RecordHead, Stamp, Subject};
use anyhow::Context;
use std::collections::{HashMap, HashSet};
use tracing::warn;

/// A batch below this size is small: it is written again as soon as something in it is dead, and small batches are
/// put together once there are more than [MAX_SMALL_BATCHES] of them. Sizes are those of the uncompressed lines.
const SMALL_BATCH_BYTES: usize = 256 * 1024;
const MAX_SMALL_BATCHES: usize = 16;
/// A batch that is not small is only written again when more than this share of it is dead. Removing a single
/// stroke should not make a sync tool upload a large batch again.
const MAX_DEAD_SHARE: f64 = 0.25;
/// The size of the batches that records are put together into.
const BATCH_BYTES: usize = 1024 * 1024;

/// What tidying did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TidyReport {
    pub removed_batches: usize,
    pub written_batches: usize,
    pub dropped_records: usize,
}

#[derive(Debug)]
struct OwnRecord {
    head: RecordHead,
    line: String,
    /// Whether a record with a higher stamp exists about the same part of the note.
    beaten: bool,
}

#[derive(Debug)]
struct OwnBatch {
    name: BatchName,
    records: Vec<OwnRecord>,
}

impl OwnBatch {
    fn bytes(&self, counts: impl Fn(&OwnRecord) -> bool) -> usize {
        let counted = self.records.iter().filter(|record| counts(record));
        counted.map(|record| record.line.len()).sum()
    }

    fn is_small(&self) -> bool {
        self.bytes(|_| true) < SMALL_BATCH_BYTES
    }
}

impl NoteFolder {
    /// Takes out of the batches of this device what is no longer needed, and puts small batches together. The
    /// batches of other devices are never touched. To be called when a note is opened or closed.
    ///
    /// - A record is dead when there is a record with a higher stamp about the same part of the note, in any
    ///   batch.
    /// - The mark of a removed stroke is dead once no batch holds the stroke any more: none of another device,
    ///   and none of this device after this tidying. Until then the mark is what keeps the stroke removed.
    ///
    /// The batches that are affected are written again without their dead records, and then deleted.
    pub fn tidy(&mut self) -> anyhow::Result<TidyReport> {
        let ink_dir = self.ink_dir();
        let mut newest: HashMap<Subject, Stamp> = HashMap::new();
        let mut held_by_others: HashSet<Subject> = HashSet::new();
        let mut own_batches = Vec::new();
        for name in record::list_batches(&ink_dir)? {
            // What can not be read can not be judged: a batch a sync tool is still writing could hold anything
            let Ok(lines) = record::read_batch(&name.path(&ink_dir)) else {
                warn!(
                    "Not tidying {:?}, the batch {name:?} can not be read.",
                    self.dir
                );
                return Ok(TidyReport::default());
            };
            let mut records = Vec::new();
            for line in lines {
                let Ok(head) = serde_json::from_str::<RecordHead>(&line) else {
                    warn!(
                        "Not tidying {:?}, a record of {name:?} can not be read.",
                        self.dir
                    );
                    return Ok(TidyReport::default());
                };
                let is_newer = newest
                    .get(&head.subject)
                    .is_none_or(|stamp| head.stamp > *stamp);
                if is_newer {
                    newest.insert(head.subject.clone(), head.stamp.clone());
                }
                if name.device == self.device.id {
                    records.push(OwnRecord {
                        head,
                        line,
                        beaten: false,
                    });
                } else {
                    held_by_others.insert(head.subject);
                }
            }
            if name.device == self.device.id {
                own_batches.push(OwnBatch { name, records });
            }
        }
        for record in own_batches.iter_mut().flat_map(|b| b.records.iter_mut()) {
            record.beaten = newest.get(&record.head.subject) != Some(&record.head.stamp);
        }

        // The batches to write again because of their beaten records
        let n_small = own_batches.iter().filter(|b| b.is_small()).count();
        let mut rewritten = own_batches
            .iter()
            .filter(|batch| {
                let beaten = batch.bytes(|record| record.beaten);
                let share = beaten as f64 / batch.bytes(|_| true).max(1) as f64;
                let worth_it = batch.is_small() || share > MAX_DEAD_SHARE;
                (beaten > 0 && worth_it) || (batch.is_small() && n_small > MAX_SMALL_BATCHES)
            })
            .map(|batch| batch.name.clone())
            .collect::<HashSet<BatchName>>();

        // A mark can go when no batch holds its stroke any more. The beaten records of the stroke in a batch that
        // is not written again would bring the stroke back.
        let kept_strokes = own_batches
            .iter()
            .filter(|batch| !rewritten.contains(&batch.name))
            .flat_map(|batch| batch.records.iter())
            .filter(|record| record.beaten)
            .map(|record| record.head.subject.clone())
            .collect::<HashSet<Subject>>();
        let is_dead_mark = |record: &OwnRecord| {
            matches!(record.head.content, ContentKind::Removed)
                && !record.beaten
                && !held_by_others.contains(&record.head.subject)
                && !kept_strokes.contains(&record.head.subject)
        };
        let with_dead_marks = own_batches
            .iter()
            .filter(|batch| batch.is_small() && batch.records.iter().any(&is_dead_mark))
            .map(|batch| batch.name.clone())
            .collect::<Vec<BatchName>>();
        rewritten.extend(with_dead_marks);
        if rewritten.is_empty() {
            return Ok(TidyReport::default());
        }

        let mut report = TidyReport {
            removed_batches: rewritten.len(),
            ..TidyReport::default()
        };
        // The live records, put together into batches. After an interrupted tidying a record can be there twice.
        let mut live = HashSet::<&Subject>::new();
        let mut batches: Vec<Vec<String>> = Vec::new();
        let mut batch_bytes = 0;
        for record in own_batches
            .iter()
            .filter(|batch| rewritten.contains(&batch.name))
            .flat_map(|batch| batch.records.iter())
        {
            if record.beaten || is_dead_mark(record) || !live.insert(&record.head.subject) {
                report.dropped_records += 1;
                continue;
            }
            if batches.is_empty() || batch_bytes + record.line.len() > BATCH_BYTES {
                batches.push(Vec::new());
                batch_bytes = 0;
            }
            batch_bytes += record.line.len();
            batches
                .last_mut()
                .expect("a batch was pushed")
                .push(record.line.clone());
        }

        // First the new batches, then the old ones go. Stopped in between, the note has records twice, not never.
        let first = self.next_batch()?;
        for (i, lines) in batches.iter().enumerate() {
            let name = BatchName {
                seq: first.seq + i as u32,
                ..first.clone()
            };
            let file = name.path(&ink_dir);
            record::write_batch(&file, lines)
                .with_context(|| format!("Writing the batch {file:?} failed."))?;
            self.batches.insert(name);
            report.written_batches += 1;
        }
        for name in rewritten {
            let file = name.path(&ink_dir);
            std::fs::remove_file(&file)
                .with_context(|| format!("Removing the batch {file:?} failed."))?;
            self.batches.remove(&name);
        }
        Ok(report)
    }
}
