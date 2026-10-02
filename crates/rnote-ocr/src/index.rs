//! The search index: the text lines of all indexed files in one SQLite database.
//!
//! A file is indexed unit by unit. Units stay in the index for as long as the file still has a unit with the same
//! hash, so only the changed parts of a file need to be recognised again.

// Imports
use crate::{Bounds, Hit, Line, Query, Unit};
use anyhow::Context;
use rusqlite::{Connection, params};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// What a file was indexed from: its state on disk, and whether zhuyin was read. A file is looked at again when
/// either changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    /// The modification time in nanoseconds since the unix epoch.
    pub mtime: i64,
    pub size: i64,
    pub zhuyin: bool,
}

impl FileStamp {
    pub fn of(path: &Path, zhuyin: bool) -> anyhow::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        Ok(Self {
            mtime: metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos() as i64,
            size: metadata.len() as i64,
            zhuyin,
        })
    }
}

/// The search index.
#[derive(Debug)]
pub struct Index {
    conn: Connection,
}

impl Index {
    const FILE_NAME: &'static str = "index.sqlite";
    /// To be raised whenever the tables or the recognition models change. An index of another version is emptied
    /// and fills again as files get indexed.
    const VERSION: i32 = 2;
    // The stamp of a file is zero until all of its units are indexed.
    const SCHEMA: &'static str = "
        PRAGMA foreign_keys = ON;
        CREATE TABLE IF NOT EXISTS files (
            id INTEGER PRIMARY KEY,
            path TEXT UNIQUE NOT NULL,
            mtime INTEGER NOT NULL DEFAULT 0,
            size INTEGER NOT NULL DEFAULT 0,
            zhuyin INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS units (
            id INTEGER PRIMARY KEY,
            file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            hash INTEGER NOT NULL,
            page INTEGER NOT NULL,
            source INTEGER NOT NULL,
            UNIQUE (file_id, hash)
        );
        CREATE TABLE IF NOT EXISTS lines (
            id INTEGER PRIMARY KEY,
            unit_id INTEGER NOT NULL REFERENCES units(id) ON DELETE CASCADE,
            x REAL NOT NULL, y REAL NOT NULL, w REAL NOT NULL, h REAL NOT NULL,
            text TEXT NOT NULL,
            chars TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS lines_unit_id ON lines(unit_id);
    ";

    /// Opens the index in the data directory, creating it when it does not exist.
    pub fn open() -> anyhow::Result<Self> {
        Self::open_at(&crate::data_dir()?.join(Self::FILE_NAME))
    }

    /// Opens the index at the given path, creating it when it does not exist.
    pub fn open_at(path: &Path) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("Opening the index \"{}\" failed.", path.display()))?;
        Self::with_connection(conn)
    }

    fn with_connection(conn: Connection) -> anyhow::Result<Self> {
        let version: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != Self::VERSION {
            conn.execute_batch(
                "DROP TABLE IF EXISTS lines; DROP TABLE IF EXISTS units; DROP TABLE IF EXISTS files;",
            )?;
            conn.pragma_update(None, "user_version", Self::VERSION)?;
        }
        conn.execute_batch(Self::SCHEMA)?;
        Ok(Self { conn })
    }

    /// Whether the file is fully indexed in the given state.
    pub fn is_current(&self, path: &Path, stamp: FileStamp) -> anyhow::Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM files
             WHERE path = ?1 AND mtime = ?2 AND size = ?3 AND zhuyin = ?4)",
            params![path_str(path)?, stamp.mtime, stamp.size, stamp.zhuyin],
            |row| row.get(0),
        )?)
    }

    /// The hashes of the units that are indexed for the file.
    pub fn unit_hashes(&self, path: &Path) -> anyhow::Result<HashSet<u64>> {
        let hashes = self
            .conn
            .prepare(
                "SELECT units.hash FROM units JOIN files ON files.id = units.file_id
                 WHERE files.path = ?1",
            )?
            .query_map(params![path_str(path)?], |row| row.get::<_, i64>(0))?
            .map(|hash| hash.map(|h| h as u64))
            .collect::<Result<HashSet<u64>, _>>()?;
        Ok(hashes)
    }

    /// Adds a unit of the file with its lines.
    pub fn insert_unit(&mut self, path: &Path, unit: Unit, lines: &[Line]) -> anyhow::Result<()> {
        let tx = self.conn.transaction()?;
        let file_id = file_id(&tx, path)?;
        tx.execute(
            "DELETE FROM units WHERE file_id = ?1 AND hash = ?2",
            params![file_id, unit.hash as i64],
        )?;
        tx.execute(
            "INSERT INTO units (file_id, hash, page, source) VALUES (?1, ?2, ?3, ?4)",
            params![file_id, unit.hash as i64, unit.page, unit.source as u8],
        )?;
        let unit_id = tx.last_insert_rowid();
        {
            let mut insert = tx.prepare(
                "INSERT INTO lines (unit_id, x, y, w, h, text, chars)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for line in lines {
                insert.execute(params![
                    unit_id,
                    line.bounds.x,
                    line.bounds.y,
                    line.bounds.w,
                    line.bounds.h,
                    line.text(),
                    serde_json::to_string(&line.chars)?,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Marks the file as fully indexed in the given state.
    ///
    /// `units` are all units the file has now. Indexed units that are not among them are removed.
    pub fn finish_file(
        &mut self,
        path: &Path,
        stamp: FileStamp,
        units: &[Unit],
    ) -> anyhow::Result<()> {
        let pages = units
            .iter()
            .map(|unit| (unit.hash as i64, unit.page))
            .collect::<HashMap<i64, u32>>();
        let tx = self.conn.transaction()?;
        let file_id = file_id(&tx, path)?;
        let indexed = tx
            .prepare("SELECT id, hash FROM units WHERE file_id = ?1")?
            .query_map(params![file_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<(i64, i64)>, _>>()?;
        for (unit_id, hash) in indexed {
            match pages.get(&hash) {
                // The page of an unchanged unit changes when pages before it gain or lose content
                Some(page) => tx.execute(
                    "UPDATE units SET page = ?1 WHERE id = ?2",
                    params![page, unit_id],
                )?,
                None => tx.execute("DELETE FROM units WHERE id = ?1", params![unit_id])?,
            };
        }
        tx.execute(
            "UPDATE files SET mtime = ?1, size = ?2, zhuyin = ?3 WHERE id = ?4",
            params![stamp.mtime, stamp.size, stamp.zhuyin, file_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Removes the files that no longer exist on disk. Returns how many were removed.
    pub fn remove_missing(&mut self) -> anyhow::Result<usize> {
        let missing = self
            .conn
            .prepare("SELECT path FROM files")?
            .query_map([], |row| row.get::<_, String>(0))?
            .filter(|path| path.as_ref().is_ok_and(|p| !Path::new(p).exists()))
            .collect::<Result<Vec<String>, _>>()?;
        for path in missing.iter() {
            self.conn
                .execute("DELETE FROM files WHERE path = ?1", params![path])?;
        }
        Ok(missing.len())
    }

    /// Finds the query in all indexed lines, the best hits first. See [Query::find] for what matches.
    pub fn search(&self, query: &str) -> anyhow::Result<Vec<Hit>> {
        let Some(query) = Query::new(query) else {
            return Ok(Vec::new());
        };

        let mut hits = Vec::new();
        let mut select = self.conn.prepare(
            "SELECT files.path, units.page, lines.x, lines.y, lines.w, lines.h, lines.text, lines.chars
             FROM lines JOIN units ON units.id = lines.unit_id JOIN files ON files.id = units.file_id",
        )?;
        let mut rows = select.query([])?;
        while let Some(row) = rows.next()? {
            let line = Line {
                bounds: Bounds {
                    x: row.get(2)?,
                    y: row.get(3)?,
                    w: row.get(4)?,
                    h: row.get(5)?,
                },
                chars: serde_json::from_str(row.get_ref(7)?.as_str()?)?,
            };
            for found in query.find(&line) {
                hits.push(Hit {
                    path: PathBuf::from(row.get::<_, String>(0)?),
                    page: row.get(1)?,
                    bounds: found.bounds,
                    text: row.get(6)?,
                    score: found.score,
                });
            }
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        Ok(hits)
    }
}

/// The id of the file's row, which is created when the file is not known yet.
fn file_id(conn: &Connection, path: &Path) -> anyhow::Result<i64> {
    let path = path_str(path)?;
    conn.execute(
        "INSERT INTO files (path) VALUES (?1) ON CONFLICT (path) DO NOTHING",
        params![path],
    )?;
    Ok(conn.query_row(
        "SELECT id FROM files WHERE path = ?1",
        params![path],
        |row| row.get(0),
    )?)
}

fn path_str(path: &Path) -> anyhow::Result<&str> {
    path.to_str()
        .with_context(|| format!("The path \"{}\" is not valid UTF-8.", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Candidate, CharBox, Source};

    const PATH: &str = "/notes/a.rnote";
    const STAMP: FileStamp = FileStamp {
        mtime: 7,
        size: 7,
        zhuyin: false,
    };
    const BOUNDS: Bounds = Bounds {
        x: 0.0,
        y: 5.0,
        w: 30.0,
        h: 12.0,
    };

    /// A line read with full confidence, with its characters spread evenly over the bounds.
    fn typed(text: &str, bounds: Bounds) -> Line {
        let advance = bounds.w / text.chars().count() as f64;
        let chars = text
            .chars()
            .enumerate()
            .map(|(i, ch)| {
                CharBox::exact(
                    ch,
                    bounds.x + i as f64 * advance,
                    bounds.x + (i + 1) as f64 * advance,
                )
            })
            .collect();
        Line { bounds, chars }
    }

    fn unit(hash: u64, page: u32) -> Unit {
        Unit {
            hash,
            page,
            source: Source::Ink,
        }
    }

    /// An index holding one file with one unit per line.
    fn index_with(lines: Vec<Line>) -> Index {
        let mut index = Index::with_connection(Connection::open_in_memory().unwrap()).unwrap();
        let units = (0..lines.len() as u64)
            .map(|i| unit(i, 3))
            .collect::<Vec<Unit>>();
        for (unit, line) in units.iter().zip(lines) {
            index.insert_unit(Path::new(PATH), *unit, &[line]).unwrap();
        }
        index.finish_file(Path::new(PATH), STAMP, &units).unwrap();
        index
    }

    #[test]
    fn matches_on_lower_candidates_and_ranks_them_below_exact() {
        let candidates = |readings: &[(char, f32)]| {
            readings
                .iter()
                .map(|&(ch, confidence)| Candidate { ch, confidence })
                .collect::<Vec<Candidate>>()
        };
        let mut misread = typed("我没有", BOUNDS);
        misread.chars[1].candidates = candidates(&[('没', 0.6), ('沒', 0.3)]);
        let index = index_with(vec![misread, typed("我沒有", BOUNDS)]);

        let hits = index.search("沒有").unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].text, "我沒有");
        assert_eq!(hits[1].text, "我没有");
        assert!(hits[0].score > 1.0 && hits[1].score < 1.0);
        // A hit covers the matched characters only
        assert_eq!((hits[1].bounds.x, hits[1].bounds.w), (10.0, 20.0));
        assert_eq!((hits[1].bounds.y, hits[1].bounds.h), (5.0, 12.0));
        assert_eq!(hits[1].page, 3);

        assert!(index.search("有我").unwrap().is_empty());
    }

    #[test]
    fn ignores_whitespace_and_case() {
        let index = index_with(vec![typed("Hello World", BOUNDS)]);
        assert_eq!(index.search("LOW or").unwrap().len(), 1);
        assert!(index.search("  ").unwrap().is_empty());
    }

    #[test]
    fn a_file_read_without_zhuyin_is_not_current_with_it() {
        let index = index_with(vec![typed("text", BOUNDS)]);
        let path = Path::new(PATH);
        assert!(index.is_current(path, STAMP).unwrap());
        let with_zhuyin = FileStamp {
            zhuyin: true,
            ..STAMP
        };
        assert!(!index.is_current(path, with_zhuyin).unwrap());
    }

    #[test]
    fn ignores_tone_marks() {
        let index = index_with(vec![typed("ㄇㄚˇ ㄇㄚ˙", BOUNDS)]);
        assert_eq!(index.search("ㄇㄚㄇㄚ").unwrap().len(), 1);
        assert_eq!(index.search("ㄇㄚˋㄇㄚ").unwrap().len(), 1);
        assert!(index.search("ˇ").unwrap().is_empty());
    }

    #[test]
    fn a_query_without_zhuyin_matches_across_ruby_zhuyin() {
        let index = index_with(vec![typed("老ㄌ師ㄕ好", BOUNDS)]);
        let hits = index.search("老師").unwrap();
        assert_eq!(hits.len(), 1);
        // From the start of 老 to the end of 師
        assert_eq!((hits[0].bounds.x, hits[0].bounds.w), (0.0, 18.0));
        // Found once, though it matches with and without the zhuyin left out
        assert_eq!(index.search("師").unwrap().len(), 1);
        // A query with zhuyin is matched as it is
        assert_eq!(index.search("ㄌ師").unwrap().len(), 1);
        assert!(index.search("ㄌㄕ").unwrap().is_empty());
    }

    #[test]
    fn an_index_of_another_version_is_emptied() {
        let index = index_with(vec![typed("kept", BOUNDS)]);
        let same = Index::with_connection(index.conn).unwrap();
        assert_eq!(same.search("kept").unwrap().len(), 1);

        same.conn
            .pragma_update(None, "user_version", Index::VERSION + 1)
            .unwrap();
        let other = Index::with_connection(same.conn).unwrap();
        assert!(other.search("kept").unwrap().is_empty());
    }

    #[test]
    fn a_file_is_current_only_once_finished() {
        let mut index = index_with(Vec::new());
        let path = Path::new("/notes/b.rnote");
        index
            .insert_unit(path, unit(1, 0), &[typed("partial", BOUNDS)])
            .unwrap();
        assert!(!index.is_current(path, STAMP).unwrap());
        // What is indexed so far can be found already, and does not need to be recognised again
        assert_eq!(index.search("partial").unwrap().len(), 1);
        assert!(index.unit_hashes(path).unwrap().contains(&1));

        index.finish_file(path, STAMP, &[unit(1, 0)]).unwrap();
        assert!(index.is_current(path, STAMP).unwrap());
    }

    #[test]
    fn finishing_keeps_unchanged_units_and_removes_the_rest() {
        let lines = vec![typed("kept", BOUNDS), typed("gone", BOUNDS)];
        let mut index = index_with(lines);
        let path = Path::new(PATH);
        let changed = FileStamp {
            mtime: 8,
            size: 8,
            zhuyin: false,
        };

        // Unit 0 is unchanged but now on another page, unit 1 no longer exists
        index.finish_file(path, changed, &[unit(0, 4)]).unwrap();
        assert!(index.is_current(path, changed).unwrap());
        assert_eq!(index.unit_hashes(path).unwrap(), HashSet::from([0]));
        assert_eq!(index.search("kept").unwrap()[0].page, 4);
        assert!(index.search("gone").unwrap().is_empty());

        // The file does not exist on disk
        assert_eq!(index.remove_missing().unwrap(), 1);
        assert!(index.search("kept").unwrap().is_empty());
    }
}
