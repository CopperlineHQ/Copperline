//! Preflight the pinned generator's version-16 numbered-file format before
//! handing it to C. Missing final files must never look like successful EOF.

use std::fs;
use std::path::{Path, PathBuf};

pub const PR_OPCODES: &str = include_str!("../pr-opcodes.txt");

fn read(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|error| format!("{}: {error}", path.display()))
}

pub fn validate(root: &Path, opcode: &str, require_pr: bool) -> Result<(), String> {
    let mut names = if opcode == "all" {
        let mut names = Vec::new();
        for entry in fs::read_dir(root).map_err(|error| format!("{}: {error}", root.display()))? {
            let entry = entry.map_err(|error| error.to_string())?;
            if entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_dir()
            {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        names
    } else {
        vec![opcode.to_owned()]
    };
    names.sort();
    if names.is_empty() {
        return Err(format!("{}: no opcode directories", root.display()));
    }
    if require_pr {
        let mut expected: Vec<_> = PR_OPCODES.lines().map(str::to_owned).collect();
        expected.sort();
        if names != expected {
            return Err(format!(
                "PR corpus opcode set differs: expected {expected:?}, found {names:?}"
            ));
        }
    }
    for name in names {
        validate_opcode(&root.join(name))?;
    }
    Ok(())
}

fn validate_opcode(dir: &Path) -> Result<(), String> {
    let header = read(&dir.join("0000.dat"))?;
    if header.len() != 98 || header[..4] != 16u32.to_be_bytes() {
        return Err(format!(
            "{}: expected a version-16 generator header",
            dir.display()
        ));
    }
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|error| error.to_string())?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<_, _>>()
        .map_err(|error| error.to_string())?;
    files.retain(|path| path.extension().is_some_and(|ext| ext == "dat"));
    files.sort();
    if files.len() < 2 {
        return Err(format!("{}: no test data files", dir.display()));
    }
    for (index, file) in files.iter().enumerate().skip(1) {
        if file.file_name().unwrap() != format!("{index:04}.dat").as_str() {
            return Err(format!(
                "{}: missing numbered data file {index:04}.dat",
                dir.display()
            ));
        }
        let data = read(file)?;
        if data.len() <= 18 || data[..8] != header[..8] {
            return Err(format!("{}: truncated or stale data file", file.display()));
        }
        let last = if index == files.len() - 1 {
            0xff
        } else {
            index as u8
        };
        if data[data.len() - 2..] != [0xff, last] {
            return Err(format!(
                "{}: missing completion footer or incorrect file number",
                file.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "cputest-corpus-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(dir.join("MOVE.L")).unwrap();
            let mut header = vec![0; 98];
            header[..4].copy_from_slice(&16u32.to_be_bytes());
            fs::write(dir.join("MOVE.L/0000.dat"), &header).unwrap();
            let mut data = vec![0; 19];
            data[..8].copy_from_slice(&header[..8]);
            data[17..].copy_from_slice(&[0xff, 0xff]);
            fs::write(dir.join("MOVE.L/0001.dat"), data).unwrap();
            Self(dir)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn complete_numbered_corpus_is_accepted() {
        let f = Fixture::new();
        validate(&f.0, "all", false).unwrap();
    }

    #[test]
    fn empty_and_header_only_corpora_fail() {
        let f = Fixture::new();
        fs::remove_file(f.0.join("MOVE.L/0001.dat")).unwrap();
        assert!(validate(&f.0, "all", false)
            .unwrap_err()
            .contains("no test data"));
        fs::remove_dir_all(f.0.join("MOVE.L")).unwrap();
        assert!(validate(&f.0, "all", false)
            .unwrap_err()
            .contains("no opcode"));
    }

    #[test]
    fn missing_tail_middle_and_stale_files_fail() {
        let f = Fixture::new();
        let file = f.0.join("MOVE.L/0001.dat");
        let mut data = read(&file).unwrap();
        data[18] = 1; // Non-final footer, but the next file is absent.
        fs::write(&file, &data).unwrap();
        assert!(validate(&f.0, "all", false)
            .unwrap_err()
            .contains("completion footer"));
        data[18] = 0xff;
        fs::write(f.0.join("MOVE.L/0003.dat"), &data).unwrap();
        assert!(validate(&f.0, "all", false).is_err());
        fs::remove_file(f.0.join("MOVE.L/0003.dat")).unwrap();
        data[7] = 1;
        fs::write(&file, &data).unwrap();
        assert!(validate(&f.0, "all", false).unwrap_err().contains("stale"));
    }

    #[test]
    fn pr_corpus_requires_every_named_opcode() {
        let f = Fixture::new();
        assert!(validate(&f.0, "all", true)
            .unwrap_err()
            .contains("opcode set differs"));
    }
}
