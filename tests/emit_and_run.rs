//! Constructs that `check` accepts must run once emitted.
//!
//! See `tests/support/emit_and_run.rs` for the harness and why it
//! exists. The ignored tests below are known places where the two
//! disagree: `check` is clean and the emitted program fails. Each is a
//! complete statement of the fix: make it pass and drop the `#[ignore]`.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn the_unedited_blog_runs() {
    emit_and_run::real_blog()
        .run_test("test/controllers/articles_controller_test.rb")
        .assert_passes();
}

#[test]
#[ignore = "check is clean but the emitted view raises NoMethodError: no runtime defines human_attribute_name (#147)"]
fn human_attribute_name_runs() {
    emit_and_run::real_blog()
        .edit(
            "app/views/articles/_form.html.erb",
            "<%= form.label :title %>",
            "<%= form.label :title %><%= Article.human_attribute_name(:title) %>",
        )
        .run_test("test/controllers/articles_controller_test.rb")
        .assert_passes();
}

#[test]
#[ignore = "check is clean but the emitted tree fails to load: no runtime FormBuilder, and the inlined form has no builder object (#148)"]
fn a_custom_form_builder_runs() {
    emit_and_run::real_blog()
        .write(
            "app/helpers/custom_form_builder.rb",
            "class CustomFormBuilder < ActionView::Helpers::FormBuilder\n  \
               def marker_field(name)\n    \
                 @template.content_tag(:span, name.to_s, class: \"builder-marker\")\n  \
               end\n\
             end\n",
        )
        .edit(
            "app/views/articles/_form.html.erb",
            "form_with(model: article, class: \"contents\")",
            "form_with(model: article, class: \"contents\", builder: CustomFormBuilder)",
        )
        .edit(
            "app/views/articles/_form.html.erb",
            "<%= form.label :title %>",
            "<%= form.label :title %><%= form.marker_field :title %>",
        )
        .run_test("test/controllers/articles_controller_test.rb")
        .assert_passes();
}

#[test]
fn pattern_matching_business_logic_runs() {
    emit_and_run::real_blog()
        .write("app/models/pattern_examples.rb", include_str!("fixtures/pattern_matching.rb"))
        .write("app/models/pattern_result.rb", include_str!("fixtures/pattern_result.rb"))
        .run_ruby(r#"
raise "range/regexp matching" unless PatternExamples.fee == "fixed"
raise "false binding" unless PatternExamples.result == false
raise "guard/rest binding" unless PatternExamples.guarded == 9
raise "custom deconstruction" unless PatternExamples.custom == false
raise "subject evaluated twice" unless PatternExamples.once == [false, 1]
begin
  PatternExamples.unmatched
  raise "unmatched case did not raise"
rescue NoMatchingPatternError
end
begin
  PatternExamples.exhaustive
  raise "missing key did not raise"
rescue NoMatchingPatternKeyError => e
  raise "wrong missing key" unless e.key == :present
end
"#)
        .assert_passes();
}
