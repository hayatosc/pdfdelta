use std::{
    io::Write,
    process::{Command, Output, Stdio},
};

fn query(input: &[u8], args: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .args(["text-envelope", "-"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start query");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input)
        .expect("write request");
    child.wait_with_output().expect("query result")
}

#[test]
fn field14_cli_exposes_joint_choices_without_certifying_correspondence() {
    let output = query(
        br#"{"pairs":[{"id":"field14","old":"14 Excess golden parachute payments","new":"14 "}]}"#,
        &[],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert_eq!(value["strict_status_changed"], false);
    assert_eq!(
        value["interpretation"],
        "conditional_on_supplied_order_and_correspondence"
    );
    let envelope = &value["results"][0]["result"]["envelope"];
    assert_eq!(envelope["claims"]["changed_source_lower"], 32);
    assert_eq!(envelope["ambiguous_old"], serde_json::json!([2, 9, 16, 26]));
    assert_eq!(
        envelope["new"][2]["matches"],
        serde_json::json!([2, 9, 16, 26])
    );
    assert_eq!(envelope["new"][2]["can_be_unmatched"], false);
}

#[test]
fn cli_output_and_work_refusals_do_not_publish_partial_proofs() {
    let input = br#"{"pairs":[{"id":"x","old":"AAAA","new":"AA"}]}"#;
    let output = query(input, &["--max-output-bytes", "256"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let output = query(input, &["--max-work", "0"]);
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON refusal");
    assert_eq!(
        value["results"][0]["result"],
        serde_json::json!({"status":"unavailable","reason":"work_limit"})
    );
}
