use pdfdelta_bench::{cases::built_in_cases, evaluator::evaluate_case, renderers::RendererKind};

#[test]
fn layout_only_mutations_preserve_the_content_result() {
    let cases = built_in_cases().expect("built-in cases are valid");
    for case in cases
        .iter()
        .filter(|case| case.name().contains("wrap") || case.name().contains("page-break"))
    {
        for renderer in RendererKind::all() {
            let result = evaluate_case(case, renderer).expect("layout case evaluates");
            assert!(
                result.passed,
                "{} / {}: {}",
                case.name(),
                renderer.name(),
                result.detail
            );
            assert_eq!(result.actual_changes, 0, "layout changed content");
            assert!(
                result.comparison_complete,
                "layout comparison remained incomplete"
            );
        }
    }
}

#[test]
fn content_mutation_remains_one_exact_event_under_both_renderers() {
    let case = built_in_cases()
        .expect("built-in cases are valid")
        .into_iter()
        .find(|case| case.name() == "text-replacement")
        .expect("replacement case exists");
    for renderer in RendererKind::all() {
        let result = evaluate_case(&case, renderer).expect("replacement case evaluates");
        assert!(result.passed, "{}: {}", renderer.name(), result.detail);
        assert_eq!(result.actual_changes, 1);
        assert_eq!(result.precision.matched_events, 1);
    }
}
