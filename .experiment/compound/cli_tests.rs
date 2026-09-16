
#[test]
#[cfg(target_os = "linux")]
fn selected_text_exit_code_counts_live_owned_interval_changes_without_relabeling_reviews() {
    let directory = TestDirectory::new();
    for (index, (left, right, expected)) in [
        ("Budget 10.", "Budget 20.", 1),
        ("Budget 20.", "Budget 10.", 1),
        ("Budget 10.", "Budget 10.", 0),
        ("a", "aa", 1),
    ].into_iter().enumerate() {
        let old = directory.join(&format!("old-{index}.pdf"));
        let new = directory.join(&format!("new-{index}.pdf"));
        let json = directory.join(&format!("report-{index}.json"));
        write_pdf(&old, &["First boundary.", left, "Last boundary."]);
        write_pdf(&new, &["First boundary.", right, "Last boundary."]);
        let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
            .arg(&old).arg(&new).args(["--channels", "text", "--quiet", "--json"])
            .arg(&json).output().expect("selected text comparison");
        let report: Value = serde_json::from_slice(&fs::read(json).expect("report")).expect("JSON");
        assert_eq!(report["comparison_complete"], true);
        assert_eq!(output.status.code(), Some(expected), "case {index}: {}", stderr(&output));
        assert_eq!(report["typed_changes"], 0, "interval proofs do not relabel A/B/C counts");
        assert_eq!(report["inferred_changes"], 0);
        if expected == 1 {
            assert!(report["scope_content_changes"].as_u64().expect("B count") > 0);
            assert!(report["comparison"]["scopes"].as_array().expect("scopes").iter().any(|scope| {
                scope["result"]["native_text_intervals"].as_array().expect("intervals").iter()
                    .any(|interval| interval["comparison"]["text_mask"]["claims"]["changed_source_lower"].as_u64().is_some_and(|count| count > 0))
            }));
        }
    }
}

#[test]
#[cfg(target_os = "linux")]
fn selected_text_compound_paint_preserves_gaps_but_never_certifies_inventory() {
    let directory = TestDirectory::new();
    for (index, (paint, obstructed)) in [
        ("1 w 10 M 10 100 m 290 100 l 10 280 m 290 280 l S", false),
        ("10 100 280 2 re 10 280 280 2 re f", false),
        ("10 100 280 2 re 10 280 280 2 re f*", false),
        ("10 100 m 30 110 270 110 290 100 c 10 280 m 30 282 270 282 290 280 c f", false),
        ("10 100 280 2 re 10 280 280 2 re 10 210 280 30 re f", true),
    ].into_iter().enumerate() {
        let old = directory.join(&format!("compound-old-{index}.pdf"));
        let new = directory.join(&format!("compound-new-{index}.pdf"));
        for (path, value) in [(&old, "10"), (&new, "20")] {
            write_pdf(path, &["temporary"]);
            let mut pdf = Document::load(path).expect("generated PDF");
            let page = pdf.get_pages()[&1];
            let program = format!("{paint} BT /F1 10 Tf 30 250 Td (First boundary.) Tj 0 -30 Td (Budget {value}.) Tj 0 -30 Td (Last boundary.) Tj ET");
            let content = pdf.add_object(Stream::new(dictionary! {}, program.into_bytes()));
            pdf.get_object_mut(page).expect("page").as_dict_mut().expect("page dictionary").set("Contents", content);
            pdf.save(path).expect("save controlled content");
        }
        for (direction, (a, b)) in [(&old, &new), (&new, &old)].into_iter().enumerate() {
            let json = directory.join(&format!("compound-{index}-{direction}.json"));
            let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
                .arg(a).arg(b).args(["--channels", "text", "--quiet", "--json"])
                .arg(&json).output().expect("selected text comparison");
            assert_eq!(output.status.code(), Some(3), "opaque content remains unexamined: {}", stderr(&output));
            let report: Value = serde_json::from_slice(&fs::read(json).expect("report")).expect("JSON");
            assert_eq!(report["comparison_complete"], false);
            let coverage = &report["coverage"][0];
            for side in ["old", "new"] {
                assert_eq!(coverage[format!("{side}_inventory_complete")], false);
                assert_eq!(coverage[format!("{side}_discovered_sources")], 39);
                assert_eq!(coverage[format!("{side}_compared_sources")], if obstructed {29} else {39});
                assert_eq!(coverage[format!("{side}_uncompared_sources")], if obstructed {10} else {0});
            }
            assert_eq!(report["scope_content_changes"], if obstructed {0} else {1});
        }
    }
}
