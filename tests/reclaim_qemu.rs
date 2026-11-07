//! Run explicitly with QEMU installed on a filesystem supporting hole punching:
//! `cargo test --no-default-features --features sync --test reclaim_qemu -- --ignored`

#![cfg(all(feature = "sync", unix))]

use imago::file::File;
use imago::qcow2::Qcow2;
use imago::{FormatAccess, FormatDriverBuilder, PermissiveImplicitOpenGate};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "imago-reclaim-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(program: &str, args: &[&str], path: &Path) {
    let output = Command::new(program)
        .args(args)
        .arg(path)
        .output()
        .expect("QEMU prerequisite missing");
    assert!(
        output.status.success(),
        "{program}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn allocated(path: &Path) -> u64 {
    fs::metadata(path).unwrap().blocks() * 512
}

fn open(path: &Path) -> FormatAccess<File> {
    FormatAccess::new(
        Qcow2::<File>::builder_path(path)
            .write(true)
            .reclaim(true)
            .open(PermissiveImplicitOpenGate::default())
            .unwrap(),
    )
}

#[test]
#[ignore = "requires qemu-img, qemu-io and a hole-punch-capable filesystem"]
fn reclaim_releases_host_blocks_and_preserves_snapshot() {
    for snapshot in [false, true] {
        let fixture = Fixture::new();
        let path = fixture.0.join("disk.qcow2");
        // qemu-img expects the size after the filename.
        let output = Command::new("qemu-img")
            .args(["create", "-f", "qcow2", "-o", "cluster_size=65536"])
            .arg(&path)
            .arg("1G")
            .output()
            .unwrap();
        assert!(output.status.success());
        run(
            "qemu-io",
            &["-f", "qcow2", "-c", "write -P 0x5a 0 64M", "-c", "flush"],
            &path,
        );
        if snapshot {
            run("qemu-img", &["snapshot", "-c", "saved"], &path);
        }
        let before = allocated(&path);
        let mut image = open(&path);
        image.discard_to_zero(0, 64 * 1024 * 1024).unwrap();
        image.flush().unwrap();
        image.sync().unwrap();
        let mut bytes = vec![1; 64 * 1024];
        image.read(&mut bytes[..], 0).unwrap();
        assert!(bytes.iter().all(|b| *b == 0));
        drop(image);
        let after = allocated(&path);
        if snapshot {
            assert!(after >= before - 1024 * 1024);
        } else {
            assert!(
                after < before / 8,
                "no physical reclaim: {before} -> {after}"
            );
        }
        run("qemu-img", &["check", "-f", "qcow2"], &path);
        if snapshot {
            run("qemu-img", &["snapshot", "-a", "saved"], &path);
            run(
                "qemu-io",
                &["-f", "qcow2", "-c", "read -P 0x5a 0 64M"],
                &path,
            );
        } else {
            let mut image = open(&path);
            for _ in 0..32 {
                image
                    .write(&vec![0xa5; 1024 * 1024][..], 128 * 1024 * 1024)
                    .unwrap();
                image
                    .discard_to_zero(128 * 1024 * 1024, 1024 * 1024)
                    .unwrap();
            }
            image.flush().unwrap();
            image.sync().unwrap();
            drop(image);
            run("qemu-img", &["check", "-f", "qcow2"], &path);
            assert!(allocated(&path) < 2 * 1024 * 1024);
        }
    }
}
