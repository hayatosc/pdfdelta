use std::{
    fs,
    path::Path,
    process::{self, Command},
};

fn download(url: &str, path: &Path) {
    let status = Command::new("curl")
        .args(["-fL", "--retry", "3", "--retry-delay", "1", "-o"])
        .arg(path)
        .arg(url)
        .status()
        .expect("curl should be available on the GitHub Actions runner");
    assert!(status.success(), "failed to download {url}");
}

#[test]
fn probe_irs_form_1040_2024_to_2025() {
    let directory = std::env::temp_dir().join(format!(
        "pdfdelta-realworld-probe-{}",
        process::id()
    ));
    fs::create_dir_all(&directory).expect("probe directory should be created");

    let old_pdf = directory.join("f1040--2024.pdf");
    let new_pdf = directory.join("f1040--2025.pdf");
    let json_report = directory.join("report.json");

    download(
        "https://www.irs.gov/pub/irs-prior/f1040--2024.pdf",
        &old_pdf,
    );
    download(
        "https://www.irs.gov/pub/irs-prior/f1040--2025.pdf",
        &new_pdf,
    );

    let text_output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .arg(&old_pdf)
        .arg(&new_pdf)
        .output()
        .expect("pdfdelta text comparison should run");

    let json_output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .arg(&old_pdf)
        .arg(&new_pdf)
        .args(["--json"])
        .arg(&json_report)
        .output()
        .expect("pdfdelta JSON comparison should run");

    let report = fs::read_to_string(&json_report).unwrap_or_else(|error| {
        format!("<JSON report was not produced: {error}>")
    });
    let report_excerpt: String = report.chars().take(20_000).collect();

    panic!(
        "REALWORLD IRS 1040 PROBE\n\
         old=https://www.irs.gov/pub/irs-prior/f1040--2024.pdf\n\
         new=https://www.irs.gov/pub/irs-prior/f1040--2025.pdf\n\
         text_status={:?}\n\
         text_stdout=\n{}\n\
         text_stderr=\n{}\n\
         json_status={:?}\n\
         json_stderr=\n{}\n\
         json_report_excerpt=\n{}",
        text_output.status.code(),
        String::from_utf8_lossy(&text_output.stdout),
        String::from_utf8_lossy(&text_output.stderr),
        json_output.status.code(),
        String::from_utf8_lossy(&json_output.stderr),
        report_excerpt,
    );
}
