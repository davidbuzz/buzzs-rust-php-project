//! Regenerates the oracle fixtures from `/usr/bin/php`:
//! `cargo test -p php-stdmisc --test regenerate -- --ignored`.

use std::path::PathBuf;
use std::process::Command;

#[test]
#[ignore = "needs /usr/bin/php; rewrites tests/fixtures"]
fn regenerate_fixtures() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut scripts: Vec<_> = std::fs::read_dir(root.join("gen"))
        .expect("tests/gen")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "php"))
        .collect();
    scripts.sort();
    for script in scripts {
        let name = script
            .file_stem()
            .expect("stem")
            .to_string_lossy()
            .into_owned();
        let out = Command::new("/usr/bin/php")
            .args([
                "-n",
                "-d",
                "memory_limit=1G",
                "-d",
                "error_reporting=-1",
                "-d",
                "display_errors=1",
            ])
            .arg(&script)
            .current_dir(root.join("gen"))
            .output()
            .expect("run php");
        assert!(
            out.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::write(
            root.join("fixtures").join(format!("{name}.txt")),
            out.stdout,
        )
        .expect("write fixture");
    }
}
