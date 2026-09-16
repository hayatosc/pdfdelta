
#[test]
fn native_interval_change_authority_does_not_survive_report_deserialization() {
    use pdfdelta_core::document::NativeTextIntervalComparison;
    let old = fixture_rows(&["First boundary.", "Budget 10.", "Last boundary."]);
    let new = fixture_rows(&["First boundary.", "Budget 20.", "Last boundary."]);
    for (a, b) in [(&old, &new), (&new, &old)] {
        let result = compare(a, b);
        let intervals = &result.scopes[0].result.native_text_intervals;
        assert_eq!(intervals.len(), 1);
        assert!(intervals[0].proves_content_change());
        let encoded = serde_json::to_value(&intervals[0]).expect("descriptive report");
        let decoded: NativeTextIntervalComparison =
            serde_json::from_value(encoded.clone()).expect("deserialized report");
        assert_eq!(decoded.comparison(), intervals[0].comparison());
        assert!(!decoded.proves_content_change());
        let mut forged = encoded;
        forged["verified"] = serde_json::json!(true);
        let forged: NativeTextIntervalComparison =
            serde_json::from_value(forged).expect("unknown report fields are not authority");
        assert!(!forged.proves_content_change());
    }
    let repeated = fixture_rows(&["First boundary.", "repeat", "repeat", "Last boundary."]);
    let unchanged = compare(&repeated, &repeated);
    let intervals = &unchanged.scopes[0].result.native_text_intervals;
    assert!(!intervals.is_empty());
    assert!(intervals.iter().all(|interval| !interval.proves_content_change()));
}
