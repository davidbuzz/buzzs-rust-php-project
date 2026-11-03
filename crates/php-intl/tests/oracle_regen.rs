//! Regenerates the oracle fixtures from `/usr/bin/php` (ICU 74.2):
//! `cargo test -p php-intl --test oracle_regen -- --ignored`.

use std::path::PathBuf;
use std::process::Command;

#[test]
#[ignore = "needs /usr/bin/php with ext/intl; rewrites tests/fixtures"]
fn regenerate_fixtures() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut scripts: Vec<_> = std::fs::read_dir(root.join("oracle"))
        .expect("oracle dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "php"))
        .collect();
    scripts.sort();
    for script in scripts {
        let name = script
            .file_stem()
            .expect("stem")
            .to_string_lossy()
            .into_owned();
        let out = Command::new("/usr/bin/php")
            .args(["-d", "memory_limit=1G"])
            .arg(&script)
            .output()
            .expect("run php");
        assert!(
            out.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::write(
            root.join("fixtures").join(format!("{name}.tsv")),
            out.stdout,
        )
        .expect("write fixture");
    }
}
