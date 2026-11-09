//! Exercise process exit statuses, including a valid corpus containing only
//! CT_END_SKIP records. That must not pass merely because rounds were counted.

use std::{fs, path::PathBuf, process::Command};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn empty_header_only_and_all_skipped_corpora_exit_one() {
    let fixture = Fixture(std::env::temp_dir().join(format!("cputest-cli-{}", std::process::id())));
    let root = fixture.0.join("68020");
    fs::create_dir_all(&root).unwrap();
    let run = || {
        let out = Command::new(env!("CARGO_BIN_EXE_cputest-runner"))
            .args([fixture.0.to_str().unwrap(), "all", "68020"])
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(1),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    };
    assert!(run().contains("no opcode directories"));

    let opcode = root.join("MOVE.L");
    fs::create_dir(&opcode).unwrap();
    // Version 16 header, no low/high memory, 256 KiB test memory. These are
    // format fields, not generated instruction-test data or external assets.
    let fields: [u32; 16] = [
        16,
        1,
        0x80008000,
        0x860000,
        0x40000,
        0x87ffa0,
        0x80020000,
        0,
        u32::MAX,
        u32::MAX,
        u32::MAX,
        u32::MAX,
        u32::MAX,
        u32::MAX,
        0x860400,
        0x860840,
    ];
    let mut header: Vec<_> = fields.into_iter().flat_map(u32::to_be_bytes).collect();
    header.resize(98, 0);
    fs::write(opcode.join("0000.dat"), &header).unwrap();
    assert!(run().contains("no test data files"));

    fs::write(root.join("tmem.dat"), vec![0; 0x40000]).unwrap();
    let mut data = header[..8].to_vec();
    data.resize(16, 0);
    // End init, one CCR round, skip, end group, final data/file footer.
    data.extend_from_slice(&[0xc0, 1, 0xc1, 0x80, 0xff, 0xff]);
    fs::write(opcode.join("0001.dat"), &data).unwrap();
    assert!(run().contains("No test cases executed"));

    // A self-loop whose unchanged state could otherwise match the expected
    // state after the callback's instruction ceiling must also fail.
    let mut memory = vec![0; 0x40000];
    memory[0x1ffa0..0x1ffa2].copy_from_slice(&[0x60, 0xfe]);
    fs::write(root.join("tmem.dat"), memory).unwrap();
    data.truncate(16);
    // CT_ENDPC (long), end init, one CCR, no changed result fields, end.
    data.extend_from_slice(&[
        0x5a, 0x00, 0x87, 0xff, 0xa4, 0xc0, 1, 0x80, 0, 0x80, 0xff, 0xff,
    ]);
    fs::write(opcode.join("0001.dat"), data).unwrap();
    assert!(run().contains("did not terminate within 64 steps"));
}
