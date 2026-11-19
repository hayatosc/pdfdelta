//! End-to-end checks for `pdfbench flow-verify`.

use std::process::Command;

#[test]
fn flow_verify_accepts_every_known_fixture() {
    let output = Command::new(env!("CARGO_BIN_EXE_pdfbench"))
        .arg("flow-verify")
        .output()
        .expect("pdfbench runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("flow verification: 56/56 passed"),
        "{stdout}"
    );
    assert!(
        stdout.contains("case=text-replacement renderer=lopdf-tj"),
        "{stdout}"
    );
    assert!(
        stdout.contains("case=external/vertical-tectonic glyph-insert"),
        "{stdout}"
    );
    assert!(!stdout.contains("FAIL "), "{stdout}");
}
