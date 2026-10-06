use roundhouse::expr::{Expr, ExprNode};
use roundhouse::ingest::ingest_expr;
use std::process::Command;

fn ingest(source: &str) -> Expr {
    roundhouse::ingest::sources::reset();
    roundhouse::ingest::sources::register("pattern.rb", source);
    let parsed = ruby_prism::parse(source.as_bytes());
    assert!(
        parsed.errors().next().is_none(),
        "invalid fixture: {source}"
    );
    ingest_expr(
        &parsed
            .node()
            .as_program_node()
            .unwrap()
            .statements()
            .as_node(),
        "pattern.rb",
    )
    .expect("ingest pattern")
}

fn ruby(source: &str, prelude: &str) -> String {
    let script = format!(
        "{prelude}\nbegin\nanswer = begin\n{source}\nend\np [:ok, answer]\nrescue => e\np [:error, e.class.name]\nend"
    );
    let output = Command::new("ruby")
        .args(["-e", &script])
        .output()
        .expect("ruby oracle");
    assert!(
        output.status.success(),
        "Ruby failed:\n{}\n{script}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn equivalent(source: &str, prelude: &str) {
    let ir = ingest(source);
    let emitted = roundhouse::emit::ruby::emit_expr(&ir);
    assert_eq!(ir, ingest(&emitted), "IR round-trip for {source}");
    assert_eq!(
        ruby(&emitted, prelude),
        ruby(source, prelude),
        "source:\n{source}\nemitted:\n{emitted}"
    );
}

#[test]
fn hashes_nested_values_and_absent_keys() {
    for subject in [
        "{code: :ok, data: {user: false}}",
        "{code: :ok, data: {user: nil}}",
        "{code: :ok, data: {}}",
        "{}",
        "nil",
        "false",
        "{code: :other}",
    ] {
        equivalent(
            &format!(
                "case {subject}; in {{code: :ok, data: {{user:}}}}; [1,user]; in {{code: :other}}; 2; else; 3; end"
            ),
            "",
        );
    }
    equivalent("case {a: nil}; in {a: nil}; true; else; false; end", "");
    equivalent("case {}; in {a: nil}; true; else; false; end", "");
}

#[test]
fn array_lengths_nested_patterns_and_rests() {
    for subject in ["[]", "[1]", "[1,2]", "[1,2,3]", "[1,2,3,4]", "nil", "{}"] {
        for pattern in [
            "[a,b]", "[a,*b]", "[*a,b]", "[a,*b,c]", "[a,*,b]", "[*,a]", "[]",
        ] {
            equivalent(
                &format!("case {subject}; in {pattern}; :yes; else; :no; end"),
                "",
            );
        }
    }
    equivalent("case [1,false,nil,4]; in [a,*b,c]; [a,b,c]; end", "");
    equivalent(
        "case [{data: [false,nil]}]; in [{data: [a,b]}]; [a,b]; end",
        "",
    );
    equivalent("case [1,2]; in [1]; 1; in [1,2]; 2; in [1,*]; 3; end", "");
}

#[test]
fn hash_rest_and_exclusion() {
    for subject in ["{}", "{a: 1}", "{a: 1, b: false}", "{a: nil, b: nil}"] {
        for pattern in ["{}", "{**}", "{**nil}", "{a:}", "{a:, **nil}"] {
            equivalent(
                &format!("case {subject}; in {pattern}; :yes; else; :no; end"),
                "",
            );
        }
        equivalent(
            &format!("h={subject}; r=case h; in {{a:, **rest}}; [a,rest]; else; nil; end; [r,h]"),
            "",
        );
    }
}

#[test]
fn values_use_case_equality_and_alternatives() {
    for subject in [
        "nil", "false", "true", "0", "1", "2", "4", "'hello'", "'no'", ":ok",
    ] {
        equivalent(
            &format!(
                "case {subject}; in nil | false; :empty; in (1...4); :range; in /^he/; :regexp; in Symbol; :symbol; else; :other; end"
            ),
            "",
        );
        equivalent(
            &format!("pin = (1..); case {subject}; in ^pin; :yes; else; :no; end"),
            "",
        );
        equivalent(
            &format!("case {subject}; in (..2); :yes; else; :no; end"),
            "",
        );
        equivalent(
            &format!("case {subject}; in Integer => number if number > 1; number; else; 0; end"),
            "",
        );
    }
    equivalent(
        "case [:asc,:before]; in [:asc,:after] | [:desc,:before]; 1; in [:asc,:before] | [:desc,:after]; 2; end",
        "",
    );
    equivalent("case [1]; in Array[Integer => n]; n; end", "");
    equivalent("case {a: 1}; in Hash[a: Integer => n]; n; end", "");
    equivalent("case 2; in ^(1 + 1); :yes; end", "");
}

#[test]
fn bindings_survive_failed_matches_and_guards() {
    for source in [
        "x=:old; case [1,2]; in [x,3]; :yes; else; :no; end; x",
        "case {a: 1}; in {a: x,b:}; :yes; else; :no; end; [x,b]",
        "case [false]; in [x] if false; :yes; else; :no; end; x",
        "case [1]; in [x] unless true; :yes; in [y]; :next; end; [x,y]",
        "x=9; case []; in [x]; :yes; else; :no; end; x",
        "case {a: 1,b: 2}; in {a: x, **nil}; :yes; else; :no; end; x",
        "case [1,2,3]; in [x,*rest,4]; :yes; else; :no; end; [x,rest]",
        "case false; in _; :yes; end; _",
    ] {
        equivalent(source, "");
    }
}

#[test]
fn failure_and_expression_values() {
    for source in [
        "case 1; in 2; 7; end",
        "case 1; in 2; 7; else; end",
        "case {}; in {a:}; end",
        "case {a: {}}; in {a: {b:}}; end",
        "case {}; in {a:}; in {b:}; end",
        "case {b: 1}; in {a: _} | {b: _} if false; end",
        "case {b: 2}; in {a: _} | {b: 1}; end",
        "case {a: 1}; in {a: 2} | {b: 1}; end",
        "case {}; in {a:}; else; nil; end",
        "[10, (case [false]; in [x]; x; end), 20]",
        "1 + (case 2; in 2; 3; else; 4; end)",
        "r = case 2; in 2; 7; else; 8; end; r",
    ] {
        equivalent(source, "");
    }
    equivalent(
        "begin; case {a: {}}; in {a: {b:}}; end; rescue NoMatchingPatternKeyError => e; [e.key,e.matchee]; end",
        "",
    );
}

#[test]
fn subject_guard_and_matching_evaluation_order() {
    let prelude = "TRACE=[]; def subject; TRACE << :subject; {a: false}; end; def guard(x); TRACE << x; false; end";
    equivalent(
        "r = case subject; in {b: x} if guard(:wrong); 1; in {a: x} if guard(x); 2; in {a: y}; y; end; [r,TRACE]",
        prelude,
    );
    equivalent(
        "r = case subject; in {a: x}; 1; in {a: y} if guard(:wrong); 2; end; [r,TRACE]",
        prelude,
    );
}

#[test]
fn custom_deconstruction_keys_and_return_validation() {
    let prelude = r#"
TRACE=[]
class Shape
  def deconstruct_keys(keys)
    TRACE << keys
    {a: false, b: nil}
  end
  def deconstruct
    [false,nil]
  end
end
class Invalid
  def deconstruct_keys(keys); []; end
  def deconstruct; {}; end
end
"#;
    for source in [
        "r=case Shape.new; in {a:}; a; end; [r,TRACE]",
        "r=case Shape.new; in {}; 1; else; 2; end; [r,TRACE]",
        "r=case Shape.new; in {**}; 1; end; [r,TRACE]",
        "r=case Shape.new; in {a:, **rest}; [a,rest]; end; [r,TRACE]",
        "case Shape.new; in [a,b]; [a,b]; end",
        "case Invalid.new; in {}; 1; else; 2; end",
        "case Invalid.new; in []; 1; else; 2; end",
        "r=case Shape.new; in {a:, **nil}; 1; else; 2; end; [r,TRACE]",
    ] {
        equivalent(source, prelude);
    }
}

#[test]
fn unsupported_find_patterns_are_located_in_strict_and_survey_modes() {
    let source = "case [1,2]; in [*,1,*]; true; end";
    let parsed = ruby_prism::parse(source.as_bytes());
    let error = ingest_expr(
        &parsed
            .node()
            .as_program_node()
            .unwrap()
            .statements()
            .as_node(),
        "pattern.rb",
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("find patterns") && error.contains("bytes 15..22"),
        "{error}"
    );
    roundhouse::ingest::survey::activate();
    let result = ingest_expr(
        &parsed
            .node()
            .as_program_node()
            .unwrap()
            .statements()
            .as_node(),
        "pattern.rb",
    );
    let gaps = roundhouse::ingest::survey::drain();
    assert!(result.is_ok());
    assert_eq!(gaps.len(), 1);
}

#[test]
fn shared_lowering_has_no_pattern_specific_emitter_nodes() {
    fn visit(e: &Expr) {
        assert!(!matches!(&*e.node, ExprNode::Case { .. }));
        e.node.for_each_child(&mut visit);
    }
    visit(&ingest("case {a: [1]}; in {a: [x]}; x; else; 0; end"));
}

#[test]
fn dynamic_custom_protocol_is_an_explicit_analysis_error() {
    let source = "class DynamicShape; def deconstruct_keys(keys); unknown_shape; end; end; class UseShape; def run; case DynamicShape.new; in {a:}; a; end; end; end";
    let mut app = roundhouse::App::new();
    app.library_classes =
        roundhouse::ingest::ingest_library_classes(source.as_bytes(), "dynamic.rb").unwrap();
    roundhouse::analyze::Analyzer::new(&app).analyze(&mut app);
    let diagnostics = roundhouse::analyze::diagnose(&app);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.message.contains("dynamic custom deconstruction")),
        "{diagnostics:?}"
    );
}

#[test]
fn unported_target_reports_pattern_scope_gap_before_emitting() {
    let source = "class Example; def run; case [1]; in [a]; a; end; end; end";
    let mut app = roundhouse::App::new();
    app.library_classes =
        roundhouse::ingest::ingest_library_classes(source.as_bytes(), "example.rb").unwrap();
    roundhouse::session::analyze_and_lower(&mut app);
    let (result, diagnostics) = roundhouse::emit::diagnostics::scope(|| {
        roundhouse::project::target_files(
            &app,
            std::path::Path::new("fixtures/tiny-blog"),
            roundhouse::project::BuildTarget::Typescript,
        )
    });
    assert!(result.unwrap_err().contains("case/in pattern matching"));
    assert!(
        diagnostics
            .iter()
            .any(|d| d.message.contains("pattern-local scope") && d.span.end > d.span.start)
    );
}

#[test]
fn pattern_temporaries_do_not_capture_source_locals() {
    equivalent(
        "case [7]; in [x]; __rh_pattern_0_1 = 9; [x,__rh_pattern_0_1]; end",
        "",
    );
}

#[test]
fn independent_match_operators_stay_out_of_scope() {
    for source in ["[1] => [a]", "[1] in [a]"] {
        let parsed = ruby_prism::parse(source.as_bytes());
        let node = parsed
            .node()
            .as_program_node()
            .unwrap()
            .statements()
            .as_node();
        assert!(ingest_expr(&node, "separate.rb").is_err());
    }
}

#[test]
fn ordinary_non_deconstructable_values_fall_through_without_type_errors() {
    let source = "class Example; def run; case nil; in {a:}; a; in [b]; b; else; 3; end; end; end";
    let mut app = roundhouse::App::new();
    app.library_classes =
        roundhouse::ingest::ingest_library_classes(source.as_bytes(), "example.rb").unwrap();
    roundhouse::analyze::Analyzer::new(&app).analyze(&mut app);
    let errors: Vec<_> = roundhouse::analyze::diagnose(&app)
        .into_iter()
        .filter(|d| d.severity == roundhouse::diagnostic::Severity::Error)
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn custom_collection_return_shape_is_explicitly_unsupported() {
    let source = "class Payload < Hash; end; class Shape; def deconstruct_keys(keys); Payload.new; end; end; class Example; def run; case Shape.new; in {a:}; a; end; end; end";
    let mut app = roundhouse::App::new();
    app.library_classes =
        roundhouse::ingest::ingest_library_classes(source.as_bytes(), "subclass.rb").unwrap();
    roundhouse::analyze::Analyzer::new(&app).analyze(&mut app);
    let diagnostics = roundhouse::analyze::diagnose(&app);
    assert!(diagnostics.iter().any(|d| d.message.contains("dynamic custom deconstruction")), "{diagnostics:?}");
}
